//! `compose paths` core — closure + bridges → recommended path(s), serialized to
//! the `compose/v1` paths YAML. Takes already-joined config YAML (the CLI joins
//! multiple `-f` files; the MCP tool passes one document) + a deployment string.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::str::FromStr;
use wxctl_schema::dependency_graph::{Constraint, EdgeType, ResolvedEdge, ResolvedPath, SchemaDependencyGraph};
use wxctl_schema::deployment::Deployment;
use wxctl_schema::resource::RawResource;

/// Input to the paths core: raw resource-list / partial-config YAML + deployment.
pub struct PathsInput<'a> {
    pub content: &'a str,
    pub deployment: &'a str,
}

/// Resolve the recommended path(s) and return the `compose/v1` paths YAML.
pub fn resolve_paths(input: PathsInput<'_>) -> Result<String> {
    let deployment = Deployment::from_str(input.deployment).with_context(|| format!("invalid --deployment '{}': expected 'saas' or 'software'", input.deployment))?;
    let parsed = parse_input(input.content)?;
    let kind_refs: Vec<&str> = parsed.kinds.iter().map(|s| s.as_str()).collect();
    let closure = if let Some(ref owned_fv) = parsed.field_values {
        let fv = to_str_map(owned_fv);
        SchemaDependencyGraph::compute_closure_from_kinds_with_config(&kind_refs, Some(&fv), &deployment)
    } else {
        SchemaDependencyGraph::compute_closure_from_kinds_with_config(&kind_refs, None, &deployment)
    };
    let bridges = closure.find_bridges(&deployment);
    let paths = closure.enumerate_paths(&bridges, &kind_refs);
    serialize_paths(&deployment, &paths)
}

// ── Input parsing ──

#[derive(Deserialize)]
struct ResourceListItem {
    kind: String,
    #[serde(default)]
    #[allow(dead_code)]
    reason: Option<String>,
}

#[derive(Deserialize)]
struct ResourceList {
    resources: Vec<ResourceListItem>,
}

struct ParsedInput {
    kinds: Vec<String>,
    field_values: Option<HashMap<String, HashMap<String, Vec<String>>>>,
}

fn parse_input(content: &str) -> Result<ParsedInput> {
    if let Ok(list) = serde_norway::from_str::<ResourceList>(content)
        && !list.resources.is_empty()
    {
        return Ok(ParsedInput { kinds: list.resources.into_iter().map(|r| r.kind).collect(), field_values: None });
    }

    // Parse the multi-doc config YAML directly into RawResource (wxctl-schema, wasm-safe).
    // No `${env:VAR}` interpolation: compose-paths only extracts kinds + string field-values
    // to pick a DAG path, and the wasm/MCP surfaces carry no process-env context.
    let mut resources: Vec<RawResource> = Vec::new();
    for document in serde_norway::Deserializer::from_str(content) {
        let value = serde_norway::Value::deserialize(document).context("Failed to parse input as resource config")?;
        let resource: RawResource = serde_norway::from_value(value).context("Failed to parse input as resource config")?;
        resources.push(resource);
    }
    if resources.is_empty() {
        anyhow::bail!("No resources found in input. Expected resource list or partial config YAML.");
    }

    let mut kinds = Vec::new();
    let mut field_values: HashMap<String, HashMap<String, Vec<String>>> = HashMap::new();

    for resource in &resources {
        kinds.push(resource.kind.clone());
        let fields = field_values.entry(resource.kind.clone()).or_default();
        collect_string_values(&resource.data, "", fields);
    }

    Ok(ParsedInput { kinds, field_values: Some(field_values) })
}

fn collect_string_values(value: &Value, path: &str, result: &mut HashMap<String, Vec<String>>) {
    match value {
        Value::String(s) => add_to_ancestors(path, s, result),
        Value::Array(arr) => {
            for item in arr {
                if let Value::String(s) = item {
                    add_to_ancestors(path, s, result);
                } else {
                    collect_string_values(item, path, result);
                }
            }
        }
        Value::Object(map) => {
            for (key, val) in map {
                let child_path = if path.is_empty() { key.clone() } else { format!("{}.{}", path, key) };
                collect_string_values(val, &child_path, result);
            }
        }
        _ => {}
    }
}

fn add_to_ancestors(path: &str, value: &str, result: &mut HashMap<String, Vec<String>>) {
    if path.is_empty() {
        return;
    }
    result.entry(path.to_string()).or_default().push(value.to_string());
    let mut end = path.len();
    while let Some(pos) = path[..end].rfind('.') {
        result.entry(path[..pos].to_string()).or_default().push(value.to_string());
        end = pos;
    }
}

fn to_str_map(owned: &HashMap<String, HashMap<String, Vec<String>>>) -> HashMap<&str, HashMap<&str, Vec<&str>>> {
    owned
        .iter()
        .map(|(kind, fields)| {
            let borrowed_fields: HashMap<&str, Vec<&str>> = fields.iter().map(|(field, values)| (field.as_str(), values.iter().map(|v| v.as_str()).collect())).collect();
            (kind.as_str(), borrowed_fields)
        })
        .collect()
}

// ── Output serialization (compose/v1) ──

#[derive(Serialize)]
struct PathsOutput {
    format: String,
    deployment: String,
    paths: Vec<PathOutput>,
}

#[derive(Serialize)]
struct PathOutput {
    name: String,
    #[serde(skip_serializing_if = "is_false")]
    recommended: bool,
    resources: Vec<ResourceOutput>,
    edges: Vec<EdgeOutput>,
}

#[derive(Serialize)]
struct ResourceOutput {
    kind: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    constraints: Vec<ConstraintOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    added_by: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    field_mappings: Vec<FieldOutput>,
}

#[derive(Serialize)]
struct ConstraintOutput {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    one_of: Option<Vec<String>>,
}

#[derive(Serialize)]
struct EdgeOutput {
    source: String,
    target: String,
    edge_type: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    field: String,
}

#[derive(Serialize)]
struct FieldOutput {
    name: String,
    value: String,
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn serialize_constraint(c: &Constraint) -> ConstraintOutput {
    if c.is_single() { ConstraintOutput { name: c.name.to_string(), value: Some(c.values[0].to_string()), one_of: None } } else { ConstraintOutput { name: c.name.to_string(), value: None, one_of: Some(c.values.iter().map(|v| v.to_string()).collect()) } }
}

fn serialize_edge(edge: &ResolvedEdge) -> EdgeOutput {
    EdgeOutput {
        source: edge.source.to_string(),
        target: edge.target.to_string(),
        edge_type: match &edge.edge_type {
            EdgeType::Reference => "reference".to_string(),
            EdgeType::Bridge(name) => format!("bridge:{}", name),
        },
        field: edge.field.clone(),
    }
}

fn serialize_paths(deployment: &Deployment, paths: &[ResolvedPath]) -> Result<String> {
    let output = PathsOutput {
        format: "compose/v1".to_string(),
        deployment: deployment.to_string(),
        paths: paths
            .iter()
            .map(|p| PathOutput {
                name: p.name.clone(),
                recommended: p.recommended,
                resources: p
                    .resources
                    .iter()
                    .map(|r| ResourceOutput {
                        kind: r.kind.to_string(),
                        constraints: r.constraints.iter().map(serialize_constraint).collect(),
                        added_by: r.added_by.clone(),
                        field_mappings: r.field_mappings.iter().map(|(name, value)| FieldOutput { name: name.to_string(), value: value.clone() }).collect(),
                    })
                    .collect(),
                edges: p.edges.iter().map(serialize_edge).collect(),
            })
            .collect(),
    };

    serde_norway::to_string(&output).context("Failed to serialize resolved paths")
}

#[cfg(test)]
mod tests {
    use super::*;
    use wxctl_schema::dependency_graph::{Constraint, EdgeType, ResolvedEdge, ResolvedPath, ResolvedResource};

    #[test]
    fn test_parse_input_resource_list_and_partial_config() {
        // `resources:` list form → kinds only, no field values.
        let parsed = parse_input("resources:\n  - kind: agent\n    reason: needed\n  - kind: tool\n").unwrap();
        assert_eq!(parsed.kinds, vec!["agent", "tool"]);
        assert!(parsed.field_values.is_none());

        // Partial multi-doc config form → kinds + per-kind field values harvested.
        let parsed = parse_input("kind: agent\nref_name: my_agent\nllm: groq/openai/gpt-oss-120b\n---\nkind: tool\nref_name: my_tool\n").unwrap();
        assert_eq!(parsed.kinds, vec!["agent", "tool"]);
        let fv = parsed.field_values.expect("partial config carries field values");
        assert_eq!(fv.get("agent").unwrap().get("llm").unwrap(), &vec!["groq/openai/gpt-oss-120b"]);
    }

    #[test]
    fn test_serialize_paths_header_constraints_and_deployment_label() {
        // SaaS path with constraints → compose/v1 header + one_of (multi) / value (single) constraint forms.
        let paths = vec![ResolvedPath {
            name: "database_access".to_string(),
            recommended: true,
            resources: vec![ResolvedResource { kind: "common_core_connection", constraints: vec![Constraint::one_of("datasource_type", vec!["postgres", "mysql"]), Constraint::single("connection_type", "key_value_creds")], added_by: None, field_mappings: vec![] }],
            edges: vec![ResolvedEdge { source: "orchestrate_connection", target: "common_core_connection", edge_type: EdgeType::Bridge("database_access".to_string()), field: String::new() }],
        }];
        let yaml = serialize_paths(&Deployment::Saas, &paths).unwrap();
        assert!(yaml.contains("format: compose/v1"), "must carry compose/v1 header");
        assert!(yaml.contains("deployment: saas"));
        assert!(yaml.contains("recommended: true"));
        assert!(yaml.contains("one_of"), "multi-value constraint must serialize as one_of");
        assert!(yaml.contains("value: key_value_creds"), "single-value constraint must serialize as value");

        // Software deployment string round-trips into the `deployment:` label verbatim.
        let paths = vec![ResolvedPath { name: "default".to_string(), recommended: true, resources: vec![], edges: vec![] }];
        let yaml = serialize_paths(&Deployment::from_str("software-5.3.0").unwrap(), &paths).unwrap();
        assert!(yaml.contains("deployment: software-5.3.0"));
    }

    #[test]
    fn unknown_deployment_errors() {
        let err = resolve_paths(PathsInput { content: "resources:\n  - kind: agent\n", deployment: "frobnicate" }).unwrap_err();
        assert!(err.to_string().contains("invalid --deployment"), "got: {}", err);
    }
}
