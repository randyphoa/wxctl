//! Scaffold run manifest: per-path outcome, human + check-friendly rendering.

use std::fmt::Write as _;

/// Outcome for a single target path in a scaffold run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// File/dir was written (or, under --dry-run, would have been written).
    Created,
    /// Target already existed; left untouched.
    SkippedExists,
    /// Generation/write failed; carries the reason.
    Failed(String),
}

/// One manifest line: a path and its outcome.
#[derive(Debug, Clone)]
pub struct Entry {
    pub path: String,
    pub outcome: Outcome,
}

/// Collected outcomes for an entire scaffold run.
#[derive(Debug, Default)]
pub struct Manifest {
    pub entries: Vec<Entry>,
    /// True when assembled under --dry-run (changes the header wording).
    pub dry_run: bool,
}

impl Manifest {
    pub fn new(dry_run: bool) -> Self {
        Self { entries: Vec::new(), dry_run }
    }

    pub fn created(&mut self, path: impl Into<String>) {
        self.entries.push(Entry { path: path.into(), outcome: Outcome::Created });
    }

    pub fn skipped(&mut self, path: impl Into<String>) {
        self.entries.push(Entry { path: path.into(), outcome: Outcome::SkippedExists });
    }

    pub fn failed(&mut self, path: impl Into<String>, reason: impl Into<String>) {
        self.entries.push(Entry { path: path.into(), outcome: Outcome::Failed(reason.into()) });
    }

    /// True if any entry failed (drives the non-zero exit).
    pub fn any_failed(&self) -> bool {
        self.entries.iter().any(|e| matches!(e.outcome, Outcome::Failed(_)))
    }

    pub fn counts(&self) -> (usize, usize, usize) {
        let mut created = 0;
        let mut skipped = 0;
        let mut failed = 0;
        for e in &self.entries {
            match e.outcome {
                Outcome::Created => created += 1,
                Outcome::SkippedExists => skipped += 1,
                Outcome::Failed(_) => failed += 1,
            }
        }
        (created, skipped, failed)
    }

    /// Render the manifest to a String (printed to stderr by the caller).
    pub fn render(&self) -> String {
        let mut out = String::new();
        let verb = if self.dry_run { "would create" } else { "created" };
        for e in &self.entries {
            match &e.outcome {
                Outcome::Created => {
                    let _ = writeln!(out, "  {verb}: {}", e.path);
                }
                Outcome::SkippedExists => {
                    let _ = writeln!(out, "  skipped (exists): {}", e.path);
                }
                Outcome::Failed(reason) => {
                    let _ = writeln!(out, "  FAILED: {} — {reason}", e.path);
                }
            }
        }
        let (created, skipped, failed) = self.counts();
        let head = if self.dry_run { "Scaffold dry-run" } else { "Scaffold complete" };
        let _ = writeln!(out, "\n{head}: {created} created, {skipped} skipped, {failed} failed");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_render_and_dry_run_wording() {
        // Live run: one of each outcome → any_failed, (created, skipped, failed) counts, full render.
        let mut m = Manifest::new(false);
        m.created("a.py");
        m.skipped("b/");
        m.failed("c.txt", "boom");
        assert!(m.any_failed());
        assert_eq!(m.counts(), (1, 1, 1));
        let r = m.render();
        assert!(r.contains("created: a.py"));
        assert!(r.contains("skipped (exists): b/"));
        assert!(r.contains("FAILED: c.txt — boom"));
        assert!(r.contains("1 created, 1 skipped, 1 failed"));

        // Dry-run mode rewords a Created entry as "would create" and labels the run dry-run.
        let mut m = Manifest::new(true);
        m.created("a.py");
        let r = m.render();
        assert!(r.contains("would create: a.py"));
        assert!(r.contains("dry-run"));
    }
}
