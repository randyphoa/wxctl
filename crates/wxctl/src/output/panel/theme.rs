//! Panel theme layer: re-exports the resolved `Theme`/`Color` from `color.rs`,
//! adds Windows VT init (truecolor + ANSI on ConPTY-era consoles), and the
//! semantic role → `Color` mapping the panel renders against. If VT cannot be
//! enabled (legacy conhost), callers should resolve to Plain so raw truecolor
//! codes never reach an incapable terminal.

use std::sync::OnceLock;

pub use crate::output::color::{Color, Theme};
// `ColorMode` is referenced only by the panel/snapshot unit tests (fixed-mode
// panels); the non-test build never names it directly.
#[allow(unused_imports)]
pub use crate::output::color::ColorMode;

/// Semantic roles the panel paints by meaning, not by raw color name. Maps to a
/// `Color` so the existing dark/light/plain palettes resolve them per theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Success / completed (green).
    Success,
    /// Active / in-flight / info / section bar (blue).
    Active,
    /// Caution / unchecked (amber).
    Caution,
    /// Danger / failed (red).
    Danger,
    /// Secondary text, rules, meta (dim).
    Meta,
    /// Emphasised heading text.
    Heading,
}

impl Role {
    /// The `Color` this role paints as in the current palette.
    pub fn color(self) -> Color {
        match self {
            Role::Success => Color::Green,
            Role::Active => Color::Blue,
            Role::Caution => Color::Yellow,
            Role::Danger => Color::Red,
            Role::Meta => Color::Dim,
            Role::Heading => Color::BoldWhite,
        }
    }
}

/// Paint `text` for a semantic `role` using the resolved theme.
pub fn paint_role(theme: &Theme, role: Role, text: &str) -> String {
    theme.paint(role.color(), text)
}

/// Enable terminal VT processing exactly once (no-op on Unix; enables Virtual
/// Terminal Processing on Windows ConPTY). Returns `true` when truecolor ANSI is
/// usable. On legacy conhost where VT can't be enabled, returns `false` and the
/// caller resolves the theme to Plain.
pub fn vt_enabled() -> bool {
    static VT: OnceLock<bool> = OnceLock::new();
    *VT.get_or_init(|| {
        // `console`'s feature probe enables VT on Windows as a side effect and
        // reports whether the terminal supports ANSI/colors. On Unix this is
        // always true for a TTY.
        let term = console::Term::stdout();
        term.features().colors_supported()
    })
}
