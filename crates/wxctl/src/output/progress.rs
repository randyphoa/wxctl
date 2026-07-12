//! Progress-rendering mode: how (and whether) the live panel is drawn.
//!
//! The human panel (stage/reconcile/execution rows + spinners) is *progress*,
//! so it draws to **stderr**, leaving stdout for machine output (`--output
//! json`). This module owns the one policy decision — animate, stream plain
//! lines, or stay silent — resolved once in `main` from the `--progress` flag
//! (or `WXCTL_PROGRESS`) and read by `OutputCollector::new`.
//!
//! Standards alignment: progress on stderr (Unix convention / CLIG.dev), TTY
//! detection with a plain fallback, and `--progress=auto|tty|plain|none`
//! modeled on Docker/BuildKit's `--progress`.

use clap::ValueEnum;
use std::io::IsTerminal;
use std::sync::OnceLock;

/// How the live progress panel is rendered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ProgressMode {
    /// Animate a live region when stderr is a TTY; stream plain lines otherwise
    /// (also plain under `CI` or `TERM=dumb`). Default.
    Auto,
    /// Force the animated live region even under `CI` / `TERM=dumb` (still needs
    /// a color-capable stderr; `NO_COLOR` / `WXCTL_COLOR=never` disable it).
    Tty,
    /// Stream plain append-only lines: no in-place repaint, so terminal
    /// scrollback stays usable during the run.
    Plain,
    /// Suppress the progress panel entirely; only errors and the final result
    /// (via `main`) reach the terminal.
    None,
}

/// Process-global resolved mode, set once by `main` before command dispatch.
/// Unset (tests, library callers) resolves to `Auto`.
static RESOLVED: OnceLock<ProgressMode> = OnceLock::new();

/// Record the resolved mode for the process. Called once from `main`; a second
/// call is a no-op (`OnceLock`), which is fine — the CLI parses `--progress` once.
pub fn set_progress_mode(mode: ProgressMode) {
    let _ = RESOLVED.set(mode);
}

/// The resolved progress mode, or `Auto` when `main` never set one.
pub fn progress_mode() -> ProgressMode {
    RESOLVED.get().copied().unwrap_or(ProgressMode::Auto)
}

impl ProgressMode {
    /// Resolve the effective mode from the CLI flag, then `WXCTL_PROGRESS`, then
    /// the `Auto` default. The flag wins over the env var.
    pub fn resolve(cli_flag: Option<ProgressMode>) -> ProgressMode {
        if let Some(mode) = cli_flag {
            return mode;
        }
        match std::env::var("WXCTL_PROGRESS").ok().as_deref().map(str::trim) {
            Some(v) if v.eq_ignore_ascii_case("auto") => ProgressMode::Auto,
            Some(v) if v.eq_ignore_ascii_case("tty") || v.eq_ignore_ascii_case("always") => ProgressMode::Tty,
            Some(v) if v.eq_ignore_ascii_case("plain") => ProgressMode::Plain,
            Some(v) if v.eq_ignore_ascii_case("none") || v.eq_ignore_ascii_case("quiet") || v.eq_ignore_ascii_case("off") => ProgressMode::None,
            _ => ProgressMode::Auto,
        }
    }

    /// Whether to drive the animated live region (indicatif `MultiProgress` on
    /// stderr). `color_plain` is the resolved theme's plain state: `NO_COLOR`,
    /// `WXCTL_COLOR=never`, or a non-TTY stderr all disable animation regardless
    /// of mode, since an in-place spinner needs a color-capable terminal.
    pub fn animates(self, color_plain: bool) -> bool {
        if color_plain {
            return false;
        }
        match self {
            ProgressMode::Tty => true,
            ProgressMode::Plain | ProgressMode::None => false,
            ProgressMode::Auto => std::io::stderr().is_terminal() && !ci_or_dumb(),
        }
    }

    /// Whether the panel is suppressed outright (no header, rows, or summary).
    pub fn is_quiet(self) -> bool {
        matches!(self, ProgressMode::None)
    }
}

/// True in a CI environment (`CI` set truthy) or a dumb terminal (`TERM=dumb`),
/// where an animated repaint is unwanted or unrenderable.
fn ci_or_dumb() -> bool {
    crate::config::env_bool("CI") || std::env::var("TERM").map(|t| t == "dumb").unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_flag_wins_over_env() {
        // Explicit flag is authoritative regardless of env.
        assert_eq!(ProgressMode::resolve(Some(ProgressMode::Plain)), ProgressMode::Plain);
        assert_eq!(ProgressMode::resolve(Some(ProgressMode::None)), ProgressMode::None);
    }

    #[test]
    fn env_parsing_covers_aliases() {
        // Serialized single-writer test: mutate WXCTL_PROGRESS under one thread.
        let prev = std::env::var_os("WXCTL_PROGRESS");
        // SAFETY: confined to this serialized test; prior value restored below.
        unsafe {
            for (val, want) in [("auto", ProgressMode::Auto), ("tty", ProgressMode::Tty), ("always", ProgressMode::Tty), ("plain", ProgressMode::Plain), ("none", ProgressMode::None), ("quiet", ProgressMode::None), ("off", ProgressMode::None), ("garbage", ProgressMode::Auto)] {
                std::env::set_var("WXCTL_PROGRESS", val);
                assert_eq!(ProgressMode::resolve(None), want, "WXCTL_PROGRESS={val}");
            }
            match &prev {
                Some(v) => std::env::set_var("WXCTL_PROGRESS", v),
                None => std::env::remove_var("WXCTL_PROGRESS"),
            }
        }
    }

    #[test]
    fn plain_mode_never_animates() {
        // Color-plain forces no animation in every mode.
        for mode in [ProgressMode::Auto, ProgressMode::Tty, ProgressMode::Plain, ProgressMode::None] {
            assert!(!mode.animates(true), "{mode:?} must not animate when color is plain");
        }
        // Plain / None never animate even with a color-capable stream.
        assert!(!ProgressMode::Plain.animates(false));
        assert!(!ProgressMode::None.animates(false));
    }

    #[test]
    fn quiet_only_for_none() {
        assert!(ProgressMode::None.is_quiet());
        for mode in [ProgressMode::Auto, ProgressMode::Tty, ProgressMode::Plain] {
            assert!(!mode.is_quiet(), "{mode:?} is not quiet");
        }
    }
}
