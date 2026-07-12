//! Data-need detection — the wasm-safe, schema-driven "what data does this config
//! need" pass. Reads `wxctl_schema::PATH_FIELDS` (is_path inference) + `SYNTH_FIELDS`
//! (marker override) and emits a `DataNeed` per data-bearing field. Pure compute: no
//! FS/env/network. Delivery is `Fixture` for file kinds and `Embedded` for the runtime
//! train/serve kinds (`wml_function`/`ai_service`), keyed by `EMBEDDED_KINDS` membership.

use anyhow::{Context, Result};
use serde_json::Value;
use wxctl_schema::resource::RawResource;

/// How a resource's data need is fulfilled. `Fixture` = a file synthesized into the
/// scaffold dir; `Embedded` = data-synthesizing source code written back to the
/// resource's `source_path` (the runtime train/serve kinds).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// A file synthesized into the scaffold dir.
    Fixture,
    /// Source code that synthesizes its data in-code at run time (`wml_function`,
    /// `ai_service`). Written back to the resource's `source_path`.
    Embedded,
}

/// Whether the need was inferred from `is_path` or forced by a `synthesize` marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DataSource {
    Inferred,
    Marker,
}

/// Declared/derived shape of the data a field needs.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DataShape {
    /// File format / extension hint (e.g. "csv", "json", "txt"); `None` = unknown.
    pub format: Option<String>,
}

/// One resource field that needs synthesized data.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DataNeed {
    pub ref_name: String,
    pub kind: String,
    /// The leaf field needing data (e.g. `source_path`, `glossary_csv`, `file_paths`).
    pub field: String,
    /// Parent object/array field when the field is nested one level (e.g. `source`).
    pub parent: Option<String>,
    pub shape: DataShape,
    pub delivery: Delivery,
    pub source: DataSource,
}

type PathField = (&'static str, &'static str, Option<&'static str>);
type SynthField = (&'static str, &'static str, Option<&'static str>, bool, Option<&'static str>);

/// Runtime train/serve kinds whose data need is delivered as embedded,
/// data-synthesizing source code (not a fixture file). Their `is_path` `source_path`
/// field yields a `Delivery::Embedded` need instead of being skipped.
const EMBEDDED_KINDS: [&str; 2] = ["wml_function", "ai_service"];

/// Kinds excluded from data detection entirely: `tool` (its body is owned by the
/// implementation pass), `toolkit` (a server), `knowledge_base` (materialized by its
/// own scaffold arm).
const EXCLUDED_KINDS: [&str; 3] = ["tool", "toolkit", "knowledge_base"];

/// `Embedded` for the runtime train/serve kinds, else `Fixture`. Delivery is a
/// property of the kind's reconciler, not the scenario (see spec Q3).
fn delivery_for(kind: &str) -> Delivery {
    if EMBEDDED_KINDS.contains(&kind) { Delivery::Embedded } else { Delivery::Fixture }
}

/// Detect every data-bearing field in a parsed config. Pure schema read over the
/// generated `PATH_FIELDS` + `SYNTH_FIELDS`.
pub fn detect_data_needs(resources: &[RawResource]) -> Vec<DataNeed> {
    detect_needs(resources, wxctl_schema::PATH_FIELDS, wxctl_schema::SYNTH_FIELDS)
}

/// Core detector with injectable metadata (unit tests pass synthetic field tables).
fn detect_needs(resources: &[RawResource], path_fields: &[PathField], synth_fields: &[SynthField]) -> Vec<DataNeed> {
    let mut out = Vec::new();
    for r in resources {
        if EXCLUDED_KINDS.contains(&r.kind.as_str()) {
            continue;
        }
        let ref_name = r.data.get("ref_name").and_then(|v| v.as_str()).unwrap_or("unnamed").to_string();

        // Inference: is_path fields present and not suppressed by a marker.
        for (_k, field, parent) in path_fields.iter().filter(|(k, ..)| *k == r.kind) {
            if suppressed(synth_fields, &r.kind, field, *parent) {
                continue;
            }
            if !field_present(&r.data, field, *parent) {
                continue;
            }
            let shape = force_shape(synth_fields, &r.kind, field, *parent).map(str::to_string).map(|f| DataShape { format: Some(f) }).unwrap_or_else(|| derive_shape(field, &r.data, *parent));
            out.push(DataNeed { ref_name: ref_name.clone(), kind: r.kind.clone(), field: field.to_string(), parent: parent.map(str::to_string), shape, delivery: delivery_for(&r.kind), source: DataSource::Inferred });
        }

        // Marker override: synthesize:true fields not already covered by is_path.
        for (k, field, parent, syn, shape) in synth_fields.iter().filter(|(k, ..)| *k == r.kind) {
            if !*syn {
                continue;
            }
            if path_fields.iter().any(|(pk, pf, pp)| pk == k && pf == field && pp == parent) {
                continue;
            }
            if !field_present(&r.data, field, *parent) {
                continue;
            }
            let shape = shape.map(str::to_string).map(|f| DataShape { format: Some(f) }).unwrap_or_else(|| derive_shape(field, &r.data, *parent));
            out.push(DataNeed { ref_name: ref_name.clone(), kind: r.kind.clone(), field: field.to_string(), parent: parent.map(str::to_string), shape, delivery: delivery_for(&r.kind), source: DataSource::Marker });
        }
    }
    out
}

/// True when a `synthesize: false` marker suppresses this (kind, field, parent).
fn suppressed(synth: &[SynthField], kind: &str, field: &str, parent: Option<&str>) -> bool {
    synth.iter().any(|(k, f, p, syn, _)| *k == kind && *f == field && *p == parent && !*syn)
}

/// The `synth_shape` hint from a matching `synthesize: true` marker, if any.
fn force_shape<'a>(synth: &'a [SynthField], kind: &str, field: &str, parent: Option<&str>) -> Option<&'a str> {
    synth.iter().find(|(k, f, p, syn, _)| *k == kind && *f == field && *p == parent && *syn).and_then(|(.., shape)| *shape)
}

/// Whether the data field carries a value: for `parent=None` a non-empty string;
/// for an object parent a non-empty string at `data[parent][field]`; for an array
/// parent a non-empty array at `data[parent]`.
fn field_present(data: &Value, field: &str, parent: Option<&str>) -> bool {
    match parent {
        None => data.get(field).and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty()),
        Some(p) => match data.get(p) {
            Some(Value::Array(items)) => !items.is_empty(),
            Some(obj @ Value::Object(_)) => obj.get(field).and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty()),
            _ => false,
        },
    }
}

/// Derive a shape from schema-agnostic signals: the field-name suffix (`*_csv`),
/// then the current path value's extension, then a sibling format field, else unknown.
fn derive_shape(field: &str, data: &Value, parent: Option<&str>) -> DataShape {
    if let Some(ext) = field.rsplit('_').next().filter(|s| matches!(*s, "csv" | "json" | "txt")) {
        return DataShape { format: Some(ext.to_string()) };
    }
    if parent.is_none()
        && let Some(v) = data.get(field).and_then(|v| v.as_str())
        && let Some(ext) = v.rsplit('.').next().filter(|e| !e.is_empty() && *e != v)
    {
        return DataShape { format: Some(ext.to_ascii_lowercase()) };
    }
    for sib in ["file_format", "file_type", "mime_type", "content_type"] {
        if let Some(v) = data.get(sib).and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            let fmt = v.rsplit(['/', '.']).next().unwrap_or(v).to_ascii_lowercase();
            return DataShape { format: Some(fmt) };
        }
    }
    DataShape { format: None }
}

/// Parse multi-document config YAML into `RawResource`s without `${env:…}` expansion.
/// Wasm-safe (no process env). The detection + data-prompt entry points share it.
pub fn parse_resources(yaml: &str) -> Result<Vec<RawResource>> {
    use serde::Deserialize as _;
    let mut resources = Vec::new();
    for document in serde_norway::Deserializer::from_str(yaml) {
        let value = serde_norway::Value::deserialize(document).context("parse config document")?;
        if value.is_null() {
            continue;
        }
        resources.push(serde_norway::from_value(value).context("deserialize config resource")?);
    }
    Ok(resources)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn res(kind: &str, data: serde_json::Value) -> RawResource {
        RawResource { kind: kind.to_string(), data }
    }

    // Synthetic field tables so detection logic is testable without production schemas.
    const PATHS: &[PathField] = &[("data_asset", "source_path", None), ("sal_glossary", "glossary_csv", None), ("tool", "source_path", None)];
    const SYNTH: &[SynthField] = &[("s3_object", "path", None, true, Some("csv")), ("data_asset", "source_path", None, false, None)];

    #[test]
    fn agent_only_config_has_no_data_need() {
        let needs = detect_needs(&[res("agent", json!({"ref_name": "a", "name": "a"}))], PATHS, SYNTH);
        assert!(needs.is_empty());
    }

    #[test]
    fn inferred_is_path_csv_field_is_detected_with_shape() {
        let needs = detect_needs(&[res("sal_glossary", json!({"ref_name": "g", "glossary_csv": "terms.csv"}))], PATHS, SYNTH);
        assert_eq!(needs.len(), 1);
        let n = &needs[0];
        assert_eq!((n.kind.as_str(), n.field.as_str(), n.source, n.delivery), ("sal_glossary", "glossary_csv", DataSource::Inferred, Delivery::Fixture));
        assert_eq!(n.shape.format.as_deref(), Some("csv"));
    }

    #[test]
    fn suppress_marker_removes_an_inferred_need() {
        // data_asset.source_path is is_path but marked synthesize:false → no need.
        let needs = detect_needs(&[res("data_asset", json!({"ref_name": "d", "source_path": "x.csv"}))], PATHS, SYNTH);
        assert!(needs.is_empty());
    }

    #[test]
    fn synthesize_marker_forces_a_need_inference_misses() {
        // s3_object.path is NOT is_path; the marker forces a Marker-sourced need with its shape.
        let needs = detect_needs(&[res("s3_object", json!({"ref_name": "o", "path": "blob"}))], PATHS, SYNTH);
        assert_eq!(needs.len(), 1);
        assert_eq!((needs[0].source, needs[0].shape.format.as_deref()), (DataSource::Marker, Some("csv")));
    }

    #[test]
    fn excluded_code_kind_is_skipped() {
        let needs = detect_needs(&[res("tool", json!({"ref_name": "t", "source_path": "t"}))], PATHS, SYNTH);
        assert!(needs.is_empty(), "code kinds are excluded from Phase-1 fixture detection");
    }

    #[test]
    fn real_statics_detect_data_asset_and_s3_object() {
        // Over the production PATH_FIELDS/SYNTH_FIELDS: data_asset (is_path) + s3_object (marker).
        let cfg = "kind: data_asset\nref_name: customers\nname: customers\nsource_path: customers.csv\n---\nkind: s3_object\nref_name: blob\nbucket: b\nkey: k\npath: sample.csv\n";
        let needs = detect_data_needs(&parse_resources(cfg).unwrap());
        let kinds: Vec<&str> = needs.iter().map(|n| n.kind.as_str()).collect();
        assert!(kinds.contains(&"data_asset") && kinds.contains(&"s3_object"), "got {kinds:?}");
    }

    #[test]
    fn embedded_kind_yields_embedded_delivery_not_skipped() {
        // wml_function.source_path is is_path → an Embedded need (was skipped in Phase 1).
        let paths: &[PathField] = &[("wml_function", "source_path", None)];
        let needs = detect_needs(&[res("wml_function", json!({"ref_name": "scorer", "source_path": "score.py"}))], paths, &[]);
        assert_eq!(needs.len(), 1);
        let n = &needs[0];
        assert_eq!((n.kind.as_str(), n.field.as_str(), n.delivery), ("wml_function", "source_path", Delivery::Embedded));
    }

    #[test]
    fn real_statics_detect_wml_function_as_embedded() {
        // Over production PATH_FIELDS: wml_function.source_path → an Embedded need.
        let cfg = "kind: wml_function\nref_name: scorer\nname: scorer\nsource_path: score.py\n";
        let needs = detect_data_needs(&parse_resources(cfg).unwrap());
        let n = needs.iter().find(|n| n.kind == "wml_function" && n.field == "source_path").expect("wml_function source_path need");
        assert_eq!(n.delivery, Delivery::Embedded);
    }

    #[test]
    fn detects_three_or_more_kinds_over_real_schema() {
        // AC2: schema-driven detection flags >=3 distinct kinds with no per-kind hardcoding,
        // shapes derived from the schema (csv). Uses the real generated field tables.
        let cfg = "kind: data_asset\nref_name: customers\nname: customers\nsource_path: customers.csv\n---\nkind: sal_glossary\nref_name: terms\nname: terms\nglossary_csv: terms.csv\n---\nkind: ingestion_job\nref_name: ingest\nname: ingest\nsource:\n  file_paths: data.csv\n";
        let needs = detect_data_needs(&parse_resources(cfg).unwrap());
        let kinds: std::collections::HashSet<&str> = needs.iter().map(|n| n.kind.as_str()).collect();
        assert!(kinds.len() >= 3, "expected >=3 distinct data-bearing kinds, got {kinds:?}");
        for k in ["data_asset", "sal_glossary", "ingestion_job"] {
            assert!(kinds.contains(k), "missing schema-driven need for {k}: {kinds:?}");
        }
        // Shapes for the three named kinds derive from the schema as csv (not hardcoded).
        for k in ["data_asset", "sal_glossary", "ingestion_job"] {
            let n = needs.iter().find(|n| n.kind == k).unwrap();
            assert_eq!(n.shape.format.as_deref(), Some("csv"), "{k} shape should derive to csv: {n:?}");
        }
    }
}
