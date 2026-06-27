### Example: WML Function (space + software spec + function + deployment)

**Input:**
A machine learning scoring service that takes a list of numbers and returns statistical summaries (mean, min, max, sum). Deployed as a WML function endpoint.

**Output:**
```yaml
kind: space
ref_name: scoring_space
name: scoring-space
type: wx
---
kind: software_specification
ref_name: scoring_swspec
name: scoring-swspec
base_software_specification: runtime-25.1-py3.12
space_id: ${space.scoring_space}
---
kind: wml_function
ref_name: scoring_function
name: scoring-function
description: Statistical scoring function that computes mean, min, max, and sum of input numbers
software_spec: ${software_specification.scoring_swspec}
space_id: ${space.scoring_space}
source_path: score.py
---
kind: wml_deployment
ref_name: scoring_deployment
name: scoring-deployment
description: Online endpoint for the statistical scoring service
asset: ${wml_function.scoring_function}
space_id: ${space.scoring_space}
online: {}
```

### WML Resource Rules

When the use case describes a **scoring service, ML function, or WML deployment** (not an agent):

- **Resource chain**: `space` → `software_specification` → `wml_function` → `wml_deployment`
- **Use `kind: space`** (not `kind: project`) for deployment environments. Set `type: wx`.
- **Use `kind: wml_function`** for Python scoring functions. Always include `source_path: score.py`.
- **Use `kind: ai_service`** instead of `wml_function` when the code is a directory (not a single file).
- **Do NOT generate both** `wml_function` and `ai_service` for the same scoring logic — pick one.
- **`wml_deployment.asset`** references the function: `${wml_function.<ref>}` or `${ai_service.<ref>}`.
- **`base_software_specification`**: Use `runtime-25.1-py3.12` (default). The older `runtime-24.1-py3.11` is deprecated.
- **Resource ordering**: `space` → `software_specification` → `wml_function`/`ai_service` → `wml_deployment`.
- **Do NOT generate agent, tool, or knowledge_base resources** for WML scoring services.
