use clap::Parser;
use tracing_subscriber::{EnvFilter, Layer, fmt, prelude::*};

mod cli;
mod commands;
mod config;
mod output;

#[tokio::main]
async fn main() {
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
    //      RunSink::write_event uses writeln!(File, …) — no userspace buffer,
    //      write(2) is called directly — so the event reaches the OS page cache
    //      before exit skips destructors.
    //   2. finalize_active_run flushes manifest.json via fs::write (open+write+close)
    //      — also synchronous to the OS before we reach exit.
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

    let profile = config::resolve_active_profile(cli.profile.as_deref());

    let result = match cli.command {
        cli::Commands::Apply { config } => commands::apply::execute(&config, &profile, cli.profile_path.as_deref(), cli.full_trace).await,
        cli::Commands::Plan { config } => commands::plan::execute(&config, &profile, cli.profile_path.as_deref(), cli.full_trace).await,
        cli::Commands::Destroy { config } => commands::destroy::execute(&config, &profile, cli.profile_path.as_deref(), cli.full_trace).await,
        cli::Commands::Validate { config, fix_prompt, output, skip_post_validate } => commands::validate::execute(&config, fix_prompt.as_deref(), output.as_ref(), skip_post_validate).await,
        cli::Commands::Compose { command } => match command {
            cli::ComposeCommands::Identify { input, output } => commands::compose::identify::execute(&input, output.as_deref()),
            cli::ComposeCommands::Paths { config, deployment, output } => commands::compose::paths::execute(&config, deployment.as_deployment_str(), output.as_deref()),
            cli::ComposeCommands::Prompt { input, paths, resources_dir, scaffold_dir, config, test_config, output } => {
                commands::compose::prompt::execute(input.as_deref(), paths.as_deref(), resources_dir.as_deref(), scaffold_dir.as_deref(), config.as_deref(), test_config.as_deref(), output.as_deref())
            }
            cli::ComposeCommands::Scaffold { config, output_dir, apply_implementations, dry_run } => commands::compose::scaffold::execute(&config, output_dir.as_deref(), apply_implementations.as_deref(), dry_run),
        },
        cli::Commands::Init { config, template } => commands::init::execute(&config, &profile, cli.profile_path.as_deref(), template),
        cli::Commands::Test { config } => commands::test::execute(&config, &profile, cli.profile_path.as_deref()).await,
        cli::Commands::Profile { command } => commands::profile::execute(command, cli.profile.as_deref(), cli.profile_path.as_deref()).await,
        cli::Commands::Resources { service, deployment, output } => commands::resources::execute(service.as_deref(), deployment.as_deref(), output),
        cli::Commands::Explain { kind, output } => commands::explain::execute(&kind, output),
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
            println!("\n\x1b[31mError: {:#}\x1b[0m", e);
        }
        let chain = wxctl_core::error_chain_vec(&e);
        tracing::error!(target: "wxctl::error", stage = "command", error_code = "WXCTL-E000", message = %e, fix = "see error chain", error_chain = %serde_json::to_string(&chain).unwrap_or_default(), "Command failed");
        output::finalize_active_run("failed");
        #[cfg(feature = "otel")]
        wxctl_core::logging::otel::shutdown();
        std::process::exit(1);
    }

    #[cfg(feature = "otel")]
    wxctl_core::logging::otel::shutdown();
}
