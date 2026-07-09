# lakehouse-analytics — a retail sales lakehouse on watsonx.data (SaaS)

> A retail analytics team wants to turn daily sales CSVs landing in object
> storage into a queryable lakehouse — register the bucket as an Iceberg
> catalog, sit a Db2 federated catalog alongside it, query both with Presto and
> Spark, and load the CSV into an Iceberg table with a Spark ingestion job. This
> is the full "COS → Iceberg → Presto/Spark → ingest" shape, end to end.

## The brief

The plain-English use case this example is built from (kept verbatim in
[`use-case.txt`](use-case.txt)):

> Build a retail sales lakehouse: land daily sales CSVs in a Cloud Object Storage bucket, register it as an Iceberg catalog alongside a Db2 federated catalog, query both with Presto and Spark, and run a Spark ingestion job that loads the CSV into an Iceberg table for analytics.

## What it provisions

[`config.yaml`](config.yaml) declares **eleven resources** and **no tests** —
this is a *data example*. The functional assertion is the live `apply` succeeding
(bucket, object, catalogs, registrations, and engines created) **and the
`ingestion_job` reaching completion** (the Spark job actually loads the CSV into
the Iceberg table), plus a clean `destroy` — not a `kind: test` check (the test
runtime targets agents/deployments/flows, not data resources).

- `kind: storage_connection` — `sales_cos`, an IBM COS connection (`type: ibm_cos`);
  HMAC creds are `${env:VAR}` placeholders (`WXCTL_COS_ACCESS_KEY`,
  `WXCTL_COS_SECRET_KEY`, `WXCTL_COS_CRN`).
- `kind: s3_bucket` — `sales_bucket`, wired to the connection; `force_destroy: true`.
- `kind: s3_object` — `sales_csv`, an inline retail sales CSV uploaded to the bucket.
- `kind: catalog` — `sales_catalog`, a Common Core (WKC) catalog over the bucket.
- `kind: storage_registration` — `sales_iceberg`, registers the bucket as an
  Iceberg catalog in watsonx.data.
- `kind: database_connection` — `sales_db2`, a Db2 federation connection
  (`${env:WXCTL_DB2_*}` placeholders).
- `kind: database_registration` — `sales_db2_catalog`, the Db2 catalog.
- `kind: presto_engine` — `sales_presto`, over **both** catalogs
  (`associated_catalogs`); sizing is env-driven (see below).
- `kind: spark_engine` — `sales_spark`, with `engine_home.storage_name` → the bucket.
- `kind: schema` — `sales_schema`, in the Iceberg catalog, on the Presto engine.
- `kind: ingestion_job` — `sales_ingest`, a Spark job loading the COS CSV into
  the Iceberg `daily_sales` table.

## Run it

All `${env:VAR}` references must be **set before any command** — including `plan`,
which errors (`WXCTL-V301`) if a referenced env var is unset. For offline `plan`
they can be any placeholder value; for a live `apply` they must be **real** COS
HMAC keys (for a COS instance in the bucket's region — `region: eu-gb` in
`config.yaml`), Db2 federation creds, and the tenant's Presto sizing. Use a SaaS
watsonx.data profile whose COS instance is in the bucket's region. Configure a
profile in `~/.wxctl/profiles.yaml` (see the [top-level README](../../../../README.md)),
then from this directory:

```bash
# Offline plan — placeholders are fine, no live credentials or network:
export WXCTL_COS_CRN=mock WXCTL_COS_ACCESS_KEY=mock WXCTL_COS_SECRET_KEY=mock
export WXCTL_DB2_HOST=mock WXCTL_DB2_USERNAME=mock WXCTL_DB2_PASSWORD=mock
export WXCTL_PRESTO_SIZE_CONFIG=mock WXCTL_PRESTO_NODE_TYPE=mock
wxctl plan -f config.yaml

# Live apply/destroy — set REAL creds + the tenant's Presto sizing:
export WXCTL_COS_CRN=<your COS instance CRN>
export WXCTL_COS_ACCESS_KEY=<HMAC access key> WXCTL_COS_SECRET_KEY=<HMAC secret key>
export WXCTL_DB2_HOST=<db2 host> WXCTL_DB2_USERNAME=<user> WXCTL_DB2_PASSWORD=<pw>
export WXCTL_PRESTO_SIZE_CONFIG=<tenant size_config> WXCTL_PRESTO_NODE_TYPE=<tenant node profile>
wxctl apply   -f config.yaml   # creates all 11; the ingestion_job loads the CSV
wxctl destroy -f config.yaml   # tears everything down (force_destroy empties the bucket)
```

> **Presto sizing is paired per-tenant.** `WXCTL_PRESTO_SIZE_CONFIG` and
> `WXCTL_PRESTO_NODE_TYPE` must match your tenant: SaaS (MCSP) wants `custom` +
> a tier name; classic/Software wants `starter` + a VPC node profile. A mismatch
> 400s at engine create.

> **Bucket region must match your COS instance.** `region: eu-gb` in
> `config.yaml` is a literal default; change it to your COS instance's region.

## Expected output

- `apply` creates all 11 resources (0 errors); the `ingestion_job` reaches a
  completed state (the CSV is loaded into the Iceberg table); a second `apply`
  reports NoChange.
- `destroy` removes all 11 (0 errors); `force_destroy: true` empties the bucket
  first.

### Generating `log.jsonl`

By default wxctl writes no log file. To capture the structured JSON log while
running any command, prefix it with two env vars — `RUST_LOG` turns the
operator-log layer on, `WXCTL_LOG_PATH` sends it to a file:

```bash
RUST_LOG=wxctl=info WXCTL_LOG_PATH=log.jsonl wxctl apply -f config.yaml
```

`WXCTL_LOG_PATH` truncates the file each run; add `WXCTL_LOG_APPEND=1` to stream
a full `apply → destroy` lifecycle into one file. A sanitized capture of a live
run is committed as `log.jsonl` once the live run is performed.
