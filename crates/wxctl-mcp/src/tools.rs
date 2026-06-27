//! Discovery-tool DTOs + backing logic. No profile, no network — these call the
//! schema loader directly (`wxctl_schema::list_kinds` / `explain_kind`), the same
//! path `wxctl resources -o json` / `wxctl explain -o json` use, so the JSON shapes
//! match those commands exactly.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use wxctl_schema::explain::{ExplainView, KindSummary};

/// Input for `wxctl_list_resource_kinds`. Both filters optional; mirror the
/// `wxctl resources --service / --deployment` filters.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListResourceKindsInput {
    /// Show only kinds belonging to this service (e.g. `watsonx_data`).
    #[serde(default)]
    pub service: Option<String>,
    /// Show only kinds available on this deployment: `saas` or `software`.
    #[serde(default)]
    pub deployment: Option<String>,
}

/// One row of the kind listing — same fields as `KindSummary`, re-declared so the
/// output carries a `JsonSchema` (the upstream type derives only `Serialize`).
#[derive(Debug, Serialize, JsonSchema)]
pub struct ResourceKindRow {
    pub kind: String,
    pub service: String,
    pub deployment_support: Vec<String>,
    pub summary: String,
}

impl From<KindSummary> for ResourceKindRow {
    fn from(k: KindSummary) -> Self {
        Self { kind: k.kind, service: k.service, deployment_support: k.deployment_support, summary: k.summary }
    }
}

/// Output for `wxctl_list_resource_kinds`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ListResourceKindsOutput {
    pub kinds: Vec<ResourceKindRow>,
}

/// Input for `wxctl_explain_kind`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExplainKindInput {
    /// Resource kind to describe (e.g. `presto_engine`, `tool`, `agent`).
    pub kind: String,
}

/// Transparent wrapper around the `ExplainView` JSON. Serializes to exactly the
/// inner value (byte-identical to `wxctl explain -o json`), but advertises a root
/// `type: object` JSON Schema — rmcp requires every tool `outputSchema` to have an
/// object root, and `serde_json::Value`'s own schema (permissive `{}`) does not.
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct ExplainKindOutput(pub serde_json::Value);

impl JsonSchema for ExplainKindOutput {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ExplainKindOutput".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        // The concrete `ExplainView` shape is documented by `wxctl explain -o json`;
        // a free-form object schema satisfies rmcp's object-root requirement without
        // re-deriving the full nested `ExplainView` schema in this crate.
        schemars::json_schema!({
            "type": "object",
            "description": "Full wxctl resource-kind descriptor: fields, types, defaults, enums, validation, nested sub-fields, dependencies, endpoints. Same JSON as `wxctl explain -o json`."
        })
    }
}

/// List resource kinds, optionally filtered by service / deployment. Pure schema
/// read — never touches a profile or the network. `Err(String)` carries an
/// already-formatted, agent-readable message (becomes an `isError` tool result).
pub fn list_resource_kinds(input: &ListResourceKindsInput) -> Result<ListResourceKindsOutput, String> {
    if let Some(d) = input.deployment.as_deref()
        && d != "saas"
        && d != "software"
    {
        return Err(format!("invalid deployment '{d}'. Valid values: saas, software."));
    }
    let kinds = wxctl_schema::list_kinds(input.service.as_deref(), input.deployment.as_deref()).into_iter().map(ResourceKindRow::from).collect();
    Ok(ListResourceKindsOutput { kinds })
}

/// Full structured descriptor for one kind — the exact value `wxctl explain -o json`
/// emits (`ExplainView`). Unknown kind → `Err` with the loader's message that lists
/// every valid kind. Pure schema read — no profile / network.
pub fn explain_kind(input: &ExplainKindInput) -> Result<ExplainView, String> {
    wxctl_schema::explain_kind(&input.kind).map_err(|e| format!("{e:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_resource_kinds_unfiltered_filtered_and_rejects_unknown() {
        // Unfiltered: full catalog; rows carry the same fields `wxctl resources -o json` exposes.
        let all = list_resource_kinds(&ListResourceKindsInput { service: None, deployment: None }).expect("listing succeeds");
        assert!(!all.kinds.is_empty(), "catalog is non-empty");
        assert!(all.kinds.iter().all(|r| !r.kind.is_empty() && !r.service.is_empty()), "every row has a kind + service");

        // `deployment` filter narrows the catalog and only yields kinds supporting that deployment.
        let saas = list_resource_kinds(&ListResourceKindsInput { service: None, deployment: Some("saas".to_string()) }).unwrap();
        assert!(saas.kinds.len() <= all.kinds.len(), "filtering does not grow the set");
        assert!(saas.kinds.iter().all(|r| r.deployment_support.iter().any(|d| d == "saas")), "every row supports saas");

        // An out-of-range deployment value is rejected with an actionable message.
        let err = list_resource_kinds(&ListResourceKindsInput { service: None, deployment: Some("onprem".to_string()) }).unwrap_err();
        assert!(err.contains("saas") && err.contains("software"), "names the valid values");
    }

    /// `explain_kind` returns the structured view for a real kind, and an unknown
    /// kind yields the loader's valid-kinds message.
    #[test]
    fn explains_known_and_reports_unknown() {
        let v = explain_kind(&ExplainKindInput { kind: "s3_bucket".to_string() }).expect("known kind resolves");
        assert_eq!(v.kind, "s3_bucket");
        let result = explain_kind(&ExplainKindInput { kind: "definitely_not_a_kind".to_string() });
        assert!(result.is_err(), "unknown kind should fail");
        let err = result.err().unwrap();
        assert!(err.contains("unknown kind") && err.contains("Valid kinds"), "lists valid kinds on miss");
    }
}
