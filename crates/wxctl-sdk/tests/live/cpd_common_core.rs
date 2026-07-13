//! Phase 3 deliverable: common_core CRUD lifecycle against an IBM Software
//! Hub 5.3.x cluster. Skips cleanly when `cp4d` is not configured in
//! `~/.wxctl/test_profiles.json` (same skip mechanism as the SaaS `test`
//! profile — see `create_test_client_for` in `tests/live.rs`).
//!
//! Run: `cargo test -p wxctl-sdk --features live-tests -- cpd_common_core`

use super::{LiveTest, short_id};

#[tokio::test]
async fn test_cpd_common_core_lifecycle() -> anyhow::Result<()> {
    let safe_id = short_id();
    let yaml = format!(
        r#"
kind: project
ref_name: cpd_common_core_{safe_id}_proj
name: cpd_common_core_{safe_id}_proj
metadata:
    requires:
        deployment: "software-5.3.x"
description: Phase 3 cpd_common_core live test project
type: wx
---
kind: catalog
ref_name: cpd_common_core_{safe_id}_cat
name: cpd_common_core_{safe_id}_cat
metadata:
    requires:
        deployment: "software-5.3.x"
description: Phase 3 cpd_common_core live test catalog
generator: bss
---
kind: space
ref_name: cpd_common_core_{safe_id}_space
name: cpd_common_core_{safe_id}_space
metadata:
    requires:
        deployment: "software-5.3.x"
description: Phase 3 cpd_common_core live test space
---
kind: software_specification
ref_name: cpd_common_core_{safe_id}_swspec
name: cpd_common_core_{safe_id}_swspec
metadata:
    requires:
        deployment: "software-5.3.x"
description: Phase 3 cpd_common_core live test software specification
software_configuration:
    type: cuda
    base_software_specification:
        name: runtime-24.1-py3.11
space: ${{space.cpd_common_core_{safe_id}_space}}
---
kind: package_extension
ref_name: cpd_common_core_{safe_id}_pkgext
name: cpd_common_core_{safe_id}_pkgext
metadata:
    requires:
        deployment: "software-5.3.x"
description: Phase 3 cpd_common_core live test package extension
type: pip_zip
space: ${{space.cpd_common_core_{safe_id}_space}}
"#
    );

    LiveTest::new("test_cpd_common_core_lifecycle").profile("cp4d").yaml(yaml).run_crud().await
}
