# member-services — health-plan assistant over a handbook + claim status

> A health plan wants one member-services assistant that answers benefits and
> coverage questions from the official plan handbook **and** checks the status of
> a member's claim — without standing up a bespoke app. It informs members; it
> never adjudicates coverage or eligibility. The "agent + knowledge base + tool"
> shape, applied to healthcare.

## The brief

The plain-English use case this example is built from (kept verbatim in
[`use-case.txt`](use-case.txt)):

> Health-plan member-services agent that answers benefits and coverage questions from the plan handbook and looks up the status of a member's claim

That sentence is what `wxctl`'s compose tools turn into the [`config.yaml`](config.yaml)
below — the resources, their `${kind.ref}` wiring, and the `kind: test` checks. You can
regenerate it from the sentence, edit it, or write it by hand; the execute step is the
same either way.

## What it provisions

[`config.yaml`](config.yaml) declares three resources plus tests:

- `kind: knowledge_base` — indexes [`resources/knowledge_base/plan_handbook.txt`](resources/knowledge_base/plan_handbook.txt) for benefits and coverage answers.
- `kind: tool` (Python) — [`claim_status_lookup`](resources/tool/claim_status_lookup/claim_status_lookup.py),
  which serves claim records (`CLM1001`–`CLM1003`) from a bundled sample directory and
  reports status only.
- `kind: agent` — the **Member Services Agent**, wired to the knowledge base and the tool,
  with a defer-to-human guideline (it informs, it does not adjudicate) and starter prompts.
- `kind: test` ×4 — benefits/coverage questions answered from the handbook, a claim-status
  lookup that must call the tool, and a determination question the agent must defer.

## Run it

No external dependencies: the tool serves a bundled sample directory, so there's nothing
to stand up. Configure a profile in `~/.wxctl/config.json` (see the
[top-level README](../../README.md)), then from this directory:

```bash
wxctl plan    -f config.yaml           # preview the DAG; no credentials needed
wxctl apply   -f config.yaml           # create the three resources
wxctl test    -f config.yaml           # run the four kind: test checks
wxctl destroy -f config.yaml           # tear it all down
```

The tool serves a bundled sample directory (`CLM1001`–`CLM1003`), so `apply` and `test`
succeed without any claims system or other backing service.

### Generating `log.jsonl`

By default wxctl writes no log file — it just renders to the terminal. To capture the
structured JSON log while running any command, prefix it with two env vars: `RUST_LOG`
turns the operator-log layer on, `WXCTL_LOG_PATH` sends it to a file instead of stderr:

```bash
RUST_LOG=wxctl=info WXCTL_LOG_PATH=log.jsonl wxctl apply -f config.yaml
```

`WXCTL_LOG_PATH` truncates the file on each run; add `WXCTL_LOG_APPEND=1` to capture a
full `plan → apply → test → destroy` lifecycle into one file.

## Expected output

- `apply` creates the knowledge base, the Python tool, and the agent (3 resources).
- `test` confirms the agent cites the handbook for preventive-care and out-of-pocket
  questions, calls `claim_status_lookup` for `CLM1001`, and defers coverage
  determinations to the plan's reviewers.
