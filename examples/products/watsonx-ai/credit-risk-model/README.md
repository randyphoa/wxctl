# credit-risk-model — deploy a transparent credit-risk scorer on watsonx.ai

> A lender wants a credit-risk scoring model packaged and deployed on
> watsonx.ai — a custom software specification, a deployable scorer, an online
> deployment — then scored for an approve/decline decision and probability.
> This is the watsonx.ai model-deployment lifecycle, end to end, on a
> deterministic mock scorer.

## The brief

The plain-English use case this example is built from (kept verbatim in
[`use-case.txt`](use-case.txt)):

> Deploy a transparent credit-risk scoring model on watsonx.ai — packaging a custom software specification, a deployable scorer, and an online deployment — and score a loan applicant for an approve/decline decision and probability.

## What it provisions

[`config.yaml`](config.yaml) declares **seven resources** plus one test:

- `kind: space` — the watsonx.ai deployment space that scopes every asset.
- `kind: package_extension` — a pip `requirements_txt` extension built from
  [`resources/model/requirements.txt`](resources/model/requirements.txt)
  (`joblib`), wired into the software specification.
- `kind: software_specification` — base runtime `runtime-25.1-py3.12` plus the
  package extension.
- `kind: wml_function`, `kind: wml_script`, `kind: ai_service` — three
  deployable forms of the same scorer
  ([`resources/model/score.py`](resources/model/score.py)), each scoped to the
  space and the custom software specification.
- `kind: wml_deployment` — an online deployment of the `wml_function`.
- `kind: test` — scores the deployment with a loan applicant and asserts the
  deterministic approve/decline output.

The scorer is **deterministic and transparent** (no training, no model load, no
randomness): for input `[annual_income, debt_to_income, credit_score]` it returns
`[decision, probability]`, where `decision = 1` (approve) iff `credit_score >= 650`
and `debt_to_income <= 0.40`, and `probability = round(min(credit_score, 850) / 850, 3)`.
The test scores `[120000, 0.25, 720]` and asserts `[1, 0.847]`.

## Run it

This example targets **watsonx.ai on watsonx SaaS** — `apply`/`test`/`destroy` run
live against a watsonx SaaS profile. It needs **no env vars**: the
package extension reads the local `requirements.txt` and the assets read the local
`score.py`. Configure a profile in `~/.wxctl/profiles.yaml` (see the
[top-level README](../../../../README.md)), then from this directory:

```bash
wxctl plan    -f config.yaml   # preview the DAG; no live credentials needed
wxctl apply   -f config.yaml   # create the seven resources
wxctl test    -f config.yaml   # score the deployment, assert [1, 0.847]
wxctl destroy -f config.yaml   # tear it all down
```

`wxctl plan` resolves references and validates without calling any service. A
trailing "Unsupported auth type" from the template `default` profile is expected
and harmless — it is an auth probe **after** validation/reconciliation complete.

> **Build latency:** the custom `software_specification` builds from
> `requirements.txt` at apply time and can take a few minutes; the single
> lightweight `joblib` dependency keeps it inside default timeouts.

### Generating `log.jsonl`

By default wxctl writes no log file. To capture the structured JSON log while
running a command, prefix it with `RUST_LOG=wxctl=info` (turns the operator-log
layer on) and `WXCTL_LOG_PATH=log.jsonl` (sends it to a file):

```bash
RUST_LOG=wxctl=info WXCTL_LOG_PATH=log.jsonl wxctl apply -f config.yaml
```

`WXCTL_LOG_PATH` truncates the file each run; add `WXCTL_LOG_APPEND=1` to stream a
full `apply → test → destroy` lifecycle into one file.
