# retail-bank-support — bank support agent on a custom gateway model

> A retail bank wants one support agent that answers product, fee, and
> disclosure questions from its handbook **and** looks up account details and
> recent transactions — running on its own AI-gateway model config rather than
> a built-in default. This is the "agent + custom model + knowledge base +
> multiple tools" shape, end to end, on mock data.

## The brief

The plain-English use case this example is built from (kept verbatim in
[`use-case.txt`](use-case.txt)):

> Retail-bank customer-support agent that answers product, fee, and disclosure questions from the bank handbook and looks up account details and recent transactions, running on a custom gateway model

## What it provisions

[`config.yaml`](config.yaml) declares **six resources** plus three tests:

- `kind: orchestrate_connection` — `riverstone-gateway`, credentials for the
  gateway model; its secret is a `${env:GATEWAY_API_KEY}` reference.
- `kind: model` — the **Riverstone Support Gateway Model**, a chat gateway
  model config (no watsonx space/project scope), referenced by the agent's
  `llm`.
- `kind: knowledge_base` — the products & disclosures handbook.
- `kind: tool` (Python) — [`account_lookup`](resources/tool/account_lookup/account_lookup.py) and [`transaction_lookup`](resources/tool/transaction_lookup/transaction_lookup.py), both over bundled mock data.
- `kind: agent` — the **Retail Bank Support** agent, wired to the model, both
  tools, and the knowledge base, with cite-disclosures and no-specific-advice
  guidelines.
- `kind: test` ×3 — a fee question, an account lookup, and the advice guardrail.

## Run it

The gateway model's connection credential is a `${env:VAR}` reference, so
`GATEWAY_API_KEY` must be **set to any value** before running any command —
including `plan`, which errors (`WXCTL-V301`) if a referenced env var is unset.
It does **not** need to be real: the tools serve bundled mock data, so a
placeholder is enough and `apply`/`test` succeed without a core-banking backend.
Configure a profile in `~/.wxctl/profiles.yaml` (see the
[top-level README](../../../../README.md)), then from this directory:

```bash
export GATEWAY_API_KEY=mock   # placeholder; a real value is not needed
wxctl plan    -f config.yaml   # preview the DAG; no live credentials needed
wxctl apply   -f config.yaml   # create the six resources
wxctl test    -f config.yaml   # run the three kind: test checks
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
a full `plan → apply → test → destroy` lifecycle into one file.
