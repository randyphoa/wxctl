# data-glossary — customer-data governance taxonomy (categories + terms + rules)

> A retail bank wants a governed vocabulary for customer data: a category
> hierarchy, business terms, and data-protection (access-enforcement) rules —
> created declaratively on the IBM Knowledge Catalog / watsonx.data common-core
> governance surface. This is the governance-artifact lifecycle, end to end, with
> a bulk CSV term import.

**Tier:** Heavy.

## The brief

The plain-English use case this example is built from (kept verbatim in
[`use-case.txt`](use-case.txt)):

> Build a customer-data governance taxonomy for a retail bank: a "Customer Data"
> category hierarchy, a "Customer Identifier" business term organized under it, a
> bulk CSV import of further customer-data glossary terms, a data-protection rule
> that restricts access to assets classified with the Customer Identifier, and a
> bulk set of further access-enforcement rules.

## What it provisions

[`config.yaml`](config.yaml) declares **six resources** (the two `category`
instances form the hierarchy; no `kind: test` — this is a data example,
verification is the live `apply` succeeding):

- `kind: category` ×2 — a root **Customer Data** category and a child **Customer
  PII** linked via `parent_category: ${category.customer_data.artifact_id}` (a
  real hierarchy edge — the one DAG edge here).
- `kind: business_term` — **Customer Identifier**, recorded under the Customer
  Data category via its `parent_category: { id: ${category.customer_data.artifact_id} }`.
- `kind: business_terms` — a **bulk CSV import**
  ([`resources/glossary/terms.csv`](resources/glossary/terms.csv)) of further
  customer-data terms (`merge_option: all`).
- `kind: rule` — a **Restrict Customer Identifier Access** data-protection rule
  (`governance_type_id: Access`; a `trigger` matching assets classified with the
  Customer Identifier term + a `Deny` action).
- `kind: rules` — a **bulk set of inline rules** (`rules:` array) of further
  access-enforcement rules reconciled per-rule.

> **Why a CSV for terms but an inline array for rules?** The business-glossary
> importer consumes a CSV (`Name,Artifact Type,Description` — the SAL/IKC importer
> rejects a `Category` header), wired as an `is_path` field resolved against this
> config's directory. The `rules` kind's `import_file` option, by contrast, targets
> `POST /v3/enforcement/rules/import`, which consumes a rules **export** document
> (the cross-system migration envelope produced by `GET /v3/enforcement/rules/export`,
> carrying a schema `version` and component-id links) — not a hand-authored rule
> list. So this example uses the `rules` kind's inline `rules:` array, which the
> handler reconciles per rule (check exists → PUT or POST). Cross-kind references
> resolve late via `${kind.ref.artifact_id}`.

## Run it

`wxctl plan` validates offline, then reconciliation issues **read-only discovery
GETs** against the catalog (`category`/`business_term`/`rule` are `list_and_get`,
so plan reads `/v3/categories`, `/v3/glossary_terms`, `/v3/enforcement/rules`) —
so `plan` needs a **CP4D / Software profile's credentials** (no secrets live in
this example; they come from your profile). Live `apply`/`destroy` target **IBM
Knowledge Catalog on watsonx.data Software (CP4D)** and additionally need that
profile's runtime user to hold the `glossary_admin` role ("Governance
Administrator"). From this directory:

```bash
wxctl plan    -f config.yaml   # preview the DAG; reconciliation reads the catalog (needs a CP4D profile)
wxctl apply   -f config.yaml   # create the six governance artifacts (bulk imports included)
wxctl destroy -f config.yaml   # delete the artifacts that expose a DELETE endpoint
```

`wxctl plan` resolves references and validates, then reads current state via
read-only discovery — it never mutates anything.

## Destroy semantics

`destroy` removes all six artifacts cleanly (live: `-6 deleted`, 0 errors).
`category`, `business_term`, and `rule` expose DELETE endpoints (`artifact_id` /
`guid`). The bulk `business_terms` / `rules` kinds use `discovery: skip`, so
`destroy` emits an optimistic delete whose `pre_delete` hook resolves each created
term/rule by name → guid and deletes them via their per-item paths — so the bulk
imports do **not** leak. (The SAL example, by contrast, retains on destroy — its
SAL kinds expose no DELETE endpoint; see that README.)
