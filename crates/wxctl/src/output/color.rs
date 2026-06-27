use std::io::IsTerminal;

/// Semantic color names — these don't change between themes
#[derive(Debug, Clone, Copy)]
pub enum Color {
    Red,
    Green,
    Yellow,
    Blue,
    Dim,
    BoldWhite,
    Reset,
}

/// Resolved color mode
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ColorMode {
    Plain,
    Dark,
    Light,
}

/// Theme holds the resolved color mode and provides mode-aware painting
#[derive(Debug, Clone)]
pub struct Theme {
    mode: ColorMode,
}

impl Default for Theme {
    fn default() -> Self {
        Self { mode: ColorMode::Dark }
    }
}

impl Theme {
    /// Create a theme with a specific mode
    pub fn new(mode: ColorMode) -> Self {
        Self { mode }
    }

    /// Resolve the theme from environment variables and config preference.
    ///
    /// Priority:
    /// 1. NO_COLOR env var (any value) -> Plain
    /// 2. WXCTL_COLOR env var -> auto|always|never|dark|light
    /// 3. config_preference from ~/.wxctl/config.json preferences.color_theme
    /// 4. Auto-detect (dark default)
    pub fn resolve(config_preference: Option<&str>) -> Self {
        // 1. NO_COLOR standard (https://no-color.org)
        if std::env::var_os("NO_COLOR").is_some() {
            return Self::new(ColorMode::Plain);
        }

        // 2. WXCTL_COLOR env var
        if let Ok(val) = std::env::var("WXCTL_COLOR") {
            return match val.to_lowercase().as_str() {
                "never" => Self::new(ColorMode::Plain),
                "always" | "dark" => Self::new(ColorMode::Dark),
                "light" => Self::new(ColorMode::Light),
                "auto" => Self::auto_detect(config_preference),
                _ => Self::auto_detect(config_preference),
            };
        }

        // 3 + 4. Auto-detect with config preference fallback
        Self::auto_detect(config_preference)
    }

    /// Auto-detect: plain if not a TTY, otherwise honor an explicit config
    /// preference, else probe the terminal background via OSC 11.
    ///
    /// The non-TTY gate runs *before* any probe, so piped/CI output resolves to
    /// `Plain` with zero ANSI and zero OSC bytes written.
    fn auto_detect(config_preference: Option<&str>) -> Self {
        if !std::io::stdout().is_terminal() {
            return Self::new(ColorMode::Plain);
        }

        match config_preference {
            Some("light") => Self::new(ColorMode::Light),
            Some("dark") => Self::new(ColorMode::Dark),
            _ => Self::new(Self::detect_terminal_background()),
        }
    }

    /// Detect dark/light from the terminal's actual background via OSC 11.
    ///
    /// Uses `terminal-colorsaurus` with a 250 ms timeout. The crate pairs the
    /// OSC 11 query with a DA1 probe so a non-supporting terminal bails fast
    /// instead of blocking the full timeout, consumes its own reply (no stray
    /// bytes on stdout), guards Screen/Windows, and sends nothing when
    /// `TERM=dumb`. Perceived lightness `< 0.5` → `Dark`, `≥ 0.5` → `Light`.
    /// Any failure (no response, timeout, unsupported terminal, parse error)
    /// falls back to `Dark` — safe because Phase 1 made primary/secondary text
    /// legible regardless of mode.
    fn detect_terminal_background() -> ColorMode {
        let mut opts = terminal_colorsaurus::QueryOptions::default();
        opts.timeout = std::time::Duration::from_millis(250);
        match terminal_colorsaurus::background_color(opts) {
            Ok(bg) if bg.perceived_lightness() < 0.5 => ColorMode::Dark,
            Ok(_) => ColorMode::Light,
            Err(_) => ColorMode::Dark,
        }
    }

    /// Returns true if in plain mode (no colors, no spinners)
    pub fn is_plain(&self) -> bool {
        self.mode == ColorMode::Plain
    }

    /// Get the ANSI code for a semantic color in the current mode.
    ///
    /// The neutral structural colors are theme-independent: `BoldWhite` is bold +
    /// the terminal's default foreground (`\x1b[1m`, no hardcoded fg) and `Dim` is a
    /// single mid-gray (`#7D8590`) that stays legible on both dark and light
    /// backgrounds. This makes primary/secondary text safe even when the dark/light
    /// mode guess is wrong.
    ///
    /// Accents (`Red`/`Green`/`Yellow`/`Blue`) remain per-mode:
    /// dark = GitHub Primer dark palette; light = GitHub Primer light palette.
    fn code(&self, color: Color) -> &'static str {
        match self.mode {
            ColorMode::Plain => "",
            ColorMode::Dark => match color {
                // #f85149 — bright red
                Color::Red => "\x1b[38;2;248;81;73m",
                // #3fb950 — bright green
                Color::Green => "\x1b[38;2;63;185;80m",
                // #d29922 — warm amber
                Color::Yellow => "\x1b[38;2;210;153;34m",
                // #78A9FF — IBM blue-40 (visible accent on dark; was #EDF5FF which renders white)
                Color::Blue => "\x1b[38;2;120;169;255m",
                // #7D8590 — theme-independent mid-gray (WCAG 5.6:1 on #000, 3.7:1 on #fff)
                Color::Dim => "\x1b[38;2;125;133;144m",
                // bold + terminal default fg (no hardcoded color — safe on any background)
                Color::BoldWhite => "\x1b[1m",
                Color::Reset => "\x1b[0m",
            },
            ColorMode::Light => match color {
                // GitHub Primer light palette
                // #d1242f — danger
                Color::Red => "\x1b[38;2;209;36;47m",
                // #1a7f37 — success
                Color::Green => "\x1b[38;2;26;127;55m",
                // #9a6700 — attention
                Color::Yellow => "\x1b[38;2;154;103;0m",
                // #0F62FE — IBM blue
                Color::Blue => "\x1b[38;2;15;98;254m",
                // #7D8590 — theme-independent mid-gray (WCAG 5.6:1 on #000, 3.7:1 on #fff)
                Color::Dim => "\x1b[38;2;125;133;144m",
                // bold + terminal default fg (no hardcoded color — safe on any background)
                Color::BoldWhite => "\x1b[1m",
                Color::Reset => "\x1b[0m",
            },
        }
    }

    fn reset_code(&self) -> &'static str {
        match self.mode {
            ColorMode::Plain => "",
            _ => "\x1b[0m",
        }
    }

    /// Wrap text with the appropriate ANSI codes for this theme
    pub fn paint(&self, color: Color, text: &str) -> String {
        let code = self.code(color);
        if code.is_empty() { text.to_string() } else { format!("{}{}{}", code, text, self.reset_code()) }
    }
}

/// Format duration in human-readable form
pub fn format_duration(ms: u64) -> String {
    if ms < 60000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        let seconds = ms / 1000;
        let minutes = seconds / 60;
        let remaining_seconds = seconds % 60;
        format!("{}m{}s", minutes, remaining_seconds)
    }
}

#[cfg(test)]
mod tests {
    use super::{Color, ColorMode, Theme};

    /// Exact `paint()` byte output across all modes, folded into one table:
    /// AC1 — BoldWhite is bold-only with no truecolor fg (both modes);
    /// AC2 — Dim is one theme-independent mid-gray (#7D8590) (both modes);
    /// AC3 — accents are byte-identical to the per-mode palette values;
    /// I2  — Plain mode emits zero ANSI for every color.
    #[test]
    fn palette_paint_codes_are_exact() {
        // AC1 + AC2: neutrals are theme-independent in both Dark and Light.
        for mode in [ColorMode::Dark, ColorMode::Light] {
            let t = Theme::new(mode);
            let bw = t.paint(Color::BoldWhite, "x");
            assert!(bw.starts_with("\x1b[1m"), "{mode:?}: BoldWhite should start with bold SGR, got {bw:?}");
            assert!(!bw.contains("38;2;"), "{mode:?}: BoldWhite must contain no truecolor fg sequence, got {bw:?}");
            assert_eq!(t.paint(Color::Dim, "x"), "\x1b[38;2;125;133;144mx\x1b[0m", "{mode:?}: Dim must emit #7D8590");
        }

        // AC3: per-mode accents are byte-exact (Red, Green, Yellow, Blue).
        let dark = Theme::new(ColorMode::Dark);
        for (color, expect) in [(Color::Red, "\x1b[38;2;248;81;73mx\x1b[0m"), (Color::Green, "\x1b[38;2;63;185;80mx\x1b[0m"), (Color::Yellow, "\x1b[38;2;210;153;34mx\x1b[0m"), (Color::Blue, "\x1b[38;2;120;169;255mx\x1b[0m")] {
            assert_eq!(dark.paint(color, "x"), expect, "dark {color:?}");
        }
        let light = Theme::new(ColorMode::Light);
        for (color, expect) in [(Color::Red, "\x1b[38;2;209;36;47mx\x1b[0m"), (Color::Green, "\x1b[38;2;26;127;55mx\x1b[0m"), (Color::Yellow, "\x1b[38;2;154;103;0mx\x1b[0m"), (Color::Blue, "\x1b[38;2;15;98;254mx\x1b[0m")] {
            assert_eq!(light.paint(color, "x"), expect, "light {color:?}");
        }

        // I2: Plain mode emits zero ANSI for every semantic color.
        let plain = Theme::new(ColorMode::Plain);
        for color in [Color::Red, Color::Green, Color::Yellow, Color::Blue, Color::Dim, Color::BoldWhite, Color::Reset] {
            assert_eq!(plain.paint(color, "x"), "x", "Plain mode must emit no ANSI for {color:?}");
        }
    }

    /// AC6 + AC7 (deterministic legs): explicit overrides resolve without any
    /// probe, in strict precedence. Runs every env-bracketed assertion in one
    /// test (single writer) to avoid the `set_var`/`remove_var` cross-thread
    /// race.
    /// The OSC 11 probe (true-auto) is not exercised here — it needs a real TTY
    /// and is covered by the [human] legs of AC5 in the E2E batch.
    #[test]
    fn explicit_overrides_resolve_without_probe() {
        // SAFETY: all env mutations are confined to this single serialized test;
        // prior values are captured and restored before returning.
        let prev_no_color = std::env::var_os("NO_COLOR");
        let prev_wxctl_color = std::env::var_os("WXCTL_COLOR");

        unsafe {
            std::env::remove_var("WXCTL_COLOR");

            // NO_COLOR wins over everything -> Plain.
            std::env::set_var("NO_COLOR", "1");
            assert_eq!(Theme::resolve(Some("dark")).mode, ColorMode::Plain, "NO_COLOR must force Plain");
            std::env::remove_var("NO_COLOR");

            // WXCTL_COLOR=never -> Plain (overrides config_pref).
            std::env::set_var("WXCTL_COLOR", "never");
            assert_eq!(Theme::resolve(Some("light")).mode, ColorMode::Plain, "never must force Plain");

            // WXCTL_COLOR=always|dark -> Dark.
            std::env::set_var("WXCTL_COLOR", "always");
            assert_eq!(Theme::resolve(Some("light")).mode, ColorMode::Dark, "always must force Dark");
            std::env::set_var("WXCTL_COLOR", "dark");
            assert_eq!(Theme::resolve(Some("light")).mode, ColorMode::Dark, "dark must force Dark");

            // WXCTL_COLOR=light -> Light.
            std::env::set_var("WXCTL_COLOR", "light");
            assert_eq!(Theme::resolve(Some("dark")).mode, ColorMode::Light, "light must force Light");

            // Restore.
            match &prev_wxctl_color {
                Some(v) => std::env::set_var("WXCTL_COLOR", v),
                None => std::env::remove_var("WXCTL_COLOR"),
            }
            if let Some(v) = &prev_no_color {
                std::env::set_var("NO_COLOR", v);
            }
        }
    }
}
