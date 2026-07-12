# ap-processing — invoice & AP agent over an ERP connection

> An accounts-payable team wants one agent that runs 3-way invoice matching,
> flags duplicate invoices, and routes exceptions to a human — without standing
> up a bespoke app. This is the "agent + knowledge base + multiple tools +
> ERP connection" shape, end to end, on mock data.

## The brief

The plain-English use case this example is built from (kept verbatim in
[`use-case.txt`](use-case.txt)):

> Accounts-payable agent that runs 3-way invoice matching against an ERP connection, flags duplicate invoices, and cites the company's AP policy and vendor directory

## What it provisions

[`config.yaml`](config.yaml) declares **five resources** plus three tests:

- `kind: orchestrate_connection` — `northwind-erp`, the ERP connection; its
  secrets are `${env:VAR}` placeholders (`ERP_BASE_URL`, `ERP_API_KEY`).
- `kind: knowledge_base` — the AP policy and vendor directory.
- `kind: tool` (Python) — [`invoice_matching`](resources/tool/invoice_matching/invoice_matching.py), a 3-way match over bundled mock PO data.
- `kind: tool` (Python) — [`duplicate_detection`](resources/tool/duplicate_detection/duplicate_detection.py), bound to the ERP connection, falling back to a bundled mock ledger.
- `kind: agent` — the **AP Processing Agent**, wired to both tools and the
  knowledge base, with cite-the-evidence and defer-to-human guidelines.
- `kind: test` ×3 — a clean match, an amount mismatch routed to a human, and a
  duplicate.

## Run it

The ERP connection's credentials are `${env:VAR}` references, so the two env vars
must be **set to any value** before running any command — including `plan`, which
errors (`WXCTL-V301`) if a referenced env var is unset. They do **not** need to be
real: the `duplicate_detection` tool falls back to bundled mock data, so a
placeholder is enough and `apply`/`test` succeed without a real ERP. Configure a
profile in `~/.wxctl/profiles.yaml` (see the
[top-level README](../../../../README.md)), then from this directory:

```bash
export ERP_BASE_URL=mock ERP_API_KEY=mock   # placeholders; real values not needed
wxctl plan    -f config.yaml   # preview the DAG; no live credentials needed
wxctl apply   -f config.yaml   # create the five resources
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
