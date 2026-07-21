//! AC1: every public `wxctl-schema` entry point — kind listing, kind explain,
//! offline config validation, kinds-markdown rendering — including the FIRST
//! schema access in the process, succeeds from a thread with a 128 KiB stack.
//!
//! The static IR is a pointer walk into the data segment, so schema access needs
//! a trivial stack; a delegating or deeply-recursive implementation would
//! stack-overflow at 128 KiB (an eighth of the smallest platform default, and
//! far below the pre-redesign ~1 MiB debug footprint). The companion
//! "without spawning any auxiliary thread" half of AC1 is proven structurally by
//! I2 — no `std::thread` anywhere in `crates/wxctl-schema/src/` (grepped in the
//! E2E invariants task) — a library with no thread API cannot delegate.

#[test]
fn schema_entry_points_run_on_a_128kib_stack() {
    let handle = std::thread::Builder::new()
        .name("small-stack-guard".into())
        .stack_size(128 * 1024)
        .spawn(|| {
            // FIRST schema access in the process happens here, on the 128 KiB thread.
            let kinds = wxctl_schema::list_kinds(None, None);
            assert!(!kinds.is_empty(), "list_kinds returns the shipped kind set");

            let _view = wxctl_schema::explain_kind("agent").expect("explain_kind(agent) succeeds");

            let _report = wxctl_schema::validate_config("resources: []").expect("offline validate_config parses");

            let md = wxctl_schema::render_kinds_markdown(None).expect("render_kinds_markdown(None) succeeds");
            assert!(md.contains("**Service:**"), "rendered markdown documents kinds");
        })
        .expect("spawn 128 KiB guard thread");

    handle.join().expect("128 KiB guard thread completed without stack overflow");
}
