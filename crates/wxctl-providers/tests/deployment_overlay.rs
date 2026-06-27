//! Regression: software-5.3 deployment overlays must not strip the `/v3/`
//! prefix from watsonx.data engine paths. The lakehouse v3 API surface is
//! identical between SaaS and Software Hub — both serve `/v3/spark_engines`
//! and `/v3/presto_engines`. Earlier overrides at `spark_engine.yaml` and
//! `presto_engine.yaml` set `base_path: /spark_engines` (no `/v3/`), which
//! returns 404 against a real Software 5.3 install (verified against
//! a CP4D 5.4.2 / watsonx.data 2.2.2 deployment).

use std::str::FromStr;
use wxctl_core::schema::effective_definition;
use wxctl_core::types::Deployment;
use wxctl_providers::load_all_schemas;

#[test]
fn watsonx_data_engine_paths_match_across_deployments() {
    let schemas = load_all_schemas().expect("load schemas");
    let saas = Deployment::from_str("saas").expect("parse saas");
    let sw = Deployment::from_str("software-5.3.0").expect("parse software-5.3.0");

    for kind in ["spark_engine", "presto_engine"] {
        let schema = schemas.iter().find(|s| s.resource.kind == kind).unwrap_or_else(|| panic!("schema {kind} not found"));

        let merged_saas = effective_definition(&schema.resource, &saas).expect("merge saas");
        let merged_sw = effective_definition(&schema.resource, &sw).expect("merge software-5.3.0");

        let v3_prefix = format!("/v3/{kind}s");
        assert!(merged_saas.api.base_path.starts_with(&v3_prefix), "{kind}: saas base_path should start with {v3_prefix}, got {:?}", merged_saas.api.base_path);
        assert!(merged_sw.api.base_path.starts_with(&v3_prefix), "{kind}: software-5.3 base_path should start with {v3_prefix} (matches SaaS — same lakehouse v3 surface), got {:?}", merged_sw.api.base_path);

        assert_eq!(merged_saas.api.base_path, merged_sw.api.base_path, "{kind}: base_path must match across SaaS and Software 5.3 — the watsonx.data v3 API surface is identical");
        assert_eq!(merged_saas.api.list_endpoint, merged_sw.api.list_endpoint, "{kind}: list_endpoint must match across SaaS and Software 5.3");
    }
}
