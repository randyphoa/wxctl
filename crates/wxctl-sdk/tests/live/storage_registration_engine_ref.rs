use super::{COS_ENV_MAPPINGS, LiveTest, set_env_from_profile, short_id};
use wxctl_engine::OperationType;

/// Verify a `presto_engine.associated_catalogs` reference to a
/// `${storage_registration.X}` builds a valid DAG and plans both
/// resources as Create without tripping dependency-kind validation.
/// Plan-only — we don't actually provision the Presto engine (too heavy
/// for a default CI run). After the 2026-04-20 refactor, the
/// registration is assembled from a `storage_connection` + `s3_bucket`
/// pair rather than inline connection fields.
#[tokio::test]
async fn test_storage_registration_engine_ref_plans() -> anyhow::Result<()> {
    let missing = set_env_from_profile("wxd", COS_ENV_MAPPINGS)?;
    if let Some(field) = missing {
        eprintln!("SKIP test_storage_registration_engine_ref_plans: profile missing {field}");
        return Ok(());
    }

    let safe_id = short_id();
    let yaml = format!(
        r#"
kind: storage_connection
ref_name: cos_{safe_id}
type: ibm_cos
access_key: ${{env:WXCTL_TEST_COS_ACCESS_KEY}}
secret_key: ${{env:WXCTL_TEST_COS_SECRET_KEY}}
endpoint: ${{env:WXCTL_TEST_COS_ENDPOINT}}
instance_crn: ${{env:WXCTL_TEST_COS_CRN}}

---
kind: s3_bucket
ref_name: bucket_{safe_id}
connection: ${{storage_connection.cos_{safe_id}}}
name: ${{env:WXCTL_TEST_COS_BUCKET}}
region: eu-gb
storage_class: smart

---
kind: storage_registration
ref_name: wxctl_test_{safe_id}
display_name: wxctl-test-sr-{safe_id}
bucket: ${{s3_bucket.bucket_{safe_id}.name}}
associated_catalog:
  catalog_name: wxctl_test_{safe_id}
  catalog_type: iceberg

---
kind: presto_engine
ref_name: wxctl_test_eng_{safe_id}
display_name: wxctl-test-eng-{safe_id}
origin: native
associated_catalogs:
  - ${{storage_registration.wxctl_test_{safe_id}}}
configuration:
  size_config: starter
  coordinator:
    node_type: bx2.48x192
    quantity: 1
  worker:
    node_type: bx2.48x192
    quantity: 1
"#
    );

    LiveTest::new("test_storage_registration_engine_ref_plans")
        .profile("wxd")
        .timeout(120)
        .run(move |ctx| async move {
            ctx.phase("plan", async {
                let plan = ctx.plan(&yaml).await?;
                let kinds: Vec<String> = plan.operations.iter().map(|op| op.operation.key.kind.to_string()).collect();
                ctx.expect("plan", kinds.iter().any(|k| k == "storage_registration"), "storage_registration in plan", format!("{kinds:?}"))?;
                ctx.expect("plan", kinds.iter().any(|k| k == "presto_engine"), "presto_engine in plan", format!("{kinds:?}"))?;
                let sr = plan.operations.iter().find(|op| &*op.operation.key.kind == "storage_registration").expect("storage_registration op");
                let eng = plan.operations.iter().find(|op| &*op.operation.key.kind == "presto_engine").expect("presto_engine op");
                ctx.expect("plan", matches!(sr.operation.op_type, OperationType::Create), "Create", format!("{:?}", sr.operation.op_type))?;
                ctx.expect("plan", matches!(eng.operation.op_type, OperationType::Create), "Create", format!("{:?}", eng.operation.op_type))?;
                Ok(())
            })
            .await
        })
        .await
}
