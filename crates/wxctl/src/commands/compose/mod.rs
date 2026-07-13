//! `wxctl compose` — the hidden LLM compose pipeline (identify → paths → prompt → scaffold).
//! Replaces the former top-level `generate`/`resolve`/`scaffold` commands.

pub mod identify;
pub mod paths;
pub mod prompt;
pub mod scaffold;
