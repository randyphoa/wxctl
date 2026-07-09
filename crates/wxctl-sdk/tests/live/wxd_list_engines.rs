use super::init_tracing;
use reqwest::Method;
use serde_json::Value;
use std::path::PathBuf;
use wxctl_core::client::{BodyKind, RequestSpec};
use wxctl_core::{ClientFactory, ConcurrencyConfig};

fn wxd_factory() -> anyhow::Result<Option<ClientFactory>> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    let path: PathBuf = home.join(".wxctl/test_profiles.json");
    if !path.exists() {
        return Ok(None);
    }
    let path_str = path.to_str().ok_or_else(|| anyhow::anyhow!("invalid path"))?;
    let cfg = ConcurrencyConfig::from_env();
    match ClientFactory::new("wxd", Some(path_str), &cfg) {
        Ok(f) => Ok(Some(f)),
        Err(e) if e.to_string().contains("not found") => Ok(None),
        Err(e) => Err(e),
    }
}

async fn audit_engines(list_endpoint: &str, envelope_key: &str, label: &str) -> anyhow::Result<()> {
    init_tracing();
    let Some(factory) = wxd_factory()? else {
        eprintln!("SKIP: 'wxd' profile not configured");
        return Ok(());
    };
    let client = factory.create_client("watsonx_data")?;
    let spec = RequestSpec::new(Method::GET, list_endpoint).body(BodyKind::None);
    let resp: Value = client.execute("audit", spec).await?;

    let engines = resp.get(envelope_key).and_then(|v| v.as_array()).cloned().unwrap_or_default();
    eprintln!("\n=== {label} ({}) ===", engines.len());
    for e in &engines {
        let id = e.get("id").and_then(|v| v.as_str()).unwrap_or("?");
        let name = e.get("display_name").and_then(|v| v.as_str()).unwrap_or("?");
        let status = e.get("status").and_then(|v| v.as_str()).unwrap_or("?");
        eprintln!("  {id}  status={status}  display_name={name}");
    }
    eprintln!("==========================");
    Ok(())
}

/// Read-only audit: list all presto engines and print id/display_name/status.
#[tokio::test]
async fn audit_list_presto_engines() -> anyhow::Result<()> {
    audit_engines("/v3/presto_engines?limit=100", "presto_engines", "Presto engines").await
}

#[tokio::test]
async fn audit_list_spark_engines() -> anyhow::Result<()> {
    audit_engines("/v3/spark_engines?limit=100", "spark_engines", "Spark engines").await
}
