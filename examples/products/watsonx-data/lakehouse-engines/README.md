# lakehouse-engines — a retail sales lakehouse with the full engine zoo (Software)

> A retail platform team running watsonx.data on Software (CP4D) wants the whole
> query stack over their lakehouse — register the COS bucket as an Iceberg
> catalog with a Db2 federated catalog alongside, stand up Presto, Prestissimo,
> an external Db2 engine, and a generic external engine, and load a CSV in with a
> Spark ingestion job. This is the "storage_registration + engine zoo + ingest"
> shape, end to end, on Software.

## The brief

The plain-English use case this example is built from (kept verbatim in
[`use-case.txt`](use-case.txt)):

> Stand up a retail sales lakehouse on Software watsonx.data: register a Cloud Object Storage bucket as an Iceberg catalog with a Db2 federated catalog alongside, run the full engine zoo (Presto, Prestissimo, external Db2, and a generic external engine), and load a CSV into the lakehouse with a Spark ingestion job.

## What it provisions

[`config.yaml`](config.yaml) declares **ten resources** and **no tests** — this
is a *data example*. The functional assertion is the live `apply` succeeding
(bucket registered, catalogs created, the four engine kinds reaching running)
**and the `ingestion_job` reaching completion**, plus a clean `destroy` — not a
`kind: test` check (the test runtime targets agents/deployments/flows, not data
resources).

- `kind: storage_connection` — `sales_cos`, an IBM COS connection; HMAC creds are
  `${env:VAR}` placeholders.
- `kind: s3_bucket` — `sales_bucket`, region from `${env:WXCTL_COS_REGION}`;
  `force_destroy: true`.
- `kind: storage_registration` — `sales_iceberg`, the registration of
  the bucket as an Iceberg catalog.
- `kind: database_connection` — `sales_db2`, a Db2 federation connection.
- `kind: database_registration` — `sales_db2_catalog`, the Db2 catalog.
- `kind: presto_engine` — `sales_presto`, over the Db2 catalog.
- `kind: prestissimo_engine` — `sales_prestissimo`, native C++ Presto.
- `kind: db2_engine` — `sales_db2_engine`, an external Db2 engine.
- `kind: other_engine` — `sales_other_engine`, a generic external engine.
- `kind: ingestion_job` — `sales_ingest`, a Spark job; its target engine, bucket,
  catalog, schema, and table reference **pre-existing** cluster resources via
  `${env:WXCTL_SOFTWARE_INGEST_*}` (the ingestion target isn't bootstrapped by
  this DAG).

## Run it

All `${env:VAR}` references must be **set before any command** — including `plan`,
which errors (`WXCTL-V301`) if a referenced env var is unset. For offline `plan`
they can be any placeholder value; for a live `apply` they must be **real** COS
HMAC keys + region, Db2 creds, the tenant's Presto sizing, and the
`WXCTL_SOFTWARE_INGEST_*` targets that already exist on the cluster. Use a
Software (CP4D) watsonx.data profile with a reachable COS bucket. Configure a
profile in `~/.wxctl/profiles.yaml` (see the [top-level README](../../../../README.md)),
then from this directory:

```bash
# Offline plan — placeholders are fine, no live credentials or network:
export WXCTL_COS_CRN=mock WXCTL_COS_ACCESS_KEY=mock WXCTL_COS_SECRET_KEY=mock WXCTL_COS_REGION=mock
export WXCTL_DB2_HOST=mock WXCTL_DB2_USERNAME=mock WXCTL_DB2_PASSWORD=mock WXCTL_DB2_DATABASE=mock
export WXCTL_PRESTO_SIZE_CONFIG=mock WXCTL_PRESTO_NODE_TYPE=mock
export WXCTL_SOFTWARE_INGEST_ENGINE_ID=mock WXCTL_SOFTWARE_INGEST_SOURCE=mock
export WXCTL_SOFTWARE_INGEST_BUCKET_NAME=mock WXCTL_SOFTWARE_INGEST_BUCKET_ENDPOINT=mock
export WXCTL_SOFTWARE_INGEST_CATALOG=mock WXCTL_SOFTWARE_INGEST_SCHEMA=mock WXCTL_SOFTWARE_INGEST_TABLE=mock
wxctl plan -f config.yaml

# Live apply/destroy — set REAL creds, Software Presto sizing, and ingest targets:
export WXCTL_COS_CRN=<COS CRN> WXCTL_COS_ACCESS_KEY=<key> WXCTL_COS_SECRET_KEY=<secret> WXCTL_COS_REGION=<region>
export WXCTL_DB2_HOST=<db2 host> WXCTL_DB2_USERNAME=<user> WXCTL_DB2_PASSWORD=<pw> WXCTL_DB2_DATABASE=<db>
export WXCTL_PRESTO_SIZE_CONFIG=starter WXCTL_PRESTO_NODE_TYPE=<VPC node profile>
export WXCTL_SOFTWARE_INGEST_ENGINE_ID=<spark engine id> WXCTL_SOFTWARE_INGEST_SOURCE=<s3://...csv>
export WXCTL_SOFTWARE_INGEST_BUCKET_NAME=<bucket> WXCTL_SOFTWARE_INGEST_BUCKET_ENDPOINT=<https endpoint>
export WXCTL_SOFTWARE_INGEST_CATALOG=<catalog> WXCTL_SOFTWARE_INGEST_SCHEMA=<schema> WXCTL_SOFTWARE_INGEST_TABLE=<table>
wxctl apply   -f config.yaml   # registers the bucket, creates catalogs + 4 engines, runs the ingestion
wxctl destroy -f config.yaml   # tears everything down (force_destroy empties the bucket)
```

> **Software Presto sizing.** `WXCTL_PRESTO_SIZE_CONFIG=starter` paired with a
> tenant VPC node profile in `WXCTL_PRESTO_NODE_TYPE`; a mismatch 400s at engine
> create.

## Expected output

- `apply` creates all 10 resources (0 errors), with the four engine kinds
  (`presto_engine`, `prestissimo_engine`, `db2_engine`, `other_engine`) reaching
  running and the `ingestion_job` reaching completion; a second `apply` reports
  NoChange.
- `destroy` removes all 10 (0 errors); `force_destroy: true` empties the bucket.

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
