# sal-enrichment — enable SAL and auto-enrich a customer table (watsonx.data)

> A retail bank wants its customer data automatically tagged against a governed
> business glossary. This example registers IBM Knowledge Catalog, enables the
> watsonx.data **Semantic Automation Layer (SAL)**, uploads a glossary, sets
> enrichment defaults, and runs an enrichment job over a customer table — the
> SAL enrichment chain, end to end.

**Tier:** Heavy. **Deployment:** Software / CP4D only (SaaS exposes no SAL).

## The brief

The plain-English use case this example is built from (kept verbatim in
[`use-case.txt`](use-case.txt)):

> Enable the watsonx.data Semantic Automation Layer for a retail bank and
> auto-enrich a customer table against a business glossary: register IBM
> Knowledge Catalog, enable SAL, upload a glossary of customer-data terms, set
> global and per-project enrichment defaults, and run an enrichment job over the
> customer table.

## What it provisions

[`config.yaml`](config.yaml) declares **six resources** (no `kind: test` — this
is a data example; verification is `apply` succeeding and the bulk/job kinds
reaching terminal success):

- `kind: integration` (`type: ikc`) — registers IBM Knowledge Catalog with
  watsonx.data (`type` + optional `catalogs.catalog_names`; **no `ssl`** — `ssl:
  true` makes the integration unreadable).
- `kind: sal_integration` — enables SAL (`apikey` + `engine_id` + `storage_type`).
- `kind: sal_glossary` — a **multipart CSV upload**
  ([`resources/glossary/sal_terms.csv`](resources/glossary/sal_terms.csv),
  `replace_option: all`), polled to `SUCCEEDED`.
- `kind: sal_global_settings` — global enrichment defaults (LLM-based term
  assignment, threshold).
- `kind: sal_enrichment_settings` — per-project settings
  (`project_id: ${env:WXCTL_SAL_PROJECT_ID}`, name matching).
- `kind: sal_enrichment_job` — an enrichment job over the
  `${env:WXCTL_SAL_ENRICH_CATALOG/_SCHEMA/_TABLE}` table; the handler resolves the
  SAL-created `SAL Mapping /{catalog}/{schema}` project and polls the run to
  `Completed`.

## Prerequisites (live apply only)

`plan` validates offline, then reconciliation issues read-only discovery GET/list
calls against the catalog (so it needs a non-SaaS CP4D/Software profile's
credentials; `${env:WXCTL_SAL_*}` placeholders are fine and no secrets are
stored here). SAL governance *writes* are further gated — a live `apply` needs
all of:

1. **SAL enabled** — this config enables it (`integration` + `sal_integration`).
2. **`glossary_admin` role** on the runtime user (custom "Governance
   Administrator" role) — glossary upload + enrichment write governance artifacts.
   Missing it → `WKCBG1025E` / `403` on upload.
3. **`WXCTL_SAL_PROJECT_ID`** — a **real** IKC governance project id (a placeholder
   `0000…` 400s with `CATSV5025E`).
4. **`WXCTL_SAL_ENRICH_CATALOG`/`_SCHEMA`/`_TABLE`** — an **existing, populated**
   table to enrich (e.g. the `lakehouse-engines` output or a native
   `iceberg_data/sal_demo/customers`). A missing target fails the job.

Set `WXCTL_IKC_URL`/`_APIKEY`/`_USERNAME`, `WXCTL_SAL_APIKEY`,
`WXCTL_SAL_ENGINE_ID`, `WXCTL_SAL_PROJECT_ID`, and the enrich
`CATALOG`/`SCHEMA`/`TABLE` vars. The glossary CSV is shipped in-repo
(`resources/glossary/sal_terms.csv`), not env-sourced.

## Run it

`wxctl plan` resolves the `${env:WXCTL_SAL_*}` placeholders and validates, then
reads current state via read-only discovery under a non-SaaS CP4D/Software
profile. Live `apply`/`destroy` target watsonx.data Software:

```bash
export WXCTL_IKC_URL=mock WXCTL_IKC_APIKEY=mock WXCTL_IKC_USERNAME=mock
export WXCTL_SAL_APIKEY=mock WXCTL_SAL_ENGINE_ID=mock WXCTL_SAL_PROJECT_ID=mock
export WXCTL_SAL_ENRICH_CATALOG=mock WXCTL_SAL_ENRICH_SCHEMA=mock WXCTL_SAL_ENRICH_TABLE=mock
wxctl plan    -f config.yaml   # preview the DAG; placeholders ok, reconciliation reads the catalog (needs a CP4D profile)
wxctl apply   -f config.yaml   # enable SAL, upload the glossary (-> SUCCEEDED), run enrichment (-> Completed)
wxctl destroy -f config.yaml   # deletes only the IKC integration; the SAL chain retains (see below)
```

On a cluster where SAL is already enabled, `apply` reconciles `integration` +
`sal_integration` + the two settings kinds as **NoChange** (idempotent enablement)
and re-runs the two action-create kinds (`sal_glossary` upload → `SUCCEEDED`,
`sal_enrichment_job` → `Completed`). A second `apply` is therefore idempotent-
equivalent: 0 errors, no new persistent resource.

## Destroy semantics

The five SAL kinds (`sal_integration`, `sal_glossary`, `sal_global_settings`,
`sal_enrichment_settings`, `sal_enrichment_job`) set `on_destroy: retain` — none
exposes a DELETE endpoint, so they persist: SAL stays enabled, the uploaded
glossary terms persist, and the enrichment results remain. Only `kind:
integration` (the IKC registration, which *does* expose a DELETE endpoint and is
not a SAL kind) is removed on `destroy` (live: `-1 deleted`, 0 errors). To fully
reset SAL (and clear the glossary store) you must disable SAL and let it settle to
`status: missing`.
