# model-router — multi-model AI-gateway routing

> Two AI-gateway models — a fast low-latency tier and a deeper specialist tier —
> back a triage agent and a specialist agent. A supervisor agent routes each
> question to the right collaborator by complexity. This is the "multiple
> gateway models + agents routed by a supervisor's `collaborators`" shape, end
> to end.

**Tier:** Medium

## The brief

The plain-English use case this example is built from (kept verbatim in
[`use-case.txt`](use-case.txt)):

> Multi-model router with a fast triage agent and a deep specialist agent on different AI-gateway models, plus a supervisor agent that routes a query to the right collaborator by complexity

## What it provisions

[`config.yaml`](config.yaml) declares **six resources** plus two tests:

- `kind: orchestrate_connection` — `router-gateway`, credentials for the gateway
  models; its secret is a `${env:GATEWAY_API_KEY}` reference.
- `kind: space` — a deployment space (created dynamically) that scopes both
  gateway models' watsonx.ai inference.
- `kind: model` ×2 — **Fast Triage Gateway Model** (`gpt-oss-120b`) and **Deep
  Specialist Gateway Model** (`llama-3-3-70b`), two distinct watsonx.ai-served
  gateway models (`virtual-model/watsonx/<provider>/<model>`, `custom_host:
  ${env:WATSONX_URL}`).
- `kind: agent` ×3 — a **Triage Agent** on the fast model, a **Specialist Agent**
  on the deep model, and a **Router Agent** supervisor whose `collaborators` list
  the other two.
- `kind: test` ×2 — a simple query that should route to triage and a complex one
  that should route to the specialist.

## Run it

This example runs live on a **SaaS watsonx Orchestrate profile** (the AI gateway
is a SaaS surface). The gateway models are served by watsonx.ai, so they need two
`${env:VAR}` references — `GATEWAY_API_KEY` (the gateway connection credential, a
watsonx API key) and `WATSONX_URL` (the watsonx.ai inference host). Both must be
**set to any value** before running any command — including `plan`, which errors
(`WXCTL-V301`) if a referenced env var is unset. They do **not** need to be real
for `plan`. Configure a profile in `~/.wxctl/config.json` (see the
[top-level README](../../README.md)), then from this directory:

```bash
export GATEWAY_API_KEY=mock WATSONX_URL=https://us-south.ml.cloud.ibm.com   # placeholders; real values needed only for a live apply/test
wxctl plan    -f config.yaml   # preview the DAG; no live credentials needed
wxctl apply   -f config.yaml   # create the connection, space, two models, and three agents
wxctl test    -f config.yaml   # run the two routing checks
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
