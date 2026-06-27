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

/// One-shot cleanup of the three known orphan presto engines from the
/// 2026-04-19 test run. IDs are hardcoded to keep blast radius minimal.
#[tokio::test]
async fn cleanup_known_orphan_presto_engines() -> anyhow::Result<()> {
    init_tracing();
    let Some(factory) = wxd_factory()? else {
        eprintln!("SKIP: 'wxd' profile not configured");
        return Ok(());
    };
    let client = factory.create_client("watsonx_data")?;

    let targets = ["presto147", "presto359", "presto803"];
    for id in targets {
        eprintln!("DELETE /v3/presto_engines/{id}");
        let spec = RequestSpec::new(Method::DELETE, format!("/v3/presto_engines/{id}")).body(BodyKind::None);
        match client.execute::<Value>("orphan_cleanup", spec).await {
            Ok(_) => eprintln!("  ok"),
            Err(e) => eprintln!("  ERROR: {e}"),
        }
    }
    Ok(())
}
