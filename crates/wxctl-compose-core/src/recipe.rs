//! Compose orchestrator core — pure, wasm-safe. `assemble_identify_prompt` (relocated
//! from `wxctl-compose`) builds the Pass-1 identification prompt; `assemble_recipe`
//! builds the structured `ComposeRecipe` an agent follows directly in Rust (the recipe
//! is compile-checked data, not a runtime-parsed file) and substitutes the use-case into
//! the identify prompt. No FS, no env, no network — every dependency
//! (`wxctl_schema::dependency_graph`, the embedded templates) is wasm-safe.
//!
//! ## The compose recipe (single source of truth)
//!
//! `assemble_recipe` returns the ordered steps the `compose_start` MCP orchestrator tool
//! hands an agent, the bounded fix loop, the gates, and the clarification policy. The nine
//! step `name` values are a **stable contract** (identify, paths, generate, validate_fix,
//! generate_tests, scaffold, plan, apply, test) — downstream parity and acceptance checks
//! key off them. Each step's `tier` is `config` (pure compute — no filesystem, profile, or
//! network) or `deploy` (needs a local execution environment: FS and/or a live profile).
//! Steps 1–5 are `config`; steps 6–9 are `deploy`. A config-only consumer can stop after the
//! config tier with a validated `config.yaml` + `kind: test` suite; `wxctl-mcp` and the CLI
//! run the full config + deploy flow. Edit this function to change the recipe everywhere it
//! is consumed.

use anyhow::Result;
use serde::Serialize;
use wxctl_schema::dependency_graph;

/// One ordered step in the compose recipe. `tier` is `config` (pure compute, runnable on
/// every surface) or `deploy` (needs a local execution environment). `tool`/`gate` are
/// present only where the step calls a discrete MCP tool or carries a precondition.
#[derive(Debug, Clone, Serialize)]
pub struct RecipeStep {
    pub n: u32,
    pub name: String,
    pub tier: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<String>,
}

/// The bounded fix loop policy returned to the agent.
#[derive(Debug, Clone, Serialize)]
pub struct FixLoop {
    pub max_iterations: u32,
    pub policy: String,
}

/// The clarification policy: how to handle a `kind: clarification_request` emitted by the
/// generate step. Surfaced to the agent alongside the fix loop.
#[derive(Debug, Clone, Serialize)]
pub struct Clarification {
    pub policy: String,
}

/// The structured compose recipe. `identify_prompt` is filled in at assembly time.
#[derive(Debug, Clone, Serialize)]
pub struct ComposeRecipe {
    pub identify_prompt: String,
    #[serde(rename(serialize = "recipe"))]
    pub steps: Vec<RecipeStep>,
    pub fix_loop: FixLoop,
    pub gates: Vec<String>,
    pub clarification: Clarification,
}

impl ComposeRecipe {
    /// Return a tier-limited *view* of this recipe: keep only the steps at or below
    /// `max_tier` in the `config` → `deploy` ordering. `Some("config")` keeps the five
    /// pure-compute authoring steps (identify…generate_tests) — the cut line for a caller
    /// that cannot reach the deploy tier (no filesystem / no profile); `Some("deploy")` or
    /// `None` keeps the full nine-step recipe. The `fix_loop`, `gates`, and `clarification`
    /// policy are cross-cutting guidance and are retained unchanged — they stay accurate for
    /// whichever steps survive. This is a filtered view of the single source of truth
    /// (`assemble_recipe`), never a forked recipe; the default (`None`) is a no-op, so every
    /// surface that does not opt in keeps returning the identical full recipe.
    pub fn with_max_tier(mut self, max_tier: Option<&str>) -> Result<Self> {
        match max_tier {
            None | Some("deploy") => {} // full recipe — keep every step
            Some("config") => self.steps.retain(|s| s.tier == "config"),
            Some(other) => anyhow::bail!("invalid max_tier '{other}'. Valid values: config, deploy."),
        }
        Ok(self)
    }
}

/// Assemble the resource-identification prompt (Pass 1) from raw use-case text.
/// Relocated verbatim from `wxctl-compose::identify` so it is reachable from wasm.
pub fn assemble_identify_prompt(user_input: &str) -> Result<String> {
    let template = crate::templates::IDENTIFICATION;
    let catalog = dependency_graph::resource_catalog_markdown();
    let body = crate::extract_prompt_body(template);
    Ok(body.replace("<RESOURCE_CATALOG>", &catalog).replace("<USER_INPUT>", user_input))
}

/// Build the full `ComposeRecipe` for a use case: construct the recipe data, then attach the
/// assembled identify prompt. Pure compute — no FS/env/network. The recipe is compile-checked
/// Rust data; a malformed edit is a compile error, never a runtime parse error.
pub fn assemble_recipe(user_input: &str) -> Result<ComposeRecipe> {
    Ok(ComposeRecipe {
        identify_prompt: assemble_identify_prompt(user_input)?,
        steps: recipe_steps(),
        fix_loop: FixLoop {
            max_iterations: 3,
            policy: "If wxctl_validate returns valid:false, run the returned fix_prompt with your model, regenerate config.yaml, and re-validate. Stop after at most 3 iterations; if still invalid, surface the remaining errors and do not proceed.".to_string(),
        },
        gates: vec![
            "The plan must be error-free before apply: run wxctl_plan and confirm it reports no errors before calling wxctl_apply with confirm:true.".to_string(),
            "wxctl_apply requires confirm:true — never call it without first reviewing an error-free wxctl_plan.".to_string(),
        ],
        clarification: Clarification {
            policy: "If the generate step returns a document with kind:clarification_request, do not fabricate values — surface its questions to the user, collect the answers, then re-run the generate step with the answers folded into the use case. Never invent account ids, CRNs, credentials, or resource names to get past a clarification request.".to_string(),
        },
    })
}

/// The nine ordered recipe steps. `name`/`tier` order is a stable contract.
fn recipe_steps() -> Vec<RecipeStep> {
    fn step(n: u32, name: &str, tier: &str, action: &str, tool: Option<&str>, gate: Option<&str>) -> RecipeStep {
        RecipeStep { n, name: name.to_string(), tier: tier.to_string(), action: action.to_string(), tool: tool.map(str::to_string), gate: gate.map(str::to_string) }
    }
    vec![
        step(1, "identify", "config", "Run the returned identify_prompt with your model to produce a resource list (resource_list.yaml).", None, None),
        step(2, "paths", "config", "Call compose_paths with the resource list to resolve dependencies and the recommended deployment path (paths.yaml).", Some("compose_paths"), None),
        step(3, "generate", "config", "Call compose_prompt with the use case and paths, then run the returned prompt with your model to produce config.yaml. If it returns kind:clarification_request, follow the clarification policy.", Some("compose_prompt"), None),
        step(
            4,
            "validate_fix",
            "config",
            "Call wxctl_validate on config.yaml (config-tier validate may skip source-file checks). If valid:false, run the returned fix_prompt, regenerate, and re-validate — at most 3 iterations.",
            Some("wxctl_validate"),
            Some("Do not proceed until wxctl_validate returns valid:true (within 3 fix iterations)."),
        ),
        step(
            5,
            "generate_tests",
            "config",
            "Call compose_prompt with test_config:true on the validated config.yaml, then run the returned prompt with your model to append a kind:test suite. The test prompt needs only config.yaml — it exercises agent behavior, not tool internals.",
            Some("compose_prompt"),
            None,
        ),
        step(
            6,
            "scaffold",
            "deploy",
            "Call compose_scaffold (omit output_dir) to materialize source stubs into the in-cwd .wxctl-scaffold dir; it returns a `config` with source paths rewritten to match — or, when the config references no source files, config_unchanged:true, in which case keep using your original config. Run the implementation prompts to fill the stubs in, then use the returned source-bearing config for every later step (plan, apply, test) — not the pre-scaffold config.",
            Some("compose_scaffold"),
            Some("After filling the stubs, re-run wxctl_validate on the returned source-bearing config — this run performs the source-file checks that step 4 skipped — and confirm valid:true before plan."),
        ),
        step(7, "plan", "deploy", "Call wxctl_plan on config.yaml and review the diff. The plan must be error-free.", Some("wxctl_plan"), Some("The plan must report no errors before apply.")),
        step(8, "apply", "deploy", "Call wxctl_apply with confirm:true to provision the resources.", Some("wxctl_apply"), Some("Requires an error-free wxctl_plan and confirm:true.")),
        step(9, "test", "deploy", "Call wxctl_test to run the config's kind:test suite and confirm the deployment is green.", Some("wxctl_test"), None),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identify_prompt_substitutes_catalog_and_input_no_placeholders() {
        let p = assemble_identify_prompt("HR chatbot with database").unwrap();
        assert!(p.contains("HR chatbot with database"));
        assert!(p.contains("agent"));
        assert!(p.contains("tool"));
        assert!(!p.contains("<RESOURCE_CATALOG>"));
        assert!(!p.contains("<USER_INPUT>"));
    }

    #[test]
    fn recipe_has_nine_tiered_steps_fix_loop_and_clarification() {
        let r = assemble_recipe("HR chatbot with employee handbook and database access").unwrap();
        let names: Vec<&str> = r.steps.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["identify", "paths", "generate", "validate_fix", "generate_tests", "scaffold", "plan", "apply", "test"]);
        let tiers: Vec<&str> = r.steps.iter().map(|s| s.tier.as_str()).collect();
        assert_eq!(tiers, ["config", "config", "config", "config", "config", "deploy", "deploy", "deploy", "deploy"]);
        assert_eq!(r.fix_loop.max_iterations, 3);
        assert!(r.gates.iter().any(|g| g.contains("plan must be error-free")));
        assert!(r.clarification.policy.contains("clarification_request"));
        assert!(r.identify_prompt.contains("HR chatbot with employee handbook and database access"));
        // AC5: per-step tool bindings (None where no discrete MCP tool).
        let tools: Vec<Option<&str>> = r.steps.iter().map(|s| s.tool.as_deref()).collect();
        assert_eq!(tools, [None, Some("compose_paths"), Some("compose_prompt"), Some("wxctl_validate"), Some("compose_prompt"), Some("compose_scaffold"), Some("wxctl_plan"), Some("wxctl_apply"), Some("wxctl_test")]);
        // AC5: gates present exactly on validate_fix, scaffold (re-validate), plan, apply.
        let gated: Vec<bool> = r.steps.iter().map(|s| s.gate.is_some()).collect();
        assert_eq!(gated, [false, false, false, true, false, true, true, true, false]);
        // AC5: the two top-level gates and step n ordering.
        assert_eq!(r.gates.len(), 2);
        assert!(r.gates[1].contains("confirm:true"));
        assert_eq!(r.steps.iter().map(|s| s.n).collect::<Vec<_>>(), [1, 2, 3, 4, 5, 6, 7, 8, 9]);
        // AC5: an action sample survives verbatim.
        assert!(r.steps[8].action.contains("wxctl_test to run the config's kind:test suite"));
    }

    #[test]
    fn with_max_tier_filters_to_config_and_is_a_noop_otherwise() {
        let full = assemble_recipe("HR chatbot with database").unwrap();
        // `None` and `deploy` are no-ops — the full nine-step recipe survives.
        assert_eq!(full.clone().with_max_tier(None).unwrap().steps.len(), 9);
        assert_eq!(full.clone().with_max_tier(Some("deploy")).unwrap().steps.len(), 9);
        // `config` keeps exactly the five config-tier steps, in recipe order.
        let cfg = full.clone().with_max_tier(Some("config")).unwrap();
        let names: Vec<&str> = cfg.steps.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["identify", "paths", "generate", "validate_fix", "generate_tests"]);
        assert!(cfg.steps.iter().all(|s| s.tier == "config"));
        // Cross-cutting policy is retained even in the filtered view.
        assert_eq!(cfg.fix_loop.max_iterations, 3);
        assert!(!cfg.gates.is_empty());
        assert!(cfg.clarification.policy.contains("clarification_request"));
        // An unknown tier is a hard error, not a silent passthrough.
        assert!(full.with_max_tier(Some("bogus")).unwrap_err().to_string().contains("config, deploy"));
    }
}
