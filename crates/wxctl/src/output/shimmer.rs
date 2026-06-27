use std::fmt::Write;

/// Cosine-wave shimmer over `text`, driven by an explicit `elapsed_secs` — the
/// Animator owns the single frame clock. `base` is the dim base color,
/// `highlight` the bright sweep color. Returns an ANSI-escaped string.
pub fn shimmer_at(text: &str, elapsed_secs: f64, base: (u8, u8, u8), highlight: (u8, u8, u8)) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return String::new();
    }

    let padding = 10usize;
    let period = chars.len() + padding * 2;
    let pos = (elapsed_secs % 2.0) / 2.0 * period as f64;
    let band = 5.0f64;

    let mut out = String::with_capacity(chars.len() * 24);
    for (i, ch) in chars.iter().enumerate() {
        let dist = ((i as f64 + padding as f64) - pos).abs();
        let t = if dist <= band { 0.5 * (1.0 + (std::f64::consts::PI * dist / band).cos()) } else { 0.0 };
        let r = (highlight.0 as f64 * t + base.0 as f64 * (1.0 - t)) as u8;
        let g = (highlight.1 as f64 * t + base.1 as f64 * (1.0 - t)) as u8;
        let b = (highlight.2 as f64 * t + base.2 as f64 * (1.0 - t)) as u8;
        let _ = write!(out, "\x1b[38;2;{};{};{}m{}", r, g, b, ch);
    }
    out += "\x1b[0m";
    out
}

/// Brightness "breathing" pulse for a short marker glyph (e.g. a single `●`): a
/// smooth cosine lerp between `base` (dim) and `highlight` (bright) on a fixed
/// `period_secs`, driven by the Animator's single `elapsed_secs` clock. Where
/// [`shimmer_at`] sweeps a highlight *band positionally* across many characters,
/// this colors the whole (short) `text` one interpolated color per frame — the
/// right motion for a one-glyph status marker that must hold a fixed column.
/// Bright (`highlight`) at `elapsed_secs == 0`, dim (`base`) at half a period.
/// ANSI-escaped; emits nothing for empty input.
pub fn pulse_at(text: &str, elapsed_secs: f64, base: (u8, u8, u8), highlight: (u8, u8, u8), period_secs: f64) -> String {
    if text.is_empty() {
        return String::new();
    }
    let phase = (elapsed_secs / period_secs) * std::f64::consts::TAU;
    let t = 0.5 * (1.0 + phase.cos()); // 1.0 (highlight) at phase 0, breathing down to 0.0 (base)
    let lerp = |a: u8, b: u8| (b as f64 * t + a as f64 * (1.0 - t)) as u8;
    format!("\x1b[38;2;{};{};{}m{}\x1b[0m", lerp(base.0, highlight.0), lerp(base.1, highlight.1), lerp(base.2, highlight.2), text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulse_is_bright_at_zero_and_dim_at_half_period() {
        let dim = (125, 133, 144); // #7D8590
        let blue = (120, 169, 255); // #78A9FF
        let bright = pulse_at("\u{25cf}", 0.0, dim, blue, 1.2);
        let faint = pulse_at("\u{25cf}", 0.6, dim, blue, 1.2);
        assert!(bright.contains("120;169;255"), "phase 0 lerps to the highlight: {bright:?}");
        assert!(faint.contains("125;133;144"), "half a period lerps to the dim base: {faint:?}");
        assert!(pulse_at("", 0.0, dim, blue, 1.2).is_empty(), "empty input -> empty output");
    }
}
