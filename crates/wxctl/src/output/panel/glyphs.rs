//! Glyph tables: unicode (default) and ascii. Ascii removes the braille-spinner
//! font risk on Windows and gives a guaranteed-printable fallback. Selected by
//! terminal capability, overridable with `WXCTL_GLYPHS=ascii|unicode`.

/// Which glyph table to render with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlyphSet {
    Unicode,
    Ascii,
}

impl GlyphSet {
    /// Resolve from `WXCTL_GLYPHS` (`ascii`|`unicode`), else terminal capability.
    /// `unicode_capable` is the caller's VT/utf8 probe result (e.g. `vt_enabled()`).
    pub fn resolve(unicode_capable: bool) -> Self {
        match std::env::var("WXCTL_GLYPHS").ok().as_deref() {
            Some("ascii") => GlyphSet::Ascii,
            Some("unicode") => GlyphSet::Unicode,
            _ => {
                if unicode_capable {
                    GlyphSet::Unicode
                } else {
                    GlyphSet::Ascii
                }
            }
        }
    }
}

/// A glyph by semantic name. Unicode and ascii fallbacks are paired here so a
/// new glyph is a one-line addition in both arms.
pub fn glyph(set: GlyphSet, name: &str) -> &'static str {
    match (set, name) {
        // Section bar
        (GlyphSet::Unicode, "bar") => "\u{258c}", // ▌
        (GlyphSet::Ascii, "bar") => "|",
        // Status markers
        (GlyphSet::Unicode, "check") => "\u{2713}", // ✓
        (GlyphSet::Ascii, "check") => "ok",
        (GlyphSet::Unicode, "cross") => "\u{2717}", // ✗
        (GlyphSet::Ascii, "cross") => "x",
        (GlyphSet::Unicode, "bang") => "!",
        (GlyphSet::Ascii, "bang") => "!",
        (GlyphSet::Unicode, "query") => "?",
        (GlyphSet::Ascii, "query") => "?",
        // Rules / arrows / bullets
        (GlyphSet::Unicode, "rule") => "\u{2500}", // ─
        (GlyphSet::Ascii, "rule") => "=",
        (GlyphSet::Unicode, "arrow") => "\u{2500}\u{2500}\u{25b6}", // ──▶
        (GlyphSet::Ascii, "arrow") => "-->",
        (GlyphSet::Unicode, "bullet") => "\u{2022}", // •
        (GlyphSet::Ascii, "bullet") => "*",
        (GlyphSet::Unicode, "emdash") => "\u{2014}", // —
        (GlyphSet::Ascii, "emdash") => "-",
        (GlyphSet::Unicode, "dot") => "\u{00b7}", // ·
        (GlyphSet::Ascii, "dot") => "-",
        (GlyphSet::Unicode, "hourglass") => "\u{29d6}", // ⧖ test-start / waiting label
        (GlyphSet::Ascii, "hourglass") => "~",
        // DAG connectors
        (GlyphSet::Unicode, "tee") => "\u{251c}\u{2500}", // ├─
        (GlyphSet::Ascii, "tee") => "+-",
        (GlyphSet::Unicode, "ell") => "\u{2514}\u{2500}", // └─
        (GlyphSet::Ascii, "ell") => "`-",
        // Fallback: empty
        _ => "",
    }
}

/// Per-row progress-spinner glyphs. The Unicode marker is a single filled dot
/// (`●`) that the Animator color-pulses (blue↔dim) in place — one column wide, so
/// it aligns under the settled `✓`/`✗` and never renders a "blank" frame. Ascii
/// has no truecolor to pulse, so it animates by *shape* instead: the classic
/// `| / - \` ticker. Swapping the motif is this one function; `Effect::Spinner`
/// picks pulse vs. ticker by glyph set.
pub fn spinner_frames(set: GlyphSet) -> &'static [&'static str] {
    match set {
        GlyphSet::Unicode => &["\u{25cf}"],
        GlyphSet::Ascii => &["|", "/", "-", "\\"],
    }
}

/// Empty-track cell for determinate bars: a dim `░` shade (`-` in ascii) drawn in
/// the unfilled remainder so the bar holds a fixed width and the trailing counter
/// sits against it instead of floating in blank space.
pub fn bar_track(set: GlyphSet) -> &'static str {
    match set {
        GlyphSet::Unicode => "\u{2591}", // ░
        GlyphSet::Ascii => "-",
    }
}

/// Eighth-block fill characters for determinate bars (`▏▎▍▌▋▊▉█`); ascii uses `#`.
pub fn bar_fills(set: GlyphSet) -> &'static [&'static str] {
    match set {
        GlyphSet::Unicode => &["\u{258f}", "\u{258e}", "\u{258d}", "\u{258c}", "\u{258b}", "\u{258a}", "\u{2589}", "\u{2588}"],
        GlyphSet::Ascii => &["#"],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_table_is_pure_ascii() {
        for name in ["bar", "check", "cross", "bang", "query", "rule", "arrow", "bullet", "emdash", "dot", "tee", "ell", "hourglass"] {
            let g = glyph(GlyphSet::Ascii, name);
            assert!(g.is_ascii(), "ascii glyph {name:?} = {g:?} must be ascii");
        }
        for f in spinner_frames(GlyphSet::Ascii) {
            assert!(f.is_ascii(), "ascii spinner frame {f:?} must be ascii");
        }
        for f in bar_fills(GlyphSet::Ascii) {
            assert!(f.is_ascii(), "ascii bar fill {f:?} must be ascii");
        }
        assert!(bar_track(GlyphSet::Ascii).is_ascii(), "ascii bar track must be ascii");
    }

    #[test]
    fn explicit_env_override_wins_over_capability() {
        // Capability says unicode, but env forces ascii.
        // (Env is read live; this test sets/unsets it.)
        unsafe { std::env::set_var("WXCTL_GLYPHS", "ascii") };
        assert_eq!(GlyphSet::resolve(true), GlyphSet::Ascii);
        unsafe { std::env::set_var("WXCTL_GLYPHS", "unicode") };
        assert_eq!(GlyphSet::resolve(false), GlyphSet::Unicode);
        unsafe { std::env::remove_var("WXCTL_GLYPHS") };
    }
}
