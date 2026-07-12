//! `wxctl update` — explicit self-update. Sync, no profile, does its own
//! blocking network (models `resources`/`explain`). Reuses the Worker `/check`
//! for `latest` + `changelog`, then downloads/verifies/installs the host release
//! archive via the `update::download` install seam. Honors `WXCTL_DISABLE_UPDATES`,
//! `--yes`/`--notes`, the non-TTY confirmation refusal, and `NotWritable` /
//! unsupported-platform guidance. Outbound hosts: the Worker `/check` endpoint
//! and the release base URL only — never `api.github.com` (AC 9).

use std::io::{IsTerminal, Write};

use anyhow::{Result, anyhow};

use crate::output::color::Theme;
use crate::output::mark_styled_error_rendered;
use crate::output::panel::theme::{Role, paint_role};
use crate::update::ChangelogEntry;
use crate::update::download::{self, RELEASES_PAGE, UpdateError};
use crate::update::registry;

/// Print friendly guidance to stderr, suppress `main`'s generic "Error:"
/// re-print, and return a non-zero error so the process exits 1.
fn refuse(msg: impl std::fmt::Display) -> anyhow::Error {
    eprintln!("{msg}");
    mark_styled_error_rendered();
    anyhow!("{msg}")
}

/// Run `wxctl update`. `force` reinstalls the current version even when already
/// up to date (re-download + verify + self-replace), for a corrupted binary or a
/// clean re-lay-down.
pub fn execute(yes: bool, notes: bool, force: bool) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");

    // (1) Kill switch — refuse before any network.
    if crate::config::env_bool("WXCTL_DISABLE_UPDATES") {
        return Err(refuse("wxctl self-update is disabled (WXCTL_DISABLE_UPDATES is set)."));
    }

    // (1b) npm-managed install: never self-replace a binary inside node_modules
    // (clobbered on the next `npm ci`, integrity mismatch). Redirect to npm
    // before any network. `--notes` is read-only, so it still prints the
    // changelog below.
    if !notes && crate::update::installed_via_npm() {
        return Err(refuse("wxctl was installed via npm; upgrade with `npm update -g wxctl`"));
    }

    // (2) Fresh /check (ignores the 24h interval; never writes the stamp).
    let check = registry::fetch_check()?;
    let theme = Theme::resolve(None);

    // (3) --notes → print the changelog and stop (no download/install).
    if notes {
        print_changelog(&theme, &check.changelog);
        return Ok(());
    }

    // (4) Pick the version to install. Normally only a strictly-newer release
    // qualifies; `--force` falls back to reinstalling the current version when
    // already up to date. A genuinely newer release still wins under `--force`.
    let (version, is_reinstall) = match newer_than(current, check.latest.as_deref()) {
        Some(latest) => (latest, false),
        None if force => (current.to_string(), true),
        None => {
            println!("wxctl {current} is already up to date.");
            return Ok(());
        }
    };

    // Unsupported OS/arch — reject before any download.
    let Some(target) = download::target_triple() else {
        let err = UpdateError::UnsupportedPlatform { os: std::env::consts::OS, arch: std::env::consts::ARCH };
        return Err(refuse(format!("{err}. Download manually: {RELEASES_PAGE}")));
    };

    // (5) Confirm. Non-TTY without --yes refuses (no surprise binary swaps in
    // scripts); an interactive "no" is a clean cancel (exit 0).
    if !yes {
        if !std::io::stdin().is_terminal() {
            return Err(refuse("refusing to self-update without confirmation. Re-run with `--yes` to update non-interactively."));
        }
        if !prompt_confirm(current, &version, is_reinstall)? {
            println!("update cancelled.");
            return Ok(());
        }
    }

    // (6) Pre-flight install-target writability → NotWritable guidance.
    let install_path = download::install_target().map_err(refuse)?;
    if let Err(UpdateError::NotWritable(dir)) = download::preflight_writable(&install_path) {
        return Err(refuse(not_writable_guidance(&dir)));
    }

    // (7) Download → verify → extract → install (verification precedes any write).
    let archive = download::download_and_verify(&version, target).map_err(refuse)?;
    let binary = download::extract_binary(&archive).map_err(refuse)?;
    download::install(&binary).map_err(refuse)?;

    // (8) Success + "What's new" (newest gainable release's notes). A reinstall
    // gains no version, so the changelog is empty — just confirm the reinstall.
    if is_reinstall {
        println!("reinstalled wxctl {current}");
        return Ok(());
    }
    println!("updated {current} \u{2192} {version}");
    if let Some(entry) = check.changelog.first() {
        println!();
        println!("{}", paint_role(&theme, Role::Heading, &format!("What's new in v{}", entry.version)));
        for note in &entry.notes {
            println!("{}", paint_role(&theme, Role::Meta, &format!("  \u{2022} {note}")));
        }
    }
    Ok(())
}

/// `Some(latest)` when `latest` parses as a strictly-greater semver than
/// `current`; `None` when up to date, missing, or unparseable. Both sides may be
/// `v`-prefixed.
fn newer_than(current: &str, latest: Option<&str>) -> Option<String> {
    let latest = latest?;
    let cur = semver::Version::parse(current.trim_start_matches('v')).ok()?;
    let new = semver::Version::parse(latest.trim_start_matches('v')).ok()?;
    (new > cur).then(|| latest.to_string())
}

/// Interactive y/N prompt on stderr; `true` only for `y`/`yes`. A reinstall
/// (`--force` while up to date) prompts without the misleading `X → X` arrow.
fn prompt_confirm(current: &str, version: &str, is_reinstall: bool) -> Result<bool> {
    if is_reinstall {
        eprint!("Reinstall wxctl {current}? [y/N] ");
    } else {
        eprint!("Update wxctl {current} \u{2192} {version}? [y/N] ");
    }
    std::io::stderr().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}

/// OS-appropriate guidance when the install directory is not writable.
fn not_writable_guidance(dir: &std::path::Path) -> String {
    #[cfg(unix)]
    {
        format!("cannot write to {} — re-run with elevated permissions (e.g. `sudo wxctl update`) or re-download from {RELEASES_PAGE}", dir.display())
    }
    #[cfg(windows)]
    {
        format!("cannot write to {} — re-run from an elevated (Administrator) prompt or re-download from {RELEASES_PAGE}", dir.display())
    }
}

/// Print every changelog entry's notes (`--notes`), or a placeholder if empty.
fn print_changelog(theme: &Theme, changelog: &[ChangelogEntry]) {
    if changelog.is_empty() {
        println!("No release notes available.");
        return;
    }
    for entry in changelog {
        println!("{}", paint_role(theme, Role::Heading, &format!("v{}", entry.version)));
        for note in &entry.notes {
            println!("{}", paint_role(theme, Role::Meta, &format!("  \u{2022} {note}")));
        }
    }
}
