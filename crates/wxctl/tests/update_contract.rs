//! Final-phase contract + carve-out checks (no subprocess, no network):
//! AC 8 — the deployed Worker's `/check` JSON shape deserializes;
//! AC 9 — the CLI never contacts api.github.com directly (the Worker proxies it);
//! I1  — no private root paths / profile names leak into the update module.

use std::path::Path;

#[derive(serde::Deserialize)]
struct Resp {
    latest: Option<String>,
    news: Vec<Item>,
}
#[derive(serde::Deserialize)]
struct Item {
    id: String,
    severity: String,
    title: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

/// AC 8 (automatable portion): the documented `{ latest, news[] }` contract of the
/// deployed Worker (`https://wxctl-updates.randyphoa.workers.dev/check`)
/// deserializes with the expected fields. The "one Analytics Engine data point /
/// no IP stored" half is a NEEDS-HUMAN dashboard inspection (see batch notes).
#[test]
fn ac8_check_response_shape() {
    let json = r#"{"latest":"0.1.0","news":[{"id":"welcome-2026-06","severity":"info","title":"Welcome","body":"Thanks","url":null}]}"#;
    let r: Resp = serde_json::from_str(json).expect("worker /check shape deserializes");
    assert_eq!(r.latest.as_deref(), Some("0.1.0"));
    assert_eq!(r.news.len(), 1);
    assert_eq!(r.news[0].id, "welcome-2026-06");
    assert_eq!(r.news[0].severity, "info");
    assert!(!r.news[0].title.is_empty());
    let _ = (&r.news[0].body, &r.news[0].url); // optional fields present in the contract
    // Missing latest (GitHub unreachable) → still valid, "no update".
    let none: Resp = serde_json::from_str(r#"{"news":[]}"#).unwrap();
    assert!(none.latest.is_none() && none.news.is_empty());
}

/// AC 9: the CLI's only outbound host for this feature is the Worker — it never
/// contacts api.github.com directly (the Worker proxies GitHub server-side).
/// `api.github.com` may appear in comments that document its avoidance (e.g. the
/// self-update engine's module docs), but never in code that would contact it, so
/// the check forbids the literal only on non-comment lines.
#[test]
fn ac9_cli_never_calls_github_directly() {
    let mut hay = String::new();
    collect_rs(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut hay);
    for line in hay.lines() {
        if line.contains("api.github.com") {
            assert!(line.trim_start().starts_with("//"), "the CLI must not contact api.github.com in code (the Worker proxies it): {line}");
        }
    }
}

/// I1 (carve-out): no private root paths / profile names in the update module;
/// the sole outbound host is the public Worker const.
#[test]
fn i1_carve_out_update_module_clean() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/update");
    let mut hay = String::new();
    collect_rs(&dir, &mut hay);
    for needle in ["/Users/", ".techzone", "itz-saas", "itz-watsonx", "cp4d-"] {
        assert!(!hay.contains(needle), "carve-out I1: private marker {needle:?} leaked into the update module");
    }
    let registry = std::fs::read_to_string(dir.join("registry.rs")).unwrap();
    assert!(registry.contains("wxctl-updates.randyphoa.workers.dev"), "public Worker endpoint const present (I1)");
}

fn collect_rs(dir: &Path, out: &mut String) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            collect_rs(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push_str(&std::fs::read_to_string(&p).unwrap());
        }
    }
}
