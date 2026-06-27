//! `wxctl-compose-core` — the pure, wasm-safe compose core. Path resolution, prompt
//! assembly (config + test), template embedding, and existing-resources rendering.
//! Compiles for native AND `wasm32-unknown-unknown`. Deps: `wxctl-schema` + serde only
//! (no `wxctl-core`, `reqwest`, `tokio`, or unconditional `std::fs`/`std::env`).

pub mod paths;
pub mod prompt;
pub mod recipe;
pub mod templates;

mod prompt_body;

pub use prompt_body::extract_prompt_body;

pub use paths::{PathsInput, resolve_paths};
pub use prompt::{assemble_config_prompt, assemble_test_prompt, render_existing_resources};
pub use recipe::{Clarification, ComposeRecipe, FixLoop, RecipeStep, assemble_identify_prompt, assemble_recipe};
