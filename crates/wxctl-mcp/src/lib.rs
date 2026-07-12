//! `wxctl-mcp` — an `rmcp` stdio MCP server exposing `wxctl` as tools for LLM hosts.
//!
//! Read-only surface (7 tools): two discovery tools backed by the schema loader (no
//! profile/network), two live tools (`validate`/`plan`) backed by a shared `WxctlClient`,
//! and three compose tools (`compose_identify`/`compose_paths`/`compose_prompt`) that are
//! pure-compute with no profile/network. Mutating surface (4 tools, registered unless
//! `--read-only`): `apply`/`destroy` (gated by `confirm:true`) and `test`, streaming MCP
//! progress and honoring cancellation; plus `compose_scaffold` (FS-writing, no profile).

pub mod compose_tools;
mod config_input;
mod run_scope;
mod server;
mod tools;
mod tools_live;
mod tools_mutate;
mod tools_runs;

pub use server::{WxctlMcpServer, serve};

/// Test-only helpers shared across the crate's unit-test modules.
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::{Mutex, MutexGuard};

    /// One process-wide lock guarding the global current directory. Any test that
    /// reads or mutates `std::env::set_current_dir`/`current_dir` must hold it, or
    /// tests across modules race on the shared process CWD (cargo runs them on
    /// threads in one process). Poison is ignored — a panicking test still releases.
    static CWD_LOCK: Mutex<()> = Mutex::new(());

    pub(crate) fn lock_cwd() -> MutexGuard<'static, ()> {
        CWD_LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }
}
