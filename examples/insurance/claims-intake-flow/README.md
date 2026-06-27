# claims-intake-flow — claims assistant with python + OpenAPI + flow tools

> A claims-intake assistant that combines three different tool bindings on one
> custom gateway model: a **Python tool** that validates a claim ID and member
> eligibility, an **OpenAPI tool** that does a public reference-data lookup
> (each operation becomes its own tool), and a **decisions flow** that computes
> the required insurance rate from a rate table the model cannot infer. This is
> the "agent + custom gateway model + python/OpenAPI/flow tools" shape, end to
> end, on mock data.

**Tier:** Medium

## The brief

The plain-English use case this example is built from (kept verbatim in
[`use-case.txt`](use-case.txt)):

> Claims-intake assistant that validates a claim ID and member eligibility with a python tool, looks up reference data via a public OpenAPI tool, and computes the required insurance rate from a decisions flow, all on a custom gateway model

## What it provisions

[`config.yaml`](config.yaml) declares **five resources** (the OpenAPI tool
expands to two operation tools at plan time) plus three tests:

- `kind: orchestrate_connection` — `claims-gateway`, credentials for the gateway
  model; its secret is a `${env:GATEWAY_API_KEY}` reference.
- `kind: space` — a deployment space (created dynamically) that scopes the
  gateway model's watsonx.ai inference.
- `kind: model` — the **Claims Assistant Gateway Model**, `gpt-oss-120b` served
  by watsonx.ai through the AI gateway (`custom_host: ${env:WATSONX_URL}`, scoped
  by the space), used by the agent's `llm` and pinned as the flow runtime's LLM.
- `kind: tool` (Python) — [`claim_eligibility`](resources/tool/claim_eligibility/claim_eligibility.py),
  over bundled mock data.
- `kind: tool` (OpenAPI) — `claims_lookup`, a public no-auth reference-data spec
  ([`resources/openapi/reference-data.yaml`](resources/openapi/reference-data.yaml));
  each operation expands to its own tool (`claims_lookup_echoGet`, `claims_lookup_echoPost`).
- `kind: tool` (flow) — `claim_rate_flow`, an ADK decisions flow
  ([`resources/flow/claim_rate.flow.json`](resources/flow/claim_rate.flow.json))
  whose `flow_llm_model` pins the gateway model.
- `kind: agent` — the **Claims Intake Assistant**, wired to the model and all
  three tool bindings.
- `kind: test` ×3 — an eligibility check (python tool), a reference-data lookup
  (OpenAPI turn), and a `flow:`-mode test that forces a real flow run.

## Run it

This example runs live on a **SaaS watsonx Orchestrate profile** (the AI gateway
and flow runtime are SaaS surfaces). The gateway model is `gpt-oss-120b` served by
watsonx.ai, so it needs two `${env:VAR}` references — `GATEWAY_API_KEY` (the
gateway connection credential, a watsonx API key) and `WATSONX_URL` (the
watsonx.ai inference host the gateway routes to). Both must be **set to any value**
before running any command — including `plan`, which errors (`WXCTL-V301`) if a
referenced env var is unset. They do **not** need to be real for `plan`.
Configure a profile in `~/.wxctl/config.json` (see the
[top-level README](../../README.md)), then from this directory:

```bash
export GATEWAY_API_KEY=mock WATSONX_URL=https://us-south.ml.cloud.ibm.com   # placeholders; real values needed only for a live apply/test
wxctl plan    -f config.yaml   # preview the DAG; no live credentials needed
wxctl apply   -f config.yaml   # register the tools/flow and create the model + agent
wxctl test    -f config.yaml   # run the three kind: test checks (incl. the flow-mode test)
wxctl destroy -f config.yaml   # tear it all down
```

### Generating `log.jsonl`

By default wxctl writes no log file. To capture the structured JSON log while
running any command, prefix it with two env vars — `RUST_LOG` turns the
operator-log layer on, `WXCTL_LOG_PATH` sends it to a file:

```bash
RUST_LOG=wxctl=info WXCTL_LOG_PATH=log.jsonl wxctl apply -f config.yaml
```

`WXCTL_LOG_PATH` truncates the file each run; add `WXCTL_LOG_APPEND=1` to stream
a full `apply → test → destroy` lifecycle into one file.
