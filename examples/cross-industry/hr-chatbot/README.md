# hr-chatbot — HR assistant over a handbook + employee directory

> An HR team wants one assistant that answers policy questions from the official
> handbook **and** pulls up individual employee records — without standing up a
> bespoke app. This is the canonical "agent + knowledge base + tool" shape,
> end to end.

## The brief

The plain-English use case this example is built from (kept verbatim in
[`use-case.txt`](use-case.txt)):

> HR chatbot with employee handbook knowledge base and an employee records lookup tool

That sentence is what `wxctl`'s compose tools turn into the [`config.yaml`](config.yaml)
below — the resources, their `${kind.ref}` wiring, and the `kind: test` checks. You can
regenerate it from the sentence, edit it, or write it by hand; the execute step is the
same either way.

## What it provisions

[`config.yaml`](config.yaml) declares three resources plus tests:

- `kind: knowledge_base` — indexes [`resources/knowledge_base/employee_handbook.txt`](resources/knowledge_base/employee_handbook.txt) for policy answers.
- `kind: tool` (Python) — [`employee_records_lookup`](resources/tool/employee_records_lookup/employee_records_lookup.py),
  which serves employee records from a bundled sample directory (`E001`–`E003`).
- `kind: agent` — the **HR Chatbot**, wired to the knowledge base and the tool, with a
  privacy guideline and starter prompts.
- `kind: test` ×4 — policy questions answered from the handbook, plus employee lookups
  that must call the tool.

## Run it

No external dependencies: the tool serves a bundled sample directory, so there's nothing
to stand up. Configure a profile in `~/.wxctl/config.json` (see the
[top-level README](../../../README.md)), then from this directory:

```bash
wxctl plan    -f config.yaml           # preview the DAG; no credentials needed
wxctl apply   -f config.yaml           # create the three resources
wxctl test    -f config.yaml           # run the four kind: test checks
wxctl destroy -f config.yaml           # tear it all down
```

The tool serves a bundled sample directory (`E001`–`E003`), so `apply` and `test`
succeed without any database or other backing service.

### Generating `log.jsonl`

By default wxctl writes no log file — it just renders to the terminal. To capture the
structured JSON log ([`log.jsonl`](log.jsonl) below) while running any command, prefix it
with two env vars: `RUST_LOG` turns the operator-log layer on, `WXCTL_LOG_PATH` sends it
to a file instead of stderr:

```bash
RUST_LOG=wxctl=info WXCTL_LOG_PATH=log.jsonl wxctl apply -f config.yaml
```

`WXCTL_LOG_PATH` truncates the file on each run; add `WXCTL_LOG_APPEND=1` to capture a
full `plan → apply → test → destroy` lifecycle into one file (see
[Behind the scenes](#behind-the-scenes--logjsonl) for the lifecycle loop).

## Expected output

- `apply` creates the knowledge base, the Python tool, and the agent (3 resources).
- `test` confirms the agent cites the handbook for PTO and remote-work questions, and
  calls `employee_records_lookup` for `E001` and `E003`.

## Behind the scenes — `log.jsonl`

[`log.jsonl`](log.jsonl) is the full lifecycle captured at `INFO`, one JSON event per
line, in `plan → apply → test → destroy` order. It's the structured view of everything
wxctl decided and did. The `target` field names the phase:

- `wxctl::decision` — what reconciliation chose for each resource and why (`Create`,
  `Update`, `NoOp`, `Delete`).
- `wxctl::substage::execution` — each create/delete, with `duration_ms`.
- `wxctl::summary` — the per-run tally (`3/3 succeeded, 0 failed`).

Most events carry a `spans` array whose root records the `command`, `run_id`, and
`profile`; `operation_id` correlates the events of a single operation. Slice it with `jq`:

```bash
# the headline result of each run in the lifecycle
jq -r 'select(.target=="wxctl::summary").fields.message' log.jsonl

# every reconciliation decision and its reason
jq -r 'select(.target=="wxctl::decision").fields | "\(.decision)\t\(.resource_type).\(.resource_name)\t\(.reason)"' log.jsonl

# what got created/deleted, with timings
jq -r 'select(.target=="wxctl::substage::execution").fields | "\(.kind) \(.name) — \(.duration_ms)ms"' log.jsonl
```

This committed copy is a **sanitized snapshot**: the profile name and the IBM Cloud
account / COS identifiers are replaced with placeholders, secrets never reach an `INFO`
log (request bodies are debug/trace and redacted by construction), and the timestamps
reflect when it was captured — treat it as illustrative, not a golden test fixture.

Regenerating it is **optional** — running the example (the four commands above) needs
none of this. It's only here if you want to refresh the bundled snapshot against your own
profile (swap `watsonx-saas`). All structured events use `wxctl::*` log targets, so a
single `RUST_LOG=wxctl=info` captures everything, and `WXCTL_LOG_APPEND=1` streams every
run into one file (it truncates by default):

```bash
: > log.jsonl                          # start fresh
for cmd in plan apply test destroy; do
  RUST_LOG=wxctl=info WXCTL_LOG_PATH=log.jsonl WXCTL_LOG_APPEND=1 \
    wxctl -p watsonx-saas "$cmd" -f config.yaml
done
```
