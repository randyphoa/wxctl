//! `pa_user` handler — a TM1 user's group memberships are set via the OData `Groups@odata.bind`
//! key (dotted -> inexpressible as a declared `api_field`, dropped by the default materializer:
//! docs/troubleshoot/pre-create-body-reshape-dropped-fix.md), so this handler OWNS the create
//! POST and builds the bind body. The `password` is write-only: it rides the create body as
//! `Password` and the handler marks that path sensitive on the request so it is redacted at
//! emission (redaction reads `RequestSpec.sensitive_paths` in wxctl-core http.rs — AC8). It is
//! excluded from `state_fields`, so it never participates in state comparison or a PATCH.

use anyhow::{Result, anyhow};
use reqwest::Method;
use serde_json::{Map, Value, json};
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::{BodyKind, HttpClient, RequestSpec};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

pub struct UserHandler;

impl ResourceHandler for UserHandler {
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let body = build_user_create_body(resource)?;
            // The emitted body key is PascalCase `Password` (the OData property), not the schema
            // field `password`; redact_by_schema matches the emitted key, so the sensitive path is
            // `Password`. (The keyword heuristic also catches it; this is the precise guard.)
            let spec = RequestSpec::new(Method::POST, endpoint).body(BodyKind::Json(body)).sensitive_paths(vec!["Password".to_string()]);
            let response: Value = client.execute(operation_id, spec).await?;
            Ok(HookOutcome::Handled(response))
        })
    }
}

/// Build the `POST /Users` body. `groups` names become `Groups@odata.bind: ["Groups('<name>')", ...]`;
/// `password` rides `Password` (write-only); the remaining scalars ride their PascalCase keys when
/// present. Names exclude `'` at schema validation, so no OData escaping is needed here.
fn build_user_create_body(resource: &Value) -> Result<Value> {
    let name = resource.get("name").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("pa_user requires a 'name' field"))?;
    let mut body = Map::new();
    body.insert("Name".to_string(), json!(name));
    if let Some(p) = resource.get("password").and_then(|v| v.as_str()) {
        body.insert("Password".to_string(), json!(p));
    }
    if let Some(f) = resource.get("friendly_name").and_then(|v| v.as_str()) {
        body.insert("FriendlyName".to_string(), json!(f));
    }
    if let Some(t) = resource.get("type").and_then(|v| v.as_str()) {
        body.insert("Type".to_string(), json!(t));
    }
    if let Some(e) = resource.get("enabled").and_then(|v| v.as_bool()) {
        body.insert("Enabled".to_string(), json!(e));
    }
    if let Some(groups) = resource.get("groups").and_then(|v| v.as_array()) {
        let binds: Vec<Value> = groups.iter().filter_map(|g| g.as_str()).map(|g| Value::String(format!("Groups('{g}')"))).collect();
        if binds.len() != groups.len() {
            return Err(anyhow!("pa_user 'groups' must be an array of group-name strings"));
        }
        body.insert("Groups@odata.bind".to_string(), Value::Array(binds));
    }
    Ok(Value::Object(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Pure-function unit tests of the body-builder (no I/O).
    #[test]
    fn build_user_body_binds_groups_and_carries_password() {
        let resource = json!({"name": "alice", "password": "s3cr3t", "type": "User", "enabled": true, "groups": ["Finance", "Admin"]});
        let body = build_user_create_body(&resource).expect("body");
        assert_eq!(body.get("Name").and_then(|v| v.as_str()), Some("alice"));
        assert_eq!(body.get("Password").and_then(|v| v.as_str()), Some("s3cr3t"));
        assert_eq!(body.get("Groups@odata.bind").unwrap(), &json!(["Groups('Finance')", "Groups('Admin')"]));
        assert_eq!(body.get("Enabled").and_then(|v| v.as_bool()), Some(true));
    }

    #[test]
    fn build_user_body_omits_absent_optionals() {
        let body = build_user_create_body(&json!({"name": "bob"})).expect("body");
        assert_eq!(body.get("Name").and_then(|v| v.as_str()), Some("bob"));
        assert!(!body.as_object().unwrap().contains_key("Password"));
        assert!(!body.as_object().unwrap().contains_key("Groups@odata.bind"));
    }

    #[test]
    fn build_user_body_requires_name() {
        assert!(build_user_create_body(&json!({"password": "x"})).is_err());
    }
}
