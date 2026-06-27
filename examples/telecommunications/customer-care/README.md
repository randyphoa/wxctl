# customer-care — telecom assistant over a care handbook + subscriber lookup

> A mobile carrier wants one customer-care assistant that answers plan, roaming,
> and troubleshooting questions from the care handbook **and** pulls up a
> subscriber's plan and data usage — without standing up a bespoke app. It
> discloses plan and usage only, never stored personal details. The
> "agent + knowledge base + tool" shape, applied to telecom.

## The brief

The plain-English use case this example is built from (kept verbatim in
[`use-case.txt`](use-case.txt)):

> Telecom customer-care agent that answers plan, roaming, and troubleshooting questions from the care handbook and looks up a subscriber's plan and data usage

That sentence is what `wxctl`'s compose tools turn into the [`config.yaml`](config.yaml)
below — the resources, their `${kind.ref}` wiring, and the `kind: test` checks. You can
regenerate it from the sentence, edit it, or write it by hand; the execute step is the
same either way.

## What it provisions

[`config.yaml`](config.yaml) declares three resources plus tests:

- `kind: knowledge_base` — indexes [`resources/knowledge_base/care_handbook.txt`](resources/knowledge_base/care_handbook.txt) for plan, roaming, and troubleshooting answers.
- `kind: tool` (Python) — [`subscriber_lookup`](resources/tool/subscriber_lookup/subscriber_lookup.py),
  which serves subscriber plan and usage from a bundled sample directory (MSISDNs in the
  reserved `555` range) and returns plan/usage only.
- `kind: agent` — the **Customer Care Agent**, wired to the knowledge base and the tool,
  with a subscriber-data privacy guideline and starter prompts.
- `kind: test` ×4 — plan/roaming/troubleshooting questions answered from the handbook,
  plus a usage lookup that must call the tool.

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

The tool serves a bundled sample directory (MSISDNs `+1-555-0142`, `+1-555-0173`,
`+1-555-0188`), so `apply` and `test` succeed without any subscriber system or other
backing service.

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
- `test` confirms the agent cites the handbook for data-overage, roaming, and
  troubleshooting questions, and calls `subscriber_lookup` for `+1-555-0142`.
