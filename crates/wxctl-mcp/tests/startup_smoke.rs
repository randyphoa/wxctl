//! Catch this class of bug: rmcp
//! validates every tool's `outputSchema` has a root `type: object` when the tool router
//! is built (inside `WxctlMcpServer::new`). A non-object output schema panics there,
//! invisibly to `cargo build`/`clippy`/unit tests. Constructing the server in both modes
//! exercises the router build for the read-only + mutating tool set (compose_start replaces compose_identify), so the panic surfaces at `cargo test`.
//!
//! Uses a bogus profile name: `new` builds the router (the failure mode under test) but
//! does NOT construct the `WxctlClient` (that is lazy, on first live-tool call), so no
//! profile file or network is touched.

use wxctl_mcp::WxctlMcpServer;

#[test]
fn server_constructs_without_panicking_in_both_modes() {
    // Both modes build the tool router (the failure mode under test) — reaching the end
    // means every registered tool's output schema is object-rooted.
    // - read_only=false registers all tools: read-only + compose_start + 3 mutating
    //   (apply/destroy ExecuteOutput, test TestOutput included).
    // - read_only=true registers only the read-only tools (compose_start, compose_paths,
    //   compose_prompt among them).
    for read_only in [false, true] {
        let _server = WxctlMcpServer::new("nonexistent-profile-for-smoke", None, read_only, false);
    }
}
