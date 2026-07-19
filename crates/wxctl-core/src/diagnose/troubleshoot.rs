//! Match a project's troubleshooting runbooks to a run's errors. Two signals: the
//! error **code** (a doc mentioning `WXCTL-H001` is a strong match) and **keywords**
//! drawn from error messages.
//!
//! The corpus belongs to whoever runs wxctl, not to wxctl itself: any directory of
//! Markdown files describing recurring failures. Directory from
//! `WXCTL_TROUBLESHOOT_DIR`, else `docs/troubleshoot/` resolved relative to the
//! current working directory, so a repo that keeps runbooks there gets them matched
//! automatically. Absent directory means no corpus, which is the common case for an
//! installed binary run from an arbitrary cwd; the section is then skipped silently
//! rather than reported as an error. Documented for users in `wxctl/AGENTS.md`
//! (Error recovery).

use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize)]
pub struct TroubleshootMatch {
    /// File stem, e.g. `engine-list-envelope-fix`.
    pub slug: String,
    /// Absolute path to the matched markdown file.
    pub path: String,
    /// First markdown H1 (`# ...`) as the human title; falls back to the slug.
    pub title: String,
    /// Why it matched: the error codes and/or keywords found in the doc.
    pub matched_on: Vec<String>,
}

/// Resolve the troubleshoot directory: `WXCTL_TROUBLESHOOT_DIR`, else `docs/troubleshoot`.
fn troubleshoot_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("WXCTL_TROUBLESHOOT_DIR") {
        return PathBuf::from(dir);
    }
    PathBuf::from("docs").join("troubleshoot")
}

/// First H1 line (`# Title`) of a markdown body, trimmed; `None` if no H1.
fn extract_title(body: &str) -> Option<String> {
    body.lines().find_map(|l| l.strip_prefix("# ").map(|t| t.trim().to_string()))
}

/// Match each `.md` file against the given codes + keywords. A file matches if it
/// contains any code verbatim, or any keyword (case-insensitive, ≥4 chars to avoid
/// noise). `matched_on` lists every signal that hit. Returns at most `limit` files,
/// strongest first (more signals = stronger). Absent dir → empty (silent skip).
pub fn match_troubleshoot(codes: &[String], keywords: &[String], limit: usize) -> Vec<TroubleshootMatch> {
    let dir = troubleshoot_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
    let kw: Vec<String> = keywords.iter().map(|k| k.to_lowercase()).filter(|k| k.len() >= 4).collect();

    let mut matches: Vec<TroubleshootMatch> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .filter_map(|path| {
            let body = std::fs::read_to_string(&path).ok()?;
            let lower = body.to_lowercase();
            let mut matched_on = Vec::new();
            for code in codes {
                if !code.is_empty() && body.contains(code.as_str()) {
                    matched_on.push(code.clone());
                }
            }
            for k in &kw {
                if lower.contains(k.as_str()) {
                    matched_on.push(k.clone());
                }
            }
            if matched_on.is_empty() {
                return None;
            }
            let slug = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let title = extract_title(&body).unwrap_or_else(|| slug.clone());
            Some(TroubleshootMatch { slug, path: path.to_string_lossy().into_owned(), title, matched_on })
        })
        .collect();

    // Strongest first (more matched signals), then by slug for stable ordering.
    matches.sort_by(|a, b| b.matched_on.len().cmp(&a.matched_on.len()).then_with(|| a.slug.cmp(&b.slug)));
    matches.truncate(limit);
    matches
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_on_code_and_keyword_and_skips_absent_dir() {
        let _env = crate::test_env_lock();
        let tmp = std::env::temp_dir().join(format!("wxctl-ts-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("envelope-fix.md"), "# Engine List Envelope Fix\n\nWhen WXCTL-R001 hits a list envelope mismatch...\n").unwrap();
        std::fs::write(tmp.join("unrelated.md"), "# Something Else\n\nNo signals here.\n").unwrap();
        unsafe { std::env::set_var("WXCTL_TROUBLESHOOT_DIR", &tmp) };

        let by_code = match_troubleshoot(&["WXCTL-R001".to_string()], &[], 5);
        assert_eq!(by_code.len(), 1);
        assert_eq!(by_code[0].slug, "envelope-fix");
        assert_eq!(by_code[0].title, "Engine List Envelope Fix");
        assert!(by_code[0].matched_on.contains(&"WXCTL-R001".to_string()));

        let by_kw = match_troubleshoot(&[], &["envelope".to_string(), "ab".to_string()], 5);
        assert_eq!(by_kw.len(), 1, "matches on the >=4-char keyword, ignores the 2-char one");

        unsafe { std::env::remove_var("WXCTL_TROUBLESHOOT_DIR") };
        // Absent dir → empty, no panic.
        unsafe { std::env::set_var("WXCTL_TROUBLESHOOT_DIR", tmp.join("does-not-exist")) };
        assert!(match_troubleshoot(&["WXCTL-R001".to_string()], &[], 5).is_empty());
        unsafe { std::env::remove_var("WXCTL_TROUBLESHOOT_DIR") };
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
