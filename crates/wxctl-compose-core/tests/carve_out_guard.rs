//! I3 carve-out guard: no compile-time include literal under `wxctl/crates/` may resolve
//! to a path outside the crate that owns it. This pins the public `wxctl/` workspace's
//! self-containment (root CLAUDE.md invariant) so a future repo-root include that climbs
//! out of the crate (a path full of leading parent-dir segments) can never silently regress it.
//!
//! The scanner keys off a macro name followed immediately by `(` and a string literal, so it
//! ignores prose/backtick mentions of the macro names and string-literal occurrences (e.g. its
//! own match list below). It does NOT strip comments, so do not write a literal include macro
//! call (name + `(` + string) inside a comment anywhere in this tree.

use std::path::{Path, PathBuf};

/// `…/wxctl/crates` — the parent of this crate's directory.
fn crates_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().expect("crate dir has a parent").to_path_buf()
}

/// The crate root owning `file` = the first ancestor that is a direct child of `crates_root`.
fn owning_crate_root(file: &Path, crates_root: &Path) -> PathBuf {
    let mut cur = file;
    while let Some(parent) = cur.parent() {
        if parent == crates_root {
            return cur.to_path_buf();
        }
        cur = parent;
    }
    panic!("{} is not under {}", file.display(), crates_root.display());
}

/// Collect every `.rs` file under `dir`, skipping `target/`.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display())) {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            if path.file_name().and_then(|n| n.to_str()) == Some("target") {
                continue;
            }
            rust_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Extract the path literal from each `include_str!("…")` / `include_bytes!("…")` invocation.
/// A match counts as an invocation only when the macro name is immediately followed (modulo
/// whitespace) by `(` and a string-literal first argument — so prose/backtick mentions and
/// string-literal occurrences of the macro names (e.g. this file's own match list) are skipped.
fn include_paths(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    for macro_name in ["include_str!", "include_bytes!"] {
        let mut from = 0;
        while let Some(idx) = src[from..].find(macro_name) {
            let after = from + idx + macro_name.len();
            // Require `(` to immediately follow the name; otherwise it's not an invocation.
            let Some(rest) = src[after..].trim_start().strip_prefix('(') else {
                from = after;
                continue;
            };
            // The first argument must be a string literal: `"<path>"`.
            let rest = rest.trim_start();
            if let Some(inner) = rest.strip_prefix('"')
                && let Some(close) = inner.find('"')
            {
                out.push(inner[..close].to_string());
            }
            from = after;
        }
    }
    out
}

#[test]
fn no_include_escapes_its_crate() {
    let crates_root = crates_root();
    let mut files = Vec::new();
    rust_files(&crates_root, &mut files);
    assert!(!files.is_empty(), "found no .rs files under {}", crates_root.display());

    let mut escapes = Vec::new();
    for file in &files {
        let src = std::fs::read_to_string(file).unwrap_or_else(|e| panic!("read {}: {e}", file.display()));
        let crate_root = owning_crate_root(file, &crates_root);
        let file_dir = file.parent().expect("file has a parent dir");
        for lit in include_paths(&src) {
            // Resolve the include path relative to the file's directory, lexically.
            let mut resolved = file_dir.to_path_buf();
            for comp in Path::new(&lit).components() {
                use std::path::Component;
                match comp {
                    Component::ParentDir => {
                        resolved.pop();
                    }
                    Component::CurDir => {}
                    other => resolved.push(other.as_os_str()),
                }
            }
            if !resolved.starts_with(&crate_root) {
                escapes.push(format!("{} → include `{}` resolves to {} (outside crate {})", file.display(), lit, resolved.display(), crate_root.display()));
            }
        }
    }
    assert!(escapes.is_empty(), "include_str!/include_bytes! escaping its crate:\n{}", escapes.join("\n"));
}
