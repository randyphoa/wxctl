use clap::Parser;
use tracing_subscriber::{EnvFilter, Layer, fmt, prelude::*};

mod cli;
mod commands;
mod config;
mod output;
mod update;

/// Whole-`main` trampoline (the uv `main2` / rustc `run_in_thread_with_globals` /
/// rust-analyzer `with_extra_thread` pattern). The entire program — clap `Command`
/// construction, the tokio runtime, and every command — runs on one explicitly
/// sized thread, so the OS main-thread stack limit never gates wxctl. Windows
/// reserves only 1 MiB for the main thread and unoptimized `wxctl` needs more than
/// that just to build clap's `Command` tower; on Unix `ulimit -s` can set it lower
/// still. `RUST_MIN_STACK` overrides the 8 MiB default (matching rustc);
/// `/STACK:8388608` in `.cargo/config.toml` is the Windows backstop for the rare
/// inline-fallback path.
fn main() {
    let stack_size = std::env::var("RUST_MIN_STACK").ok().and_then(|v| v.parse::<usize>().ok()).filter(|&n| n > 0).unwrap_or(8 * 1024 * 1024);

    // Build the tokio runtime *inside* the thread so `block_on` (and the clap tower
    // + first schema access that run before the first await) execute on the sized
    // stack, not the OS main thread.
    let run = || {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("build tokio runtime");
        rt.block_on(run_main());
    };

    match std::thread::Builder::new().name("wxctl-main".into()).stack_size(stack_size).spawn(run) {
        Ok(handle) => {
            // Normal completion returns `()`. `run_main`'s error/panic paths call
            // `std::process::exit` directly, so they never reach here; a panic that
            // escapes before the in-`run_main` hook is installed surfaces as a join
            // `Err` payload — re-raise it on the main thread so the process still aborts.
            if let Err(payload) = handle.join() {
                std::panic::resume_unwind(payload);
            }
        }
        Err(e) => {
            // Spawn failure (resource exhaustion) is rare: run inline on the OS main
            // thread. Degraded — Windows leans on the `/STACK` link-arg — but the
            // program still runs rather than aborting at startup.
            eprintln!("wxctl: could not spawn main thread ({e}); running inline");
            run();
        }
    }
}

async fn run_main() {
    // Global logging subscriber. Two layers run independently with their own filters:
    //   - OutputCollectorLayer: drives the CLI rendering. Always on for wxctl::* targets at trace
    //     so the user sees stage / substage / decision events regardless of RUST_LOG. The active
    //     per-command collector is installed by CommandContext::setup via install_collector().
    //   - fmt layer: structured logs for the operator. Honors RUST_LOG (off when unset). JSON to
    //     WXCTL_LOG_PATH when that env var is set, otherwise compact text to stderr.
    let collector_filter = EnvFilter::new(output::COLLECTOR_FILTER);
    let collector_layer = output::OutputCollectorLayer.with_filter(collector_filter);

    let fmt_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("off"));
    let fmt_layer: Box<dyn tracing_subscriber::Layer<_> + Send + Sync> = if let Ok(log_path) = std::env::var("WXCTL_LOG_PATH") {
        // WXCTL_LOG_APPEND=1/"true" appends instead of truncating, so a multi-command lifecycle
        // (plan → apply → test → destroy) streams into one file without a per-run truncate-and-cat dance.
        let append = crate::config::env_bool("WXCTL_LOG_APPEND");
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).write(true).truncate(!append).append(append);
        let file = opts.open(&log_path).expect("Failed to open WXCTL_LOG_PATH");
        Box::new(fmt::layer().json().with_file(true).with_line_number(true).with_writer(file).with_filter(fmt_filter))
    } else {
        Box::new(fmt::layer().with_writer(std::io::stderr).with_target(true).with_ansi(false).compact().with_filter(fmt_filter))
    };

    let run_record_filter = EnvFilter::new("wxctl=trace,wxctl_core=trace,wxctl_engine=trace,wxctl_providers=trace");
    let run_record_layer = output::RunRecordLayer.with_filter(run_record_filter);

    let registry = tracing_subscriber::registry().with(collector_layer).with(fmt_layer).with(run_record_layer);

    #[cfg(feature = "otel")]
    let registry = registry.with(wxctl_core::logging::otel::otel_layer());

    registry.init();

    // Panic hook: localize wxctl source bugs. Contract: capture a diagnosable
    // artifact, then abort. The process exit is deliberate — the DAG executor's
    // mpsc result channel cannot observe a panicked task (collect_results would
    // block forever on the channel), and the MCP server is in unknown state after
    // a tool-call panic (stdio clients restart servers automatically).
    //
    // Write order:
    //   1. Emit WXCTL-P001 into the active run record via RunRecordLayer.
    //      RunSink::write_event buffers through a BufWriter, so the event may still sit
    //      in the userspace buffer at this point — but step 2's finalize_active_run
    //      flushes that buffer before writing manifest.json, so the panic event is
    //      durable before exit skips destructors either way.
    //   2. finalize_active_run flushes the BufWriter, then writes manifest.json via
    //      fs::write (open+write+close) — synchronous to the OS before we reach exit.
    //   3. default_hook prints to stderr.
    //   4. exit(101) — Rust's conventional panic exit code; avoids the hang.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let bt = std::backtrace::Backtrace::force_capture();
        let msg = info.to_string();
        let loc = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        // Emit as a structured error event so RunRecordLayer captures it + indexes it.
        tracing::error!(
            target: "wxctl::error",
            stage = "panic",
            error_code = "WXCTL-P001",
            message = %msg,
            fix = "wxctl source bug — re-run with --full-trace and inspect the run record; the backtrace localizes the panic",
            src = %loc,
            backtrace = %bt,
            "panic: {msg}"
        );
        output::finalize_active_run("aborted");
        default_hook(info);
        std::process::exit(101);
    }));

    let cli = cli::Cli::parse();

    // Resolve the progress-rendering mode once (CLI flag > WXCTL_PROGRESS > auto)
    // before any collector is built. The live panel draws to stderr; this decides
    // whether it animates, streams plain lines, or is suppressed.
    output::progress::set_progress_mode(output::progress::ProgressMode::resolve(cli.progress));

    // Fail-silent background update check (isolated std::thread). Kill switches
    // are evaluated inside spawn_background_check before any network request.
    // `mcp serve` (long-lived stdio server) and `update` (runs its own explicit
    // /check) both suppress the background update check, so a self-update run does
    // not also print the post-command notice.
    let suppress_bg_check = matches!(cli.command, cli::Commands::Mcp { .. } | cli::Commands::Update { .. });
    let update_check = update::check::spawn_background_check(env!("CARGO_PKG_VERSION"), update::check::GateInputs::from_env(cli.no_update_check, suppress_bg_check));

    let profile = config::resolve_active_profile(cli.profile.as_deref());

    let result = match cli.command {
        cli::Commands::Apply { config, output } => commands::apply::execute(&config, &profile, cli.profile_path.as_deref(), cli.full_trace, output.as_ref()).await,
        cli::Commands::Plan { config, output } => commands::plan::execute(&config, &profile, cli.profile_path.as_deref(), cli.full_trace, output.as_ref()).await,
        cli::Commands::Destroy { config, output } => commands::destroy::execute(&config, &profile, cli.profile_path.as_deref(), cli.full_trace, output.as_ref()).await,
        cli::Commands::Validate { config, fix_prompt, output, skip_post_validate, deployment } => commands::validate::execute(&config, fix_prompt.as_deref(), output.as_ref(), skip_post_validate, deployment).await,
        cli::Commands::Compose { command } => match command {
            cli::ComposeCommands::Identify { input, output } => commands::compose::identify::execute(&input, output.as_deref()),
            cli::ComposeCommands::Paths { config, deployment, output } => commands::compose::paths::execute(&config, deployment.as_deployment_str(), output.as_deref()),
            cli::ComposeCommands::Prompt { input, paths, resources_dir, scaffold_dir, config, test_config, output } => {
                commands::compose::prompt::execute(input.as_deref(), paths.as_deref(), resources_dir.as_deref(), scaffold_dir.as_deref(), config.as_deref(), test_config.as_deref(), output.as_deref())
            }
            cli::ComposeCommands::Scaffold { config, output_dir, apply_implementations, dry_run } => commands::compose::scaffold::execute(&config, output_dir.as_deref(), apply_implementations.as_deref(), dry_run),
        },
        cli::Commands::Init { config, force, edit } => commands::init::execute(&config, &profile, cli.profile_path.as_deref(), force, edit).await,
        cli::Commands::Test { config, output } => commands::test::execute(&config, &profile, cli.profile_path.as_deref(), output.as_ref()).await,
        cli::Commands::Profile { command } => commands::profile::execute(command, cli.profile.as_deref(), cli.profile_path.as_deref()).await,
        cli::Commands::Resources { service, deployment, output } => commands::resources::execute(service.as_deref(), deployment.as_deref(), output),
        cli::Commands::Explain { kind, output } => commands::explain::execute(kind.as_deref(), output),
        cli::Commands::Update { yes, notes, force } => commands::update::execute(yes, notes, force),
        cli::Commands::Mcp { command } => match command {
            cli::McpCommands::Serve { read_only } => wxctl_mcp::serve(&profile, cli.profile_path.as_deref(), read_only, cli.full_trace).await,
        },
        cli::Commands::Runs { command } => match command {
            cli::RunsCommands::List => commands::runs::list(),
            cli::RunsCommands::Show { run_id, full } => commands::runs::show(&run_id, full),
        },
        cli::Commands::Debug { run_id, output } => commands::debug::execute(run_id.as_deref(), output.as_ref()),
    };

    if let Err(e) = result {
        // Suppress the redundant top-level anyhow dump when the collector already
        // rendered a styled error block — the failure is shown once, in the panel.
        // Still emit the structured WXCTL-E000 run-record event and exit non-zero.
        if !output::styled_error_rendered() {
            // Render early / codeless failures (config-load, missing profile) through
            // the same `▌ Errors` panel idiom as in-pipeline errors, so both surfaces
            // are identical. Panel built like OutputCollector::panel (honors NO_COLOR /
            // WXCTL_COLOR / non-TTY → no ANSI). Falls back to the bare line only if
            // rendering yields nothing. All output goes to stderr, not stdout.
            let panel = output::panel::layout::Panel::resolve(None);
            let lines = output::panel_render::render_top_level_error(&panel, &format!("{:#}", e));
            if lines.is_empty() {
                eprintln!("\n{}", panel.theme.paint(output::color::Color::Red, &format!("Error: {:#}", e)));
            } else {
                eprintln!();
                for line in lines {
                    eprintln!("{line}");
                }
            }
        }
        let chain = wxctl_core::error_chain_vec(&e);
        tracing::error!(target: "wxctl::error", stage = "command", error_code = "WXCTL-E000", message = %e, fix = "see error chain", error_chain = %serde_json::to_string(&chain).unwrap_or_default(), "Command failed");
        output::finalize_active_run("failed");
        #[cfg(feature = "otel")]
        wxctl_core::logging::otel::shutdown();
        std::process::exit(1);
    }

    // Bounded-join the background check (~3s cap) and render any notice to
    // stderr — after the command output, after the collector guard has dropped.
    // Success path only: the error path above already exited(1) with a styled
    // error; AC 1 requires the notice only on a normal (exit-0) run, and a notice
    // appended to a failure would clutter it. (Documented deliberate choice.)
    if let Some(notice) = update_check.join_timeout() {
        // Notice prints to stderr → gate its color on stderr's TTY.
        let theme = output::color::Theme::resolve_for_stderr(None);
        for line in output::notice::render_notice(&theme, &notice, update::installed_via_npm()) {
            eprintln!("{line}");
        }
        // Persist newly-shown `info` ids so they dedup next fetch. `security`
        // items are intentionally NOT persisted — they re-show until `current`
        // satisfies fixed_in/max_version (handled in dedup_news).
        if let Some(path) = update::cache::news_seen_path() {
            let mut seen = update::cache::load_seen(&path);
            for item in &notice.news {
                if item.severity == update::Severity::Info && !seen.shown_ids.contains(&item.id) {
                    seen.shown_ids.push(item.id.clone());
                }
            }
            let _ = update::cache::save_seen(&path, &seen);
        }
    }

    #[cfg(feature = "otel")]
    wxctl_core::logging::otel::shutdown();
}
