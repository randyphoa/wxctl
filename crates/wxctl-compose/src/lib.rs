//! `wxctl-compose` — the native FS-touching compose surface (identify → scaffold →
//! implementation prompt → existing-resources FS scan) plus a re-export of the pure
//! `wxctl-compose-core` (paths, config/test prompts, templates). The CLI (thin I/O
//! wrappers) and the `wxctl-mcp` server (thin tool wrappers) import everything through
//! this crate; no stdout/file I/O lives in the entry points (each returns a value).

pub mod prompt;
pub mod scaffold;

// Re-export the pure core's modules + fns so existing `wxctl_compose::*` call sites
// (CLI, local MCP) and the native modules above keep resolving unchanged.
pub use wxctl_compose_core::{ComposeRecipe, FixLoop, PathsInput, RecipeStep, assemble_config_prompt, assemble_recipe, assemble_test_prompt, extract_prompt_body, paths, render_existing_resources, resolve_paths, templates};

// Value-returning entry points the CLI + MCP wrappers call (native, stay here).
pub use prompt::{assemble_implementation_prompt, discover_existing_resources, tool_descriptions_from_config};
pub use scaffold::{ScaffoldOutput, scaffold_config};
pub use wxctl_compose_core::assemble_identify_prompt;
