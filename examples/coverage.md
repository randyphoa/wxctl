# Example Suite — Kind Coverage Matrix

This example suite covers all **6 watsonx Orchestrate kinds** (`agent`, `knowledge_base`, `tool`,
`model`, `orchestrate_connection`, `toolkit`), **5 Data & AI Common Core kinds**
(`common_core_connection`, `package_extension`, `project`, `software_specification`, `space`),
the Common Core `catalog`, the **3 Cloud Object Storage kinds with a live backend**
(`storage_connection`, `s3_bucket`, `s3_object` — `adls_container`/`gcs_bucket` stay plan-only
for lack of an Azure/GCS backend in any reservation), and **10 watsonx.data lakehouse kinds**
(`storage_registration`, `database_connection`, `database_registration`,
`presto_engine`, `prestissimo_engine`, `db2_engine`, `spark_engine`, `other_engine`, `schema`,
`ingestion_job`) across the scenarios in `examples/`.
The `finance/credit-risk-model` and `finance/credit-risk-governance` examples add the
**watsonx.ai** model-deployment chain and the **OpenScale** governance chain.
The `finance/data-glossary` example covers the **5 IKC/WKC governance-artifact kinds**
(`category`, `business_term`, `business_terms`, `rule`, `rules`); `finance/sal-enrichment`
covers the **6 SAL kinds** (`integration`, `sal_integration`, `sal_glossary`,
`sal_global_settings`, `sal_enrichment_settings`, `sal_enrichment_job`).
The remaining catalog kinds — those needing an Azure/GCS object-storage backend, a local
Python runtime, or watsonx.governance coverage — are deferred to follow-on work; the
Summary below lists them.

> **Plus `test`:** `test` is a meta-kind (not one of the 54 catalog kinds) and is present
> in every example. It is not listed in the table below.

## Coverage table

<!-- 54 catalog kinds, grouped by service in `wxctl resources` order -->

| Kind | Service | Status | Example |
|---|---|---|---|
| `adls_container` | Cloud Object Storage | not covered — no Azure backend in any reservation (plan-only) | |
| `gcs_bucket` | Cloud Object Storage | not covered — no GCS backend in any reservation (plan-only) | |
| `s3_bucket` | Cloud Object Storage | covered | `retail/object-storage-setup` |
| `s3_object` | Cloud Object Storage | covered | `retail/object-storage-setup` |
| `storage_connection` | Cloud Object Storage | covered | `retail/object-storage-setup` |
| `catalog` | Data & AI Common Core | covered | `retail/object-storage-setup` |
| `common_core_connection` | Data & AI Common Core | covered | `cross-industry/analytics-workspace` |
| `package_extension` | Data & AI Common Core | covered | `cross-industry/analytics-workspace`, `finance/credit-risk-model` |
| `project` | Data & AI Common Core | covered | `cross-industry/analytics-workspace` |
| `software_specification` | Data & AI Common Core | covered | `cross-industry/analytics-workspace`, `finance/credit-risk-model`, `finance/credit-risk-governance` |
| `space` | Data & AI Common Core | covered | `cross-industry/analytics-workspace`, `finance/credit-risk-model`, `finance/credit-risk-governance` |
| `python_script` | Local | not covered — needs live services (follow-on) | |
| `data_mart` | OpenScale | covered | `finance/credit-risk-governance` |
| `data_set` | OpenScale | not covered — schema + handler ship; no example yet | |
| `guardrails_policy` | OpenScale | not covered — schema + handler ship; no example yet | |
| `integrated_system` | OpenScale | not covered — schema + handler ship; no example yet | |
| `monitor_definition` | OpenScale | not covered — schema + handler ship; no example yet | |
| `monitor_instance` | OpenScale | covered | `finance/credit-risk-governance` |
| `service_provider` | OpenScale | covered | `finance/credit-risk-governance` |
| `subscription` | OpenScale | covered | `finance/credit-risk-governance` |
| `inventory` | Factsheets | not covered — schema + handler ship; no example yet | |
| `model_entry` | Factsheets | not covered — schema + handler ship; no example yet | |
| `agent` | watsonx Orchestrate | covered | `cross-industry/board-red-team` (also every other example) |
| `knowledge_base` | watsonx Orchestrate | covered | `healthcare/member-services` (and others) |
| `model` | watsonx Orchestrate | covered | `finance/retail-bank-support`, `finance/credit-risk-governance` |
| `orchestrate_connection` | watsonx Orchestrate | covered | `finance/ap-processing`, `finance/credit-risk-governance` |
| `tool` | watsonx Orchestrate | covered | `healthcare/member-services` (and others), `finance/credit-risk-governance` |
| `toolkit` | watsonx Orchestrate | covered | `cross-industry/board-red-team` |
| `ai_service` | watsonx.ai | covered | `finance/credit-risk-model` |
| `wml_deployment` | watsonx.ai | covered | `finance/credit-risk-model`, `finance/credit-risk-governance` |
| `wml_function` | watsonx.ai | covered | `finance/credit-risk-model`, `finance/credit-risk-governance` |
| `wml_script` | watsonx.ai | covered | `finance/credit-risk-model` |
| `business_term` | watsonx.data | covered | `finance/data-glossary` |
| `business_terms` | watsonx.data | covered | `finance/data-glossary` |
| `category` | watsonx.data | covered | `finance/data-glossary` |
| `database_connection` | watsonx.data | covered | `retail/lakehouse-analytics` |
| `database_registration` | watsonx.data | covered | `retail/lakehouse-analytics` |
| `db2_engine` | watsonx.data | covered | `retail/lakehouse-engines` |
| `ingestion_job` | watsonx.data | covered | `retail/lakehouse-analytics` |
| `integration` | watsonx.data | covered | `finance/sal-enrichment` |
| `milvus_service` | watsonx.data | not covered — deferred (skipped) | |
| `other_engine` | watsonx.data | covered | `retail/lakehouse-engines` |
| `prestissimo_engine` | watsonx.data | covered | `retail/lakehouse-engines` |
| `presto_engine` | watsonx.data | covered | `retail/lakehouse-analytics` |
| `rule` | watsonx.data | covered | `finance/data-glossary` |
| `rules` | watsonx.data | covered | `finance/data-glossary` |
| `sal_enrichment_job` | watsonx.data | covered | `finance/sal-enrichment` |
| `sal_enrichment_settings` | watsonx.data | covered | `finance/sal-enrichment` |
| `sal_global_settings` | watsonx.data | covered | `finance/sal-enrichment` |
| `sal_glossary` | watsonx.data | covered | `finance/sal-enrichment` |
| `sal_integration` | watsonx.data | covered | `finance/sal-enrichment` |
| `schema` | watsonx.data | covered | `retail/lakehouse-analytics` |
| `spark_engine` | watsonx.data | covered | `retail/lakehouse-analytics` |
| `storage_registration` | watsonx.data | covered | `retail/lakehouse-analytics` |

## Summary

**Covered: 44 of 54 catalog kinds** — 6 watsonx Orchestrate (`agent`, `knowledge_base`, `tool`, `model`, `orchestrate_connection`, `toolkit`) + 5 Data & AI Common Core (`common_core_connection`, `package_extension`, `project`, `software_specification`, `space`) + the Common Core `catalog` + 3 Cloud Object Storage (`storage_connection`, `s3_bucket`, `s3_object`) + 10 watsonx.data lakehouse (`storage_registration`, `database_connection`, `database_registration`, `presto_engine`, `prestissimo_engine`, `db2_engine`, `spark_engine`, `other_engine`, `schema`, `ingestion_job`) + 4 watsonx.ai (`ai_service`, `wml_deployment`, `wml_function`, `wml_script`) + 4 OpenScale (`data_mart`, `monitor_instance`, `service_provider`, `subscription`) + 5 watsonx.data IKC/WKC governance artifacts (`category`, `business_term`, `business_terms`, `rule`, `rules`) + 6 watsonx.data SAL (`integration`, `sal_integration`, `sal_glossary`, `sal_global_settings`, `sal_enrichment_settings`, `sal_enrichment_job`) (+ `test` meta-kind in every example).
The retail group adds 14 newly-covered kinds: `retail/object-storage-setup` covers the COS chain (`storage_connection`, `s3_bucket`, `s3_object`) + the Common Core `catalog`; `retail/lakehouse-analytics` covers `storage_registration`, `database_connection`, `database_registration`, `presto_engine`, `spark_engine`, `schema`, `ingestion_job`; `retail/lakehouse-engines` covers `prestissimo_engine`, `db2_engine`, `other_engine`.
The MLOps group adds 8 newly-covered kinds: `finance/credit-risk-model` covers the watsonx.ai asset chain (`ai_service`, `wml_function`, `wml_script`, `wml_deployment`) on a `space` + custom `software_specification`/`package_extension`; `finance/credit-risk-governance` adds the OpenScale governance chain (`service_provider`, `data_mart`, `subscription`, `monitor_instance`).
The finance governance group adds 11 newly-covered kinds: `finance/data-glossary` covers `category`/`business_term`/`business_terms`/`rule`/`rules`; `finance/sal-enrichment` covers `integration` + the SAL chain (`sal_integration`, `sal_glossary`, `sal_global_settings`, `sal_enrichment_settings`, `sal_enrichment_job`).
**Deferred: 10 of 54** catalog kinds — `adls_container`/`gcs_bucket` are plan-only (no Azure/GCS backend in any reservation); `milvus_service` is skipped; `python_script` (Local) is a follow-on; and the watsonx.governance kinds `data_set`/`guardrails_policy`/`integrated_system`/`monitor_definition` (OpenScale) and `inventory`/`model_entry` (Factsheets) ship with schemas and handlers but are not yet exercised by an example.
