# board-red-team — multi-agent board "red team" → synthesized memo

> Five adversarial persona critics — CFO, activist investor, regulator,
> competitor, and an independent board member — each tear apart a pasted
> strategy from their own angle. A synthesis-lead coordinator gathers their
> verdicts and produces a single board memo: one recommendation (Proceed /
> Proceed with conditions / Pause / Reject) plus the top risks. This is the
> "multi-agent critic ensemble + coordinator + knowledge base + toolkit"
> shape, end to end.

**Tier:** Heavy

## The brief

The plain-English use case this example is built from (kept verbatim in
[`use-case.txt`](use-case.txt)):

> Multi-agent board "red team" where five persona critics (CFO, activist investor, regulator, competitor, and an independent board member) critique a pasted strategy and a synthesis-lead coordinator combines their critiques into a single board memo with an overall recommendation (Proceed / Proceed with conditions / Pause / Reject) and the top risks

That sentence is what `wxctl`'s compose tools turn into the [`config.yaml`](config.yaml)
below — the resources, their `${kind.ref}` wiring, and the `kind: test` checks. You can
regenerate it from the sentence, edit it, or write it by hand; the execute step is the
same either way.

## What it provisions

[`config.yaml`](config.yaml) declares **nine resources** plus two tests:

- `kind: knowledge_base` — `board_rubric_kb`, the board review rubric used by the
  coordinator when scoring critiques and framing the final recommendation.
- `kind: tool` (Python) — `market_signals`, a mock market-signal lookup the critic
  agents can call to ground their analysis in external data.
- `kind: toolkit` — `board_clock_toolkit`, sourced from the public registry
  (`uvx mcp-server-time`); no secrets required.
- `kind: agent` — `cfo`, focused on financial risk and capital allocation.
- `kind: agent` — `activist_investor`, pressing on shareholder value and governance.
- `kind: agent` — `regulator`, examining compliance, licensing, and regulatory exposure.
- `kind: agent` — `competitor`, probing competitive dynamics and market positioning.
- `kind: agent` — `board_member`, offering independent fiduciary and reputational scrutiny.
- `kind: agent` — `synthesis_lead`, the coordinator whose `collaborators` list the five
  persona agents; it calls each, synthesises their critiques via `board_rubric_kb`, and
  emits the final board memo.
- `kind: test` ×2 — one check that the memo carries an explicit recommendation keyword
  and one that the top-risks section is non-empty.

## Run it

No `${env:VAR}` variables are needed: the toolkit pulls from the public registry and the
tool serves mock data, so there is nothing to configure beyond a profile. Configure a
profile in `~/.wxctl/config.json` (see the
[top-level README](../../../README.md)), then from this directory:

```bash
wxctl plan    -f config.yaml           # preview the DAG; no live credentials needed
wxctl apply   -f config.yaml           # create the nine resources
wxctl test    -f config.yaml           # run the two kind: test checks
wxctl destroy -f config.yaml           # tear it all down
```

`apply` fetches the `mcp-server-time` server from the public registry; `plan` does not.

### Generating `log.jsonl`

By default wxctl writes no log file — it just renders to the terminal. To capture the
structured JSON log while running any command, prefix it with two env vars: `RUST_LOG`
turns the operator-log layer on, `WXCTL_LOG_PATH` sends it to a file instead of stderr:

```bash
RUST_LOG=wxctl=info WXCTL_LOG_PATH=log.jsonl wxctl apply -f config.yaml
```

No `log.jsonl` ships with this example — generate it on a live green run against your own
profile. `WXCTL_LOG_PATH` truncates the file on each run; add `WXCTL_LOG_APPEND=1` to
capture a full `plan → apply → test → destroy` lifecycle into one file.
