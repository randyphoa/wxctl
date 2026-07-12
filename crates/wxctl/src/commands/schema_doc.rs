//! Deterministic schema-reference markdown for the LLM generation/fix prompts.
//! The renderer now lives in the wasm-safe `wxctl-schema` crate (single source of
//! truth shared with the remote MCP server); this re-export preserves the existing
//! `super::schema_doc::render_kinds_markdown` call sites in `generate` and `validate`.

pub use wxctl_schema::render_kinds_markdown;
