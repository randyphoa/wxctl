# analytics-workspace — Common Core platform primitives, one portable DAG

> Stand up an analytics workspace from a single declarative config: an analytics
> **project** with a PostgreSQL data **connection**, plus a deployment **space**
> carrying a custom conda **runtime** (a package extension + a software
> specification built from it). The same config applies unchanged on a watsonx
> SaaS-WKC profile **and** on a CP4D / Software profile — deployment is the
> profile, not the config.

## The brief

The plain-English use case this example is built from (kept verbatim in
[`use-case.txt`](use-case.txt)):

> Stand up an analytics workspace: a project with a PostgreSQL data connection, plus a deployment space with a custom conda runtime.

## What it provisions

[`config.yaml`](config.yaml) declares five Data & AI Common Core resources wired
into one DAG — no `kind: test`:

- `kind: project` (`type: wx`) — the analytics project.
- `kind: common_core_connection` — a PostgreSQL connection asset scoped to the
  project (`project_id` → the project). It is created with `test: "false"`, so
  wxctl does **not** validate a live datasource — there is no real database to
  stand up, and the password (`${env:CC_CONN_PASSWORD}`) is never used for a real
  call.
- `kind: space` (`type: wx`) — a deployment space.
- `kind: package_extension` (`type: conda_yml`) — a custom conda environment
  built from [`resources/runtime/env.yaml`](resources/runtime/env.yaml),
  scoped to the space.
- `kind: software_specification` — a custom runtime spec built on
  `runtime-25.1-py3.12` plus the package extension, scoped to the space.

The cross-kind edges (`project → connection`, `space → {extension, swspec}`,
`extension → swspec`) are wired with `${kind.ref}` references and resolve at plan
time.

## Run it

This example carries **no `kind: test`** — it is a pure-platform DAG, so
verification is the `apply` succeeding (all five resources created, 0 errors) and
a clean `destroy`, rather than `wxctl test` checks.

The connection's password is a `${env:CC_CONN_PASSWORD}` secret. wxctl's
`${env:VAR}` interpolation is strict — the variable must be set for **every**
command (including `plan`), or you get `WXCTL-V301`. Because the connection uses
`test: "false"`, the value is never used for a live call, so a placeholder is
fine:

```bash
export CC_CONN_PASSWORD=placeholder        # never used (test: "false"); set it anyway

wxctl plan    -f config.yaml               # preview the 5-resource DAG; no credentials needed
wxctl apply   -f config.yaml               # create project + connection + space + extension + swspec
wxctl destroy -f config.yaml              # tear it all down
```

`plan` is offline — it runs validation and reconciliation before any service
call, so "validates offline" means no live credentials and no network (the
`CC_CONN_PASSWORD` placeholder carries no secret).

## Portability

This is the suite's deployment-portability example: one unchanged `config.yaml`
applies on both a watsonx SaaS-WKC profile and a CP4D / Software profile. The
config deliberately omits any deployment pin. Because portability is this
example's whole point, the live lifecycle is captured on **both** deployments and
committed as two suffixed logs ([`log.jsonl.saas`](log.jsonl.saas) and
[`log.jsonl.software`](log.jsonl.software)) — departing from the single-`log.jsonl`
shape of the other examples. See those logs for the per-deployment capture.

## Expected output

- `plan` reports `validation` ok and `reconciliation 5 reconciled` (all refs and
  the `env.yaml` path resolved).
- `apply` creates the project, connection, space, package extension, and software
  specification — five resources, 0 errors.
- A second `apply` before `destroy` reports NoChange.
- `destroy` removes all five.
