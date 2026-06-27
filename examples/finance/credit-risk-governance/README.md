# credit-risk-governance — deploy, govern, and serve a credit-risk model across four IBM products

> A lender deploys a credit-risk scoring model on **watsonx.ai**, governs it with
> a **watsonx.governance** OpenScale quality monitor, and puts a **watsonx
> Orchestrate** loan-decision agent in front of it. The agent scores applicants
> by calling the *same* governed deployment through a tool — so every
> approve/decline answer it gives is the one OpenScale is watching. The agent's
> own chat model is **gpt-oss-120b served by watsonx.ai** through the Orchestrate
> AI gateway. One `config.yaml`, four products, wired by reference.

## The brief

The plain-English use case this example is built from (kept verbatim in
[`use-case.txt`](use-case.txt)):

> Deploy a transparent credit-risk scoring model on watsonx.ai, govern it with a watsonx.governance OpenScale quality monitor, and put a watsonx Orchestrate loan-decision agent in front of it — the agent scores applicants by calling the same governed deployment through a tool, so every approve/decline answer it gives is the one OpenScale is watching.

## What it provisions

[`config.yaml`](config.yaml) declares **13 resources** plus two tests, across
three layers:

**watsonx.ai — the model.**
- `kind: space` — the watsonx.ai deployment space; also scopes the agent's chat model and binds the OpenScale service provider.
- `kind: software_specification` — base runtime `runtime-25.1-py3.12`.
- `kind: wml_function` — the deployable scorer ([`resources/model/score.py`](resources/model/score.py)).
- `kind: wml_deployment` — an online deployment of the function.

**watsonx.governance — the guardrail.**
- `kind: service_provider` — the OpenScale binding to watsonx.ai (`service_type: watson_machine_learning`).
- `kind: data_mart` — an internal OpenScale data mart.
- `kind: subscription` — subscribes the live deployment to the data mart for monitoring.
- `kind: monitor_instance` — a `quality` monitor on the subscription.

**watsonx Orchestrate — the assistant.**
- `kind: orchestrate_connection` (`crg-gateway`) — credentials for the agent's chat model.
- `kind: model` — `gpt-oss-120b` served by watsonx.ai through the AI gateway, scoped by the *same* deployment space.
- `kind: orchestrate_connection` (`crg-scorer`) — a `key_value` connection carrying the watsonx.ai endpoint, an API key, and the live `deployment_id`; wxctl `depends_on`-orders it after the deployment so the id is real.
- `kind: tool` — [`score_applicant`](resources/tool/score_applicant/), a Python tool that reads the scorer connection and POSTs to the governed deployment's `/predictions` endpoint.
- `kind: agent` — the **Credit Decision Desk**, which uses the gateway model and the `score_applicant` tool.

**Tests.**
- `kind: test` (`gov_score_test`) — scores the deployment directly, asserting the deterministic `[1, 0.847]`.
- `kind: test` (`agent_decision_test`) — asks the agent to assess an applicant, asserts it calls `score_applicant` and reports **Approve @ ~0.847**.

The scorer is **deterministic and transparent**: for input
`[annual_income, debt_to_income, credit_score]` it returns `[decision, probability]`,
where `decision = 1` (approve) iff `credit_score >= 650` and `debt_to_income <= 0.40`,
and `probability = round(min(credit_score, 850) / 850, 3)`. Both tests score
`[120000, 0.25, 720]` → approve @ `0.847`.

### The cross-product seam

The whole point of this example is the wiring `wxctl` resolves for you:

```
wml_deployment ─(.metadata.id, depends_on)→ scorer_conn ─→ score_applicant.binding.python.connections
gateway_conn ─→ gateway_model ─(llm)→ loan_desk        score_applicant ─(tools)→ loan_desk
space ─→ gateway_model.watsonx_space_id                 (same space as the deployment)
```

The agent never re-implements the scoring logic — it calls the live, governed
watsonx.ai deployment. Two `key_value` connection details worth knowing if you
build your own variant (both are in the `config.yaml` comments):
- the endpoint key is **`wml_url`, not `url`** — `url` is reserved in a wxO
  connection (it becomes the server_url) and is stripped from the runtime
  credentials;
- the deployment id is wired as **`${wml_deployment.gov_deployment.metadata.id}`**
  with the explicit `.metadata.id` path — a bare `${kind.ref}` resolves to the
  whole resource object, which a key-value store drops (it keeps only strings).

## Run it

This example targets **watsonx.ai + watsonx.governance OpenScale + watsonx
Orchestrate on watsonx SaaS** — `apply`/`test`/`destroy` run live against a
watsonx SaaS profile. Your profile must declare a `watsonx_ai`, an `openscale`,
**and** a `watsonx_orchestrate` service block.

Three `${env:VAR}` references must be set before any command — including `plan`,
which errors (`WXCTL-V301`) on an unset referenced var (placeholders suffice for
`plan`; real values are needed for live `apply`):

| Var | Used by | Value |
| --- | --- | --- |
| `WATSONX_APIKEY` | OpenScale service provider + the scorer tool | a watsonx.ai/WML API key |
| `GATEWAY_API_KEY` | the agent's AI-gateway model connection | the gateway key |
| `WATSONX_URL` | the gateway model host + the scorer connection | your SaaS watsonx.ai host (e.g. `https://us-south.ml.cloud.ibm.com`) |

Configure a profile in `~/.wxctl/config.json` (see the
[top-level README](../../README.md)), then from this directory:

```bash
export WATSONX_APIKEY=mock GATEWAY_API_KEY=mock WATSONX_URL=https://us-south.ml.cloud.ibm.com  # placeholders for plan
wxctl plan    -f config.yaml   # preview the DAG; no live credentials needed
wxctl apply   -f config.yaml   # create the 13 resources across the three products
wxctl test    -f config.yaml   # score the deployment, and have the agent score via the governed model
wxctl destroy -f config.yaml   # tear it all down
```

`wxctl plan` resolves references and validates without calling any service.

> **Note — OpenScale data mart is a per-instance singleton.** OpenScale allows
> one internal data mart per instance. If a previous run on the same instance
> left one behind, `data_mart` create returns `409` (and `subscription` /
> `monitor_instance` skip). Clearing the stale data mart first lets the
> governance leg apply cleanly; the agent + scoring path do not depend on it.

### Generating `log.jsonl`

By default wxctl writes no log file. To capture the structured JSON log while
running a command, prefix it with `RUST_LOG=wxctl=info` (operator-log layer on)
and `WXCTL_LOG_PATH=log.jsonl` (send it to a file):

```bash
RUST_LOG=wxctl=info WXCTL_LOG_PATH=log.jsonl wxctl apply -f config.yaml
```

`WXCTL_LOG_PATH` truncates the file each run; add `WXCTL_LOG_APPEND=1` to stream a
full `apply → test → destroy` lifecycle into one file.
