//! Layout: the single owner of width, `▌` section headers, hanging-indent
//! word-wrap, and the `Panel` render context. Width = `WXCTL_WIDTH` (clamped
//! 40–200) if set, else `clamp(detected, 60, 100)`, detection via
//! `console::Term::size`, fallback 80.

use crate::output::panel::glyphs::{self, GlyphSet};
use crate::output::panel::theme::{Color, Role, Theme, paint_role};

/// Render context threaded through panel formatters. Bundling theme + width +
/// glyphs makes every render function pure over its inputs — the key to
/// deterministic snapshots (callers build a fixed `Panel` in tests).
#[derive(Clone)]
pub struct Panel {
    pub theme: Theme,
    pub width: usize,
    pub glyphs: GlyphSet,
}

impl Panel {
    /// Build a panel from a resolved theme, the env/terminal width, and a glyph
    /// set. Use in `resources`/`explain` entry points.
    pub fn new(theme: Theme, width: usize, glyphs: GlyphSet) -> Self {
        Self { theme, width, glyphs }
    }

    /// Resolve the content width: `WXCTL_WIDTH` (clamped 40–200) if set, else
    /// `clamp(detected, 60, 100)` via `console::Term::size`, fallback 80.
    pub fn resolve_width() -> usize {
        if let Ok(raw) = std::env::var("WXCTL_WIDTH")
            && let Ok(n) = raw.trim().parse::<usize>()
        {
            return n.clamp(40, 200);
        }
        let detected = console::Term::stdout().size_checked().map(|(_, cols)| cols as usize).unwrap_or(80);
        detected.clamp(60, 100)
    }

    /// A `g!`-style glyph lookup for this panel's glyph set.
    pub fn g(&self, name: &str) -> &'static str {
        glyphs::glyph(self.glyphs, name)
    }

    /// A `▌ Title   (hint)` section header line. `hint` renders dim in parens.
    pub fn section(&self, title: &str, hint: Option<&str>) -> String {
        let bar = paint_role(&self.theme, Role::Active, self.g("bar"));
        let heading = paint_role(&self.theme, Role::Heading, title);
        match hint {
            Some(h) => format!("  {} {}   {}", bar, heading, paint_role(&self.theme, Role::Meta, &format!("({h})"))),
            None => format!("  {} {}", bar, heading),
        }
    }

    /// Word-wrap `text` to the panel width with a hanging indent: the first line
    /// starts at `indent` columns, continuation lines align under the first
    /// line's text (never column 0). Splits only on spaces — never mid-word —
    /// and a single over-long word is emitted whole rather than broken. Returns
    /// one `String` per visual line (no trailing newline). The caller paints.
    pub fn wrap_hanging(&self, text: &str, indent: usize) -> Vec<String> {
        let avail = self.width.saturating_sub(indent).max(1);
        let pad = " ".repeat(indent);
        let mut lines: Vec<String> = Vec::new();
        let mut cur = String::new();
        for word in text.split_whitespace() {
            if cur.is_empty() {
                cur.push_str(word);
            } else if cur.chars().count() + 1 + word.chars().count() <= avail {
                cur.push(' ');
                cur.push_str(word);
            } else {
                lines.push(format!("{pad}{cur}"));
                cur = word.to_string();
            }
        }
        if !cur.is_empty() {
            lines.push(format!("{pad}{cur}"));
        }
        if lines.is_empty() {
            lines.push(pad);
        }
        lines
    }

    /// Paint `text` for a role via this panel's theme.
    pub fn paint(&self, role: Role, text: &str) -> String {
        paint_role(&self.theme, role, text)
    }

    /// Paint `text` for a raw `Color` (escape hatch for verb/deployment palettes
    /// that don't map 1:1 to a `Role`).
    pub fn paint_color(&self, color: Color, text: &str) -> String {
        self.theme.paint(color, text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::panel::theme::ColorMode;

    fn plain_panel(width: usize) -> Panel {
        Panel::new(Theme::new(ColorMode::Plain), width, GlyphSet::Unicode)
    }

    #[test]
    fn width_env_clamps_to_40_200() {
        unsafe { std::env::set_var("WXCTL_WIDTH", "10") };
        assert_eq!(Panel::resolve_width(), 40);
        unsafe { std::env::set_var("WXCTL_WIDTH", "999") };
        assert_eq!(Panel::resolve_width(), 200);
        unsafe { std::env::set_var("WXCTL_WIDTH", "60") };
        assert_eq!(Panel::resolve_width(), 60);
        unsafe { std::env::remove_var("WXCTL_WIDTH") };
    }

    #[test]
    fn wrap_hanging_never_breaks_mid_word_and_indents_continuations() {
        let p = plain_panel(20);
        let lines = p.wrap_hanging("alpha beta gamma delta epsilon", 4);
        assert!(lines.len() > 1, "should wrap at width 20");
        // continuation lines start at column 4 (the indent), never column 0
        for line in &lines[1..] {
            assert!(line.starts_with("    "), "continuation indented: {line:?}");
        }
        // no line exceeds the width (no mid-word break inflates a line)
        for line in &lines {
            assert!(line.chars().count() <= 20, "line within width: {line:?}");
        }
    }

    #[test]
    fn overlong_word_is_emitted_whole() {
        let p = plain_panel(10);
        let lines = p.wrap_hanging("supercalifragilistic ok", 2);
        assert_eq!(lines[0], "  supercalifragilistic");
    }
}
