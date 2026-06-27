# object-storage-setup — a governed COS landing zone for retail sales data

> A retail data team wants a single object-storage landing zone for daily sales
> CSVs that analysts can immediately discover and query — a Cloud Object Storage
> bucket plus a governed catalog, declared once and reproducible across
> environments. This is the minimal "COS bucket + object + Knowledge Catalog"
> shape, end to end.

## The brief

The plain-English use case this example is built from (kept verbatim in
[`use-case.txt`](use-case.txt)):

> Stand up a retail sales object-storage landing zone: an IBM Cloud Object Storage bucket holding a daily sales CSV, registered as a governed Watson Knowledge Catalog so analysts can find and query it.

## What it provisions

[`config.yaml`](config.yaml) declares **four resources** and **no tests** — this
is a *data example*. The functional assertion is the live `apply` succeeding (the
bucket exists, the object is uploaded, the catalog is created) and a clean
`destroy`, not a `kind: test` check (the test runtime targets agents/deployments/
flows, not data resources).

- `kind: storage_connection` — `sales_cos`, an IBM COS connection (`type: ibm_cos`);
  its HMAC credentials are `${env:VAR}` placeholders (`WXCTL_COS_ACCESS_KEY`,
  `WXCTL_COS_SECRET_KEY`, `WXCTL_COS_CRN`).
- `kind: s3_bucket` — `sales_bucket`, wired to the connection via
  `${storage_connection.sales_cos}`; `force_destroy: true` so `destroy` empties
  it first.
- `kind: s3_object` — `sales_csv`, an inline retail sales CSV uploaded to the
  bucket (`content:` is inline — no external file).
- `kind: catalog` — `sales_catalog`, a Common Core (Watson Knowledge Catalog)
  catalog backed by the bucket (`bucket.bucket_name` references the bucket,
  `bucket.bucket_type: bmcos_object_storage`).

## Run it

The COS credentials are `${env:VAR}` references, so the three env vars must be
**set before any command** — including `plan`, which errors (`WXCTL-V301`) if a
referenced env var is unset. For offline `plan` they can be any placeholder
value; for a live `apply` they must be **real COS HMAC keys** for a COS instance
in the bucket's region (`region: eu-gb` in `config.yaml`). Use a profile whose
COS instance is in that region (a SaaS watsonx.data profile). Configure a
profile in `~/.wxctl/config.json` (see the
[top-level README](../../README.md)), then from this directory:

```bash
# Offline plan — placeholders are fine, no live credentials or network:
export WXCTL_COS_CRN=mock WXCTL_COS_ACCESS_KEY=mock WXCTL_COS_SECRET_KEY=mock
wxctl plan -f config.yaml

# Live apply/destroy — set REAL COS HMAC keys for a COS instance in region eu-gb:
export WXCTL_COS_CRN=<your COS instance CRN>
export WXCTL_COS_ACCESS_KEY=<HMAC access key> WXCTL_COS_SECRET_KEY=<HMAC secret key>
wxctl apply   -f config.yaml   # creates the bucket, uploads the object, creates the catalog
wxctl destroy -f config.yaml   # empties + deletes the bucket, removes the catalog
```

> **Bucket region must match your COS instance.** `region: eu-gb` in
> `config.yaml` is a literal default; change it to your COS instance's region (a
> mismatch fails at bucket create). The bucket's region is independent of the
> watsonx.data region.

## Expected output

- `apply` creates the connection, bucket, uploaded object, and catalog (4
  resources, 0 errors); a second `apply` reports NoChange.
- `destroy` empties and deletes the bucket and removes the catalog (0 errors).

### Generating `log.jsonl`

By default wxctl writes no log file. To capture the structured JSON log while
running any command, prefix it with two env vars — `RUST_LOG` turns the
operator-log layer on, `WXCTL_LOG_PATH` sends it to a file:

```bash
RUST_LOG=wxctl=info WXCTL_LOG_PATH=log.jsonl wxctl apply -f config.yaml
```

`WXCTL_LOG_PATH` truncates the file each run; add `WXCTL_LOG_APPEND=1` to stream
a full `apply → destroy` lifecycle into one file. The committed
[`log.jsonl`](log.jsonl) is a sanitized capture of a live run.
