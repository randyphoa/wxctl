//! Animator: one capped-rate ticker thread (~10 fps) driving every live-region
//! effect. Effects are pure frame generators over elapsed time; the Animator
//! owns the single clock and the indicatif `ProgressBar` handles. In plain /
//! non-TTY mode the Animator is inert (no thread spawned, no ANSI).

use crate::output::color::format_duration;
use crate::output::panel::glyphs::{self, GlyphSet};
use crate::output::panel::layout::Panel;
use crate::output::panel::theme::{Color, Role, Theme};
use crate::output::panel_render::{ExecCells, ExecWidths, exec_done_row, exec_header_line, exec_row_line};
use crate::output::sections::{ExecMarker, ExecRow};
use crate::output::shimmer;
use indicatif::ProgressBar;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Per-row spinner pulse: a single `●` breathes in the accent blue — the same
/// `Color::Blue` the `▌` section bar uses (`#78A9FF` in dark mode) — between a
/// dimmed blue of the same hue (`PULSE_LOW`) and the full accent (`PULSE_HIGH`),
/// so it never grays out, on a brisk ~0.7s cosine. Dark-tuned, like `shimmer_at`.
const PULSE_LOW: (u8, u8, u8) = (72, 101, 153); // #486599 — accent blue, dimmed
const PULSE_HIGH: (u8, u8, u8) = (120, 169, 255); // #78A9FF — Color::Blue accent (the ▌ bar)
const PULSE_PERIOD_SECS: f64 = 0.7;

/// Cell count for the live `▌ Execution` header summary bar (`██████░░░░`).
const EXEC_SUMMARY_CELLS: usize = 10;

/// The live per-item progress marker: a single `●` color-pulsing in the accent blue
/// (Unicode), or a `| / - \` shape ticker (ascii — no truecolor to pulse). Shared by
/// the bare per-resource Spinner and the in-progress `▌ Execution` row so they move alike.
fn live_marker(theme: &Theme, set: GlyphSet, elapsed: f64) -> String {
    let frames = glyphs::spinner_frames(set);
    match set {
        GlyphSet::Unicode => shimmer::pulse_at(frames[0], elapsed, PULSE_LOW, PULSE_HIGH, PULSE_PERIOD_SECS),
        GlyphSet::Ascii => {
            let i = ((elapsed * 10.0) as usize) % frames.len();
            theme.paint(Role::Active.color(), frames[i])
        }
    }
}

/// The seven effects from the spec's effect table. Each is a pure frame
/// generator: `frame(elapsed) -> painted String`.
#[derive(Clone)]
#[allow(dead_code)]
pub enum Effect {
    /// Per-row progress spinner: a single `●` breathing blue↔dim (Unicode) or a
    /// `| / - \` ticker (ascii), followed by a static row label.
    Spinner { label: String },
    /// Active-section `▌` bar pulsing blue↔dim.
    Pulse,
    /// Animated ellipsis (`reconciliation` + 0..=3 dots).
    Ellipsis { label: String },
    /// Live counter + elapsed (`7/10 checked · 4.2s`).
    Counter { done: usize, total: usize, noun: String, started: Instant },
    /// Determinate eighth-block bar at `fraction` in [0,1].
    Bar { fraction: f64, cells: usize },
    /// Completion settle: bright check for ~300ms after `done_at`, then normal.
    Settle { label: String, done_at: Instant },
    /// Cosine shimmer over `label` (migrated from `shimmer.rs`).
    Shimmer { label: String },
    /// Determinate bar + `n/m done · elapsed` counter on one stage line. `done`
    /// is shared so the collector bumps it as resources complete; `total` and
    /// `cells` are fixed at registration. `label`, when present, is a shared
    /// current-item string (`<kind> <name>`) the collector updates each step;
    /// the ticker renders it between the noun and the elapsed.
    CounterBar { done: Arc<AtomicUsize>, total: usize, noun: String, cells: usize, started: Instant, label: Option<Arc<Mutex<String>>> },
    /// Live `▌ Execution` table header: the section bar + a determinate summary
    /// (`<bar> <done>/<total> · <elapsed>`) on line 1, the dim column header on line 2.
    /// `done` is the shared completion counter the collector bumps; `widths` is the
    /// fixed grid every row shares (so the header lines up with the streaming rows).
    ExecSummary { done: Arc<AtomicUsize>, total: usize, started: Instant, panel: Panel, widths: ExecWidths },
    /// Prefilled, not-yet-started `▌ Execution` row: `├─ · <kind> <name> … pending` (dim),
    /// shown from the first frame so the full scope is visible. Flipped to `ExecRowLive` when
    /// the resource starts. `last` picks the `└─` vs `├─` connector (known up front).
    ExecRowPending { panel: Panel, widths: ExecWidths, kind: String, name: String, last: bool },
    /// Live in-progress `▌ Execution` row: `├─ ● <kind> <name> … <ticking elapsed>`,
    /// the `●` pulsing in the accent blue. Swapped to `ExecRowDone` on completion.
    ExecRowLive { panel: Panel, widths: ExecWidths, kind: String, name: String, started: Instant, last: bool },
    /// Settled `▌ Execution` row, byte-identical to the final static row: `<connector>
    /// <marker> <kind> <name> [id=…] <action> <duration>`.
    ExecRowDone { panel: Panel, widths: ExecWidths, marker: ExecMarker, kind: String, name: String, id: Option<String>, duration_ms: u64, changed_fields: Vec<String>, last: bool },
}

impl Effect {
    /// Whether this effect's frame is independent of elapsed time (and of any shared
    /// counter or clock) — so it can be rendered once and cached rather than rebuilt
    /// on every ticker frame. Only the settled/prefilled `▌ Execution` rows qualify;
    /// everything else pulses, ticks, or tracks a live counter.
    fn is_static(&self) -> bool {
        matches!(self, Effect::ExecRowPending { .. } | Effect::ExecRowDone { .. })
    }

    /// Render this effect's frame at `elapsed` seconds since Animator start.
    pub fn frame(&self, elapsed: f64, theme: &Theme, set: GlyphSet) -> String {
        match self {
            Effect::Spinner { label } => {
                // One `●` breathing in the accent blue (Unicode), a single column so it sits
                // under the settled `✓`/`✗`; `| / - \` ticker in ascii. See `live_marker`.
                format!("{} {}", live_marker(theme, set, elapsed), label)
            }
            Effect::Pulse => {
                let bar = glyphs::glyph(set, "bar");
                let on = ((elapsed * 2.0) as usize).is_multiple_of(2);
                theme.paint(if on { Role::Active.color() } else { Role::Meta.color() }, bar)
            }
            Effect::Ellipsis { label } => {
                let dots = ((elapsed * 3.0) as usize) % 4;
                theme.paint(Role::Active.color(), &format!("{label}{}", ".".repeat(dots)))
            }
            Effect::Counter { done, total, noun, started } => {
                let secs = started.elapsed().as_secs_f64();
                counter_string(theme, set, *done, *total, noun, secs)
            }
            Effect::Bar { fraction, cells } => render_bar(theme, set, *fraction, *cells),
            Effect::Settle { label, done_at } => {
                let check = glyphs::glyph(set, "check");
                let fresh = done_at.elapsed() < Duration::from_millis(300);
                let color = if fresh { Color::BoldWhite } else { Role::Success.color() };
                theme.paint(color, &format!("{check} {label}"))
            }
            Effect::Shimmer { label } => {
                let base = (145, 152, 161); // dim gray
                let highlight = (120, 169, 255); // blue-40 #78A9FF
                shimmer::shimmer_at(label, elapsed, base, highlight)
            }
            Effect::CounterBar { done, total, noun, cells, started, label } => {
                let d = done.load(Ordering::Relaxed);
                let fraction = if *total == 0 { 0.0 } else { d as f64 / *total as f64 };
                let bar = render_bar(theme, set, fraction, *cells);
                let secs = started.elapsed().as_secs_f64();
                let lbl = label.as_ref().and_then(|l| l.lock().ok().map(|g| g.clone())).filter(|s| !s.is_empty());
                let counter = match lbl {
                    Some(l) => {
                        let dot = glyphs::glyph(set, "dot");
                        theme.paint(Role::Meta.color(), &format!("{d}/{total} {noun} {dot} {l} {dot} {secs:.1}s"))
                    }
                    None => counter_string(theme, set, d, *total, noun, secs),
                };
                format!("{} {}", bar, counter)
            }
            Effect::ExecSummary { done, total, started, panel, widths } => {
                let d = done.load(Ordering::Relaxed);
                let fraction = if *total == 0 { 0.0 } else { d as f64 / *total as f64 };
                let bar = render_bar(&panel.theme, panel.glyphs, fraction, EXEC_SUMMARY_CELLS);
                let dot = glyphs::glyph(panel.glyphs, "dot");
                let secs = started.elapsed().as_secs_f64();
                let summary = panel.paint(Role::Meta, &format!("{d}/{total} {dot} {secs:.1}s"));
                // Line 1: `  ▌ Execution   <bar> <done>/<total> · <elapsed>` (summary in the
                // section-header hint slot). Line 2: the dim column header on the shared grid.
                let head = format!("  {} {}   {} {}", panel.paint(Role::Active, panel.g("bar")), panel.paint(Role::Heading, "Execution"), bar, summary);
                format!("{}\n{}", head, exec_header_line(panel, widths))
            }
            Effect::ExecRowPending { panel, widths, kind, name, last } => {
                let connector = panel.paint(Role::Meta, panel.g(if *last { "ell" } else { "tee" }));
                let marker = panel.paint(Role::Meta, panel.g("dot"));
                let action = panel.paint(Role::Meta, "pending");
                let cells = ExecCells { connector: &connector, marker: &marker, kind, name, id_painted: "", id_vis: 0, action_painted: &action, action_vis: "pending".chars().count(), time_painted: "", suffix: "" };
                exec_row_line(widths, &cells)
            }
            Effect::ExecRowLive { panel, widths, kind, name, started, last } => {
                let connector = panel.paint(Role::Meta, panel.g(if *last { "ell" } else { "tee" }));
                let marker = live_marker(&panel.theme, panel.glyphs, elapsed);
                let time = panel.paint(Role::Meta, &format_duration(started.elapsed().as_millis() as u64));
                let cells = ExecCells { connector: &connector, marker: &marker, kind, name, id_painted: "", id_vis: 0, action_painted: "", action_vis: 0, time_painted: &time, suffix: "" };
                exec_row_line(widths, &cells)
            }
            Effect::ExecRowDone { panel, widths, marker, kind, name, id, duration_ms, changed_fields, last } => {
                let row = ExecRow { marker: *marker, kind: kind.clone(), name: name.clone(), changed_fields: changed_fields.clone(), duration_ms: *duration_ms, last: *last, id: id.clone() };
                exec_done_row(panel, widths, &row)
            }
        }
    }
}

/// Build the painted `"n/total noun · Xs"` counter string shared by `Counter` and `CounterBar`.
fn counter_string(theme: &Theme, set: GlyphSet, n: usize, total: usize, noun: &str, secs: f64) -> String {
    let dot = glyphs::glyph(set, "dot");
    theme.paint(Role::Meta.color(), &format!("{n}/{total} {noun} {dot} {secs:.1}s"))
}

/// Render a determinate eighth-block bar: `cells` wide, filled to `fraction`.
fn render_bar(theme: &Theme, set: GlyphSet, fraction: f64, cells: usize) -> String {
    let fills = glyphs::bar_fills(set);
    let f = fraction.clamp(0.0, 1.0);
    let total_eighths = (f * (cells * 8) as f64).round() as usize;
    let full = total_eighths / 8;
    let rem = total_eighths % 8;
    let mut filled = String::new();
    for _ in 0..full {
        filled.push_str(fills[fills.len() - 1]);
    }
    if rem > 0 && full < cells && fills.len() > 1 {
        filled.push_str(fills[rem - 1]);
    }
    // Draw the empty remainder as a dim track so the bar keeps a fixed width and the
    // trailing counter sits against it instead of floating in blank space. Paint the
    // two segments separately (filled = active accent, track = dim); skip painting an
    // empty segment so neither end emits a stray escape pair at 0% / 100%.
    let track = glyphs::bar_track(set).repeat(cells.saturating_sub(filled.chars().count()));
    let filled_seg = if filled.is_empty() { String::new() } else { theme.paint(Role::Active.color(), &filled) };
    let track_seg = if track.is_empty() { String::new() } else { theme.paint(Role::Meta.color(), &track) };
    format!("{}{}", filled_seg, track_seg)
}

/// A row registered with the Animator: an indicatif `ProgressBar` and its effect.
/// Exposed so `StartSpinnerPlan::execute()` can push directly into the shared
/// rows vec after calling `multi.add` — without holding the collector lock.
pub struct AnimatorRow {
    pub pb: ProgressBar,
    pub effect: Effect,
    /// Last message sent to `pb`; used to skip redundant identical repaints.
    pub last_msg: Option<String>,
    /// Rendered line for an elapsed-independent (`Effect::is_static`) effect, filled
    /// once by the ticker and reused every tick instead of rebuilding the frame. `None`
    /// for dynamic effects (and reset whenever the effect is swapped via `set_effect`).
    pub cached: Option<String>,
}

/// Drives all live-region effects from one ~10fps ticker thread.
pub struct Animator {
    rows: Arc<Mutex<Vec<AnimatorRow>>>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    theme: Theme,
    glyphs: GlyphSet,
    active: bool,
}

impl Animator {
    /// Create an Animator. In plain mode it is inert: `register`/`stop` are
    /// no-ops and no thread is spawned (zero ANSI, AC6).
    pub fn new(theme: Theme, glyphs: GlyphSet) -> Self {
        let active = !theme.is_plain();
        Self { rows: Arc::new(Mutex::new(Vec::new())), stop: Arc::new(AtomicBool::new(false)), handle: None, theme, glyphs, active }
    }

    /// Whether the Animator is active (non-plain mode). When false, all
    /// operations are no-ops and no thread is spawned.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Return a clone of the internal rows handle so external callers can push
    /// rows directly after performing indicatif work outside the collector lock.
    /// Only meaningful when `is_active()` is true.
    pub fn rows_handle(&self) -> Arc<Mutex<Vec<AnimatorRow>>> {
        self.rows.clone()
    }

    /// Start the single ticker thread. No-op in plain mode or if already started.
    pub fn start(&mut self) {
        // Clear the stop flag first: `stop()` leaves it set, so a start-after-stop
        // (e.g. reconciliation ticker → execution ticker) would otherwise spawn a
        // thread that sees `stop == true` and exits immediately, freezing every row.
        self.stop.store(false, Ordering::Relaxed);
        if !self.active || self.handle.is_some() {
            return;
        }
        let rows = self.rows.clone();
        let stop = self.stop.clone();
        let theme = self.theme.clone();
        let glyphs = self.glyphs;
        let start = Instant::now();
        self.handle = Some(std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(100)); // ~10 fps cap
                let elapsed = start.elapsed().as_secs_f64();
                let mut guard = rows.lock().unwrap();
                for row in guard.iter_mut() {
                    // Elapsed-independent effects (settled/prefilled Exec rows) are rendered
                    // once and cached, then reused — otherwise a large settled table would
                    // re-format every row ~10×/s. `last_msg` still gates the actual write.
                    if let Some(cached) = &row.cached {
                        if row.last_msg.as_deref() != Some(cached.as_str()) {
                            let msg = cached.clone();
                            row.pb.set_message(msg.clone());
                            row.last_msg = Some(msg);
                        }
                        continue;
                    }
                    let msg = row.effect.frame(elapsed, &theme, glyphs);
                    if row.effect.is_static() {
                        row.cached = Some(msg.clone());
                    }
                    if row.last_msg.as_deref() != Some(msg.as_str()) {
                        row.pb.set_message(msg.clone());
                        row.last_msg = Some(msg);
                    }
                }
            }
        }));
    }

    /// Bind a `ProgressBar` to an effect. No-op in plain mode. Returns the row id
    /// (index into the internal rows vec) so callers can later swap the effect via
    /// `set_effect`. In plain mode returns `0` (sentinel; `set_effect` is also a no-op).
    pub fn register(&self, pb: ProgressBar, effect: Effect) -> usize {
        if !self.active {
            return 0;
        }
        let mut rows = self.rows.lock().unwrap();
        let id = rows.len();
        rows.push(AnimatorRow { pb, effect, last_msg: None, cached: None });
        id
    }

    /// Replace the effect for an existing row by its id. No-op in plain mode or if
    /// the id is out of range (defensive — callers must not assume liveness).
    pub fn set_effect(&self, id: usize, effect: Effect) {
        if !self.active {
            return;
        }
        let mut rows = self.rows.lock().unwrap();
        if let Some(row) = rows.get_mut(id) {
            // Invalidate any cached line so the new effect re-renders (a static→static
            // swap, e.g. ExecRowPending → ExecRowDone, would otherwise keep the old line).
            row.cached = None;
            row.effect = effect;
        }
    }

    /// Stop the ticker and join the thread. Idempotent; guarantees no hang
    /// (AC6) — sets the flag, joins once, drops handles.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        self.rows.lock().unwrap().clear();
    }
}

impl Drop for Animator {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::panel::theme::ColorMode;

    #[test]
    fn plain_animator_is_inert() {
        let mut a = Animator::new(Theme::new(ColorMode::Plain), GlyphSet::Unicode);
        a.start();
        assert!(a.handle.is_none(), "no thread in plain mode");
        a.register(ProgressBar::hidden(), Effect::Spinner { label: "tool/a".into() });
        assert!(a.rows.lock().unwrap().is_empty(), "register is a no-op in plain mode");
        a.stop(); // must not hang
    }

    #[test]
    fn active_animator_starts_and_stops_without_hang() {
        let mut a = Animator::new(Theme::new(ColorMode::Dark), GlyphSet::Unicode);
        a.start();
        assert!(a.handle.is_some(), "thread spawned in active mode");
        a.register(ProgressBar::hidden(), Effect::Shimmer { label: "resource".into() });
        std::thread::sleep(Duration::from_millis(250)); // let it tick
        a.stop(); // joins; must return promptly
        assert!(a.handle.is_none(), "handle dropped after stop");
    }

    /// `start()` after `stop()` must re-arm the ticker: `stop()` leaves the flag set,
    /// so a naive restart spawns a thread that exits on its first `stop.load()` and
    /// freezes every row. The fix clears the flag at the top of `start()`.
    #[test]
    fn restart_after_stop_respawns_live_ticker() {
        let mut a = Animator::new(Theme::new(ColorMode::Dark), GlyphSet::Unicode);
        a.start();
        a.stop();
        assert!(a.handle.is_none(), "stopped");
        a.start();
        assert!(a.handle.is_some(), "restart spawns a fresh thread");
        assert!(!a.stop.load(Ordering::Relaxed), "stop flag cleared so the new thread ticks");
        a.stop();
    }

    #[test]
    fn effect_frames_are_nonempty_and_stable_shape() {
        let theme = Theme::new(ColorMode::Dark);
        let set = GlyphSet::Unicode;
        for eff in [
            Effect::Spinner { label: "tool/a".into() },
            Effect::Pulse,
            Effect::Ellipsis { label: "reconciliation".into() },
            Effect::Counter { done: 7, total: 10, noun: "checked".into(), started: Instant::now() },
            Effect::Bar { fraction: 0.5, cells: 10 },
            Effect::Settle { label: "agent".into(), done_at: Instant::now() },
            Effect::Shimmer { label: "agent".into() },
        ] {
            let f = eff.frame(0.3, &theme, set);
            assert!(!f.is_empty(), "effect frame should be non-empty");
        }
    }

    /// The per-row spinner marker animates in both glyph modes, folded into one test:
    /// - Unicode: a single `●` (one column, aligning under the settled `✓`), never a
    ///   hollow `○`, that pulses by *color* over time (two elapsed values → different
    ///   strings) while the glyph stays a lone dot and the row label is preserved.
    /// - Ascii: no truecolor to pulse, so it animates by *shape* — the `| / - \`
    ///   ticker — advancing between frames and staying pure ascii.
    #[test]
    fn spinner_marker_animates_per_glyph_set() {
        // Unicode: color-pulsing single dot.
        let dark = Theme::new(ColorMode::Dark);
        let uni = Effect::Spinner { label: "tool/a".into() };
        let bright = uni.frame(0.0, &dark, GlyphSet::Unicode);
        let dim = uni.frame(PULSE_PERIOD_SECS / 2.0, &dark, GlyphSet::Unicode);
        assert_eq!(bright.matches('\u{25cf}').count(), 1, "exactly one filled dot: {bright:?}");
        assert!(!bright.contains('\u{25cb}'), "never a hollow ○ frame: {bright:?}");
        assert_ne!(bright, dim, "marker color must change over time (it's alive): {bright:?} vs {dim:?}");
        assert!(bright.contains("tool/a") && dim.contains("tool/a"), "row label preserved each frame");

        // Ascii: shape-ticker (Plain mode so there's no ANSI to reason about).
        let plain = Theme::new(ColorMode::Plain);
        let asc = Effect::Spinner { label: "tool/a".into() };
        let f0 = asc.frame(0.0, &plain, GlyphSet::Ascii);
        let f1 = asc.frame(0.1, &plain, GlyphSet::Ascii);
        assert!(f0.is_ascii() && f1.is_ascii(), "ascii ticker stays ascii: {f0:?} {f1:?}");
        assert_ne!(f0, f1, "ticker advances shape between frames: {f0:?} vs {f1:?}");
    }

    #[test]
    fn counter_bar_reflects_shared_count() {
        let theme = Theme::new(ColorMode::Plain); // no ANSI, easy to assert
        let done = Arc::new(AtomicUsize::new(0));
        let eff = Effect::CounterBar { done: done.clone(), total: 4, noun: "applied".into(), cells: 8, started: Instant::now(), label: None };
        assert!(eff.frame(0.1, &theme, GlyphSet::Unicode).contains("0/4 applied"), "initial count");
        done.store(2, Ordering::Relaxed);
        let f = eff.frame(0.1, &theme, GlyphSet::Unicode);
        assert!(f.contains("2/4 applied"), "count bumped: {f}");
        assert!(f.contains('\u{2588}'), "bar shows fill at 2/4: {f}");
    }

    #[test]
    fn determinate_bar_fills_proportionally() {
        let theme = Theme::new(ColorMode::Plain); // no ANSI, easy to assert on chars
        let full = render_bar(&theme, GlyphSet::Unicode, 1.0, 8);
        assert_eq!(full.chars().filter(|c| *c == '\u{2588}').count(), 8, "fraction 1.0 fills all 8 cells");
        assert_eq!(full.chars().filter(|c| *c == '\u{2591}').count(), 0, "fraction 1.0 leaves no empty track");
        let empty = render_bar(&theme, GlyphSet::Unicode, 0.0, 8);
        assert_eq!(empty.chars().filter(|c| *c == '\u{2591}').count(), 8, "fraction 0.0 is all dim track");
        assert_eq!(empty.chars().filter(|c| *c == '\u{2588}').count(), 0, "fraction 0.0 has no fill");
    }

    /// A plain panel for the ▌ Execution effect tests — Plain mode keeps the painted cells
    /// ANSI-free so we can assert on the literal text (the pulsing marker still carries its
    /// own truecolor, which is fine — these effects only ever run live in a non-plain TTY).
    fn exec_panel() -> Panel {
        Panel::new(Theme::new(ColorMode::Plain), 200, GlyphSet::Unicode)
    }

    /// A settled live row must be byte-identical to what the final static render draws for the
    /// same row (with `last = false`) — that's what lets the live table finalize without churn.
    #[test]
    fn exec_row_done_effect_matches_static_row() {
        let panel = exec_panel();
        let w = ExecWidths::new(10, 30, 7);
        let row = ExecRow { marker: ExecMarker::Created, kind: "tool".into(), name: "weather_tool".into(), changed_fields: vec![], duration_ms: 2000, last: false, id: Some("abc123".into()) };
        let eff = Effect::ExecRowDone { panel: panel.clone(), widths: w.clone(), marker: row.marker, kind: row.kind.clone(), name: row.name.clone(), id: row.id.clone(), duration_ms: row.duration_ms, changed_fields: row.changed_fields.clone(), last: false };
        let live = eff.frame(0.5, &panel.theme, panel.glyphs);
        let static_row = exec_done_row(&panel, &w, &row);
        assert_eq!(live, static_row, "live done row must equal the static row formatter");
        assert!(live.contains("created") && live.contains("[id=abc123]") && live.contains("2.0s"), "row carries action + id + time: {live:?}");
    }

    /// An in-progress row shows the `├─` connector, the kind+name, a running `●` marker, and a
    /// ticking elapsed in the Time column (no action / id yet).
    #[test]
    fn exec_row_live_shows_connector_name_and_marker() {
        let panel = exec_panel();
        let eff = Effect::ExecRowLive { panel: panel.clone(), widths: ExecWidths::new(14, 30, 7), kind: "knowledge_base".into(), name: "ibm_kb".into(), started: Instant::now(), last: false };
        let f = eff.frame(0.3, &panel.theme, panel.glyphs);
        assert!(f.contains("\u{251c}\u{2500}"), "live row uses the ├─ connector: {f:?}");
        assert!(f.contains("knowledge_base") && f.contains("ibm_kb"), "kind + name present: {f:?}");
        assert!(f.contains('\u{25cf}'), "running marker is a ●: {f:?}");
        assert!(!f.contains("created") && !f.contains("[id="), "no action/id while running: {f:?}");
    }

    /// The header effect shows the `▌ Execution` heading, a live `done/total` count that tracks
    /// the shared counter, and the column header (Type/Name/Action/Time) on a second line.
    #[test]
    fn exec_summary_shows_heading_live_count_and_columns() {
        let panel = exec_panel();
        let done = Arc::new(AtomicUsize::new(0));
        let eff = Effect::ExecSummary { done: done.clone(), total: 5, started: Instant::now(), panel: panel.clone(), widths: ExecWidths::new(10, 30, 7) };
        let f = eff.frame(0.1, &panel.theme, panel.glyphs);
        assert!(f.contains("Execution"), "section heading present: {f:?}");
        assert!(f.contains("0/5"), "summary count present: {f:?}");
        for h in ["Type", "Name", "Action", "Time"] {
            assert!(f.contains(h), "column header {h} present: {f:?}");
        }
        assert!(f.contains('\n'), "header is two lines (summary + columns): {f:?}");
        done.store(3, Ordering::Relaxed);
        assert!(eff.frame(0.1, &panel.theme, panel.glyphs).contains("3/5"), "count reflects the shared done counter");
    }

    /// A prefilled (not-yet-started) row shows the kind+name with a `pending` action and no id
    /// or time, and a `last = true` row draws the `└─` connector (known up front).
    #[test]
    fn exec_row_pending_shows_scope_and_last_connector() {
        let panel = exec_panel();
        let last = Effect::ExecRowPending { panel: panel.clone(), widths: ExecWidths::new(10, 30, 7), kind: "agent".into(), name: "weather_agent".into(), last: true };
        let f = last.frame(0.0, &panel.theme, panel.glyphs);
        assert!(f.contains("\u{2514}\u{2500}"), "last pending row uses └─: {f:?}");
        assert!(f.contains("agent") && f.contains("weather_agent"), "kind + name visible up front: {f:?}");
        assert!(f.contains("pending"), "pending status shown: {f:?}");
        assert!(!f.contains("[id=") && !f.contains("created"), "no id / action yet while pending: {f:?}");

        let mid = Effect::ExecRowPending { panel: panel.clone(), widths: ExecWidths::new(10, 30, 7), kind: "tool".into(), name: "weather_tool".into(), last: false };
        assert!(mid.frame(0.0, &panel.theme, panel.glyphs).contains("\u{251c}\u{2500}"), "non-last pending row uses ├─");
    }
}
