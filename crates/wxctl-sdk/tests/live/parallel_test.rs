use super::{LiveTest, StderrTestObserver, load_fixture, short_id};
use std::sync::Arc;
use wxctl_core::Config;
use wxctl_sdk::TestObserver;

/// Deploy simple_chain, then run 3 parallel test cases via client.test().
/// Validates the full SDK test() code path: plan → resolve agents → parallel chat
/// execution → result collection.
#[tokio::test]
async fn test_parallel_test_execution() -> anyhow::Result<()> {
    let test_id = short_id();
    let yaml = load_fixture("parallel_test.yaml", &test_id);
    let real_yaml = load_fixture("simple_chain.yaml", &test_id);

    LiveTest::new("test_parallel_test_execution")
        .timeout(600)
        .guard_yaml(real_yaml.clone())
        .run(move |ctx| async move {
            ctx.phase("create", async {
                let result = ctx.apply("create", &real_yaml).await?;
                ctx.expect_eq_usize("create", "expected_resources", 3, result.succeeded.len())?;
                Ok(())
            })
            .await?;

            ctx.phase("test_cases", async {
                let mut test_config = Config::from_yaml(&yaml)?;
                let observer: Arc<dyn TestObserver> = Arc::new(StderrTestObserver);
                let results = ctx.client.test_with_observer(&mut test_config, observer).await.map_err(|e| anyhow::anyhow!("{e}"))?;
                ctx.expect_eq_usize("test_cases", "total", 3, results.total())?;
                ctx.expect_eq_usize("test_cases", "passed", 3, results.passed)?;
                ctx.expect_eq_usize("test_cases", "failed", 0, results.failed)?;
                for test in &results.tests {
                    ctx.expect("test_cases", test.passed, format!("'{}' passed", test.ref_name), format!("'{}' failed", test.ref_name))?;
                    ctx.expect("test_cases", !test.turns.is_empty(), format!("'{}' has turns", test.ref_name), "no turns")?;
                    for turn in &test.turns {
                        match &turn.outcome {
                            wxctl_sdk::TurnOutcome::Success { content, .. } => {
                                ctx.expect("test_cases", !content.is_empty(), format!("'{}' turn {} non-empty", test.ref_name, turn.turn_num), "empty")?;
                            }
                            other => {
                                ctx.expect("test_cases", false, "Success outcome", format!("turn {} of '{}': {:?}", turn.turn_num, test.ref_name, other))?;
                            }
                        }
                    }
                }
                Ok(())
            })
            .await?;

            ctx.phase("destroy", async { ctx.destroy("destroy", &real_yaml).await.map(|_| ()) }).await?;
            ctx.phase("verify_destroyed", async { ctx.assert_destroyed(&real_yaml).await }).await?;

            Ok(())
        })
        .await
}
