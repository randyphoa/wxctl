# Example Suite — Kind Coverage Matrix

This example suite covers all **6 watsonx Orchestrate kinds** (`agent`, `knowledge_base`, `tool`,
`model`, `orchestrate_connection`, `toolkit`), **3 Data & AI Common Core kinds**
(`package_extension`, `software_specification`, `space`),
the Common Core `catalog`, the **3 Cloud Object Storage kinds with a live backend**
(`storage_connection`, `s3_bucket`, `s3_object` — `adls_container`/`gcs_bucket` stay plan-only
for lack of an Azure/GCS backend in any reservation), and **10 watsonx.data lakehouse kinds**
(`storage_registration`, `database_connection`, `database_registration`,
`presto_engine`, `prestissimo_engine`, `db2_engine`, `spark_engine`, `other_engine`, `schema`,
`ingestion_job`) across the scenarios in `examples/`.
The `products/watsonx-ai/credit-risk-model` and `solutions/credit-risk-governance` examples add the
**watsonx.ai** model-deployment chain and the **OpenScale** governance chain.
The `products/knowledge-catalog/data-glossary` example covers the **5 IKC/WKC governance-artifact kinds**
(`category`, `business_term`, `business_terms`, `rule`, `rules`); `products/watsonx-data/sal-enrichment`
covers the **6 SAL kinds** (`integration`, `sal_integration`, `sal_glossary`,
`sal_global_settings`, `sal_enrichment_settings`, `sal_enrichment_job`).
The remaining catalog kinds — those needing an Azure/GCS object-storage backend, the
watsonx.governance (OpenScale/Factsheets) kinds, the Common Core workspace kinds
(`project`, `common_core_connection`), the traditional-ML lifecycle chain
(`script_asset`, `notebook`, `environment`, `job`, `job_run`, `asset_promotion`), and the
newer product families
(**IBM Concert**, **IBM Concert Workflows**, **IBM Instana**, **IBM Planning Analytics**) — have
schemas and handlers but no public example yet, and are deferred to follow-on work; the
Summary below lists them.

> **Plus `test`:** `test` is a meta-kind (not one of the 99 catalog kinds) and is present
> in every example. It is not listed in the table below.

## Coverage table

<!-- 99 catalog kinds, grouped by product in `wxctl resources` order -->

| Kind | Service | Status | Example |
|---|---|---|---|
| `inventory` | AI Factsheets | not covered — schema + handler ship; no example yet | |
| `model_entry` | AI Factsheets | not covered — schema + handler ship; no example yet | |
| `model_tracking` | AI Factsheets | not covered — schema + handler ship; no example yet | |
| `adls_container` | Cloud Object Storage | not covered — no Azure backend in any reservation (plan-only) | |
| `gcs_bucket` | Cloud Object Storage | not covered — no GCS backend in any reservation (plan-only) | |
| `s3_bucket` | Cloud Object Storage | covered | `products/watsonx-data/lakehouse-analytics` |
| `s3_object` | Cloud Object Storage | covered | `products/watsonx-data/lakehouse-analytics` |
| `storage_connection` | Cloud Object Storage | covered | `products/watsonx-data/lakehouse-analytics` |
| `asset_promotion` | Data & AI Common Core | not covered — schema + handler ship; no example yet | |
| `business_term` | Data & AI Common Core | covered | `products/knowledge-catalog/data-glossary` |
| `business_terms` | Data & AI Common Core | covered | `products/knowledge-catalog/data-glossary` |
| `catalog` | Data & AI Common Core | covered | `products/watsonx-data/lakehouse-analytics` |
| `category` | Data & AI Common Core | covered | `products/knowledge-catalog/data-glossary` |
| `common_core_connection` | Data & AI Common Core | not covered — schema + handler ship; no example yet | |
| `data_asset` | Data & AI Common Core | not covered — schema + handler ship; no example yet | |
| `environment` | Data & AI Common Core | not covered — schema + handler ship; no example yet | |
| `job` | Data & AI Common Core | not covered — schema + handler ship; no example yet | |
| `job_run` | Data & AI Common Core | not covered — schema + handler ship; no example yet | |
| `package_extension` | Data & AI Common Core | covered | `products/watsonx-ai/credit-risk-model` |
| `project` | Data & AI Common Core | not covered — schema + handler ship; no example yet | |
| `rule` | Data & AI Common Core | covered | `products/knowledge-catalog/data-glossary` |
| `rules` | Data & AI Common Core | covered | `products/knowledge-catalog/data-glossary` |
| `script_asset` | Data & AI Common Core | not covered — schema + handler ship; no example yet | |
| `software_specification` | Data & AI Common Core | covered | `products/watsonx-ai/credit-risk-model`, `solutions/credit-risk-governance` |
| `space` | Data & AI Common Core | covered | `products/watsonx-ai/credit-risk-model`, `solutions/credit-risk-governance` |
| `concert_application` | IBM Concert | not covered — schema + handler ship; no example yet | |
| `concert_automation_rule` | IBM Concert | not covered — schema + handler ship; no example yet | |
| `concert_compliance_profile` | IBM Concert | not covered — schema + handler ship; no example yet | |
| `concert_credential` | IBM Concert | not covered — schema + handler ship; no example yet | |
| `concert_environment` | IBM Concert | not covered — schema + handler ship; no example yet | |
| `concert_ingestion_job` | IBM Concert | not covered — schema + handler ship; no example yet | |
| `concert_resilience_input_data_key` | IBM Concert | not covered — schema + handler ship; no example yet | |
| `concert_resilience_library` | IBM Concert | not covered — schema + handler ship; no example yet | |
| `concert_resilience_posture` | IBM Concert | not covered — schema + handler ship; no example yet | |
| `concert_resilience_profile` | IBM Concert | not covered — schema + handler ship; no example yet | |
| `concert_source_repo` | IBM Concert | not covered — schema + handler ship; no example yet | |
| `concert_worker_group` | IBM Concert Workflows | not covered — schema + handler ship; no example yet | |
| `concert_workflow` | IBM Concert Workflows | not covered — schema + handler ship; no example yet | |
| `concert_workflow_exposure` | IBM Concert Workflows | not covered — schema + handler ship; no example yet | |
| `concert_workflow_role` | IBM Concert Workflows | not covered — schema + handler ship; no example yet | |
| `concert_workflow_schedule` | IBM Concert Workflows | not covered — schema + handler ship; no example yet | |
| `instana_alerting_channel` | IBM Instana | not covered — schema + handler ship; no example yet | |
| `instana_application_alert_config` | IBM Instana | not covered — schema + handler ship; no example yet | |
| `instana_application_perspective` | IBM Instana | not covered — schema + handler ship; no example yet | |
| `instana_maintenance_window` | IBM Instana | not covered — schema + handler ship; no example yet | |
| `instana_slo_alert_config` | IBM Instana | not covered — schema + handler ship; no example yet | |
| `instana_slo_config` | IBM Instana | not covered — schema + handler ship; no example yet | |
| `instana_synthetic_alert_config` | IBM Instana | not covered — schema + handler ship; no example yet | |
| `instana_synthetic_test` | IBM Instana | not covered — schema + handler ship; no example yet | |
| `instana_website_alert_config` | IBM Instana | not covered — schema + handler ship; no example yet | |
| `instana_website_config` | IBM Instana | not covered — schema + handler ship; no example yet | |
| `pa_chore` | IBM Planning Analytics | not covered — schema + handler ship; no example yet | |
| `pa_cube` | IBM Planning Analytics | not covered — schema + handler ship; no example yet | |
| `pa_dimension` | IBM Planning Analytics | not covered — schema + handler ship; no example yet | |
| `pa_group` | IBM Planning Analytics | not covered — schema + handler ship; no example yet | |
| `pa_hierarchy` | IBM Planning Analytics | not covered — schema + handler ship; no example yet | |
| `pa_process` | IBM Planning Analytics | not covered — schema + handler ship; no example yet | |
| `pa_sql_data_source` | IBM Planning Analytics | not covered — schema + handler ship; no example yet | |
| `pa_subset` | IBM Planning Analytics | not covered — schema + handler ship; no example yet | |
| `pa_user` | IBM Planning Analytics | not covered — schema + handler ship; no example yet | |
| `pa_view` | IBM Planning Analytics | not covered — schema + handler ship; no example yet | |
| `data_mart` | OpenScale | covered | `solutions/credit-risk-governance` |
| `data_set` | OpenScale | not covered — schema + handler ship; no example yet | |
| `guardrails_policy` | OpenScale | not covered — schema + handler ship; no example yet | |
| `integrated_system` | OpenScale | not covered — schema + handler ship; no example yet | |
| `monitor_definition` | OpenScale | not covered — schema + handler ship; no example yet | |
| `monitor_instance` | OpenScale | covered | `solutions/credit-risk-governance` |
| `service_provider` | OpenScale | covered | `solutions/credit-risk-governance` |
| `subscription` | OpenScale | covered | `solutions/credit-risk-governance` |
| `agent` | watsonx Orchestrate | covered | `products/watsonx-orchestrate/hr-chatbot` (and others) |
| `knowledge_base` | watsonx Orchestrate | covered | `products/watsonx-orchestrate/hr-chatbot` (and others) |
| `model` | watsonx Orchestrate | covered | `products/watsonx-orchestrate/retail-bank-support`, `solutions/credit-risk-governance` |
| `orchestrate_connection` | watsonx Orchestrate | covered | `products/watsonx-orchestrate/ap-processing`, `solutions/credit-risk-governance` |
| `tool` | watsonx Orchestrate | covered | `products/watsonx-orchestrate/hr-chatbot` (and others), `solutions/credit-risk-governance` |
| `toolkit` | watsonx Orchestrate | covered | `products/watsonx-orchestrate/ops-mcp-assistant` |
| `ai_service` | watsonx.ai | covered | `products/watsonx-ai/credit-risk-model` |
| `autoai_experiment` | watsonx.ai | not covered — schema + handler ship; no example yet | |
| `notebook` | watsonx.ai | not covered — schema + handler ship; no example yet | |
| `wml_deployment` | watsonx.ai | covered | `products/watsonx-ai/credit-risk-model`, `solutions/credit-risk-governance` |
| `wml_function` | watsonx.ai | covered | `products/watsonx-ai/credit-risk-model`, `solutions/credit-risk-governance` |
| `wml_model` | watsonx.ai | not covered — schema + handler ship; no example yet | |
| `wml_script` | watsonx.ai | covered | `products/watsonx-ai/credit-risk-model` |
| `database_connection` | watsonx.data | covered | `products/watsonx-data/lakehouse-analytics` |
| `database_registration` | watsonx.data | covered | `products/watsonx-data/lakehouse-analytics` |
| `db2_engine` | watsonx.data | covered | `products/watsonx-data/lakehouse-engines` |
| `ingestion_job` | watsonx.data | covered | `products/watsonx-data/lakehouse-analytics` |
| `integration` | watsonx.data | covered | `products/watsonx-data/sal-enrichment` |
| `milvus_service` | watsonx.data | not covered — deferred (skipped) | |
| `other_engine` | watsonx.data | covered | `products/watsonx-data/lakehouse-engines` |
| `prestissimo_engine` | watsonx.data | covered | `products/watsonx-data/lakehouse-engines` |
| `presto_engine` | watsonx.data | covered | `products/watsonx-data/lakehouse-analytics` |
| `sal_enrichment_job` | watsonx.data | covered | `products/watsonx-data/sal-enrichment` |
| `sal_enrichment_settings` | watsonx.data | covered | `products/watsonx-data/sal-enrichment` |
| `sal_global_settings` | watsonx.data | covered | `products/watsonx-data/sal-enrichment` |
| `sal_glossary` | watsonx.data | covered | `products/watsonx-data/sal-enrichment` |
| `sal_integration` | watsonx.data | covered | `products/watsonx-data/sal-enrichment` |
| `schema` | watsonx.data | covered | `products/watsonx-data/lakehouse-analytics` |
| `spark_engine` | watsonx.data | covered | `products/watsonx-data/lakehouse-analytics` |
| `storage_registration` | watsonx.data | covered | `products/watsonx-data/lakehouse-analytics` |

## Summary

**Covered: 42 of 99 catalog kinds** — 6 watsonx Orchestrate (`agent`, `knowledge_base`, `tool`, `model`, `orchestrate_connection`, `toolkit`) + 3 Data & AI Common Core (`package_extension`, `software_specification`, `space`) + the Common Core `catalog` + 3 Cloud Object Storage (`storage_connection`, `s3_bucket`, `s3_object`) + 10 watsonx.data lakehouse (`storage_registration`, `database_connection`, `database_registration`, `presto_engine`, `prestissimo_engine`, `db2_engine`, `spark_engine`, `other_engine`, `schema`, `ingestion_job`) + 4 watsonx.ai (`ai_service`, `wml_deployment`, `wml_function`, `wml_script`) + 4 OpenScale (`data_mart`, `monitor_instance`, `service_provider`, `subscription`) + 5 Data & AI Common Core IKC/WKC governance artifacts (`category`, `business_term`, `business_terms`, `rule`, `rules`) + 6 watsonx.data SAL (`integration`, `sal_integration`, `sal_glossary`, `sal_global_settings`, `sal_enrichment_settings`, `sal_enrichment_job`) (+ `test` meta-kind in every example).
The watsonx.data examples add 14 newly-covered kinds: `products/watsonx-data/lakehouse-analytics` covers the COS chain (`storage_connection`, `s3_bucket`, `s3_object`) + the Common Core `catalog` + `storage_registration`, `database_connection`, `database_registration`, `presto_engine`, `spark_engine`, `schema`, `ingestion_job`; `products/watsonx-data/lakehouse-engines` covers `prestissimo_engine`, `db2_engine`, `other_engine`.
The MLOps group adds 8 newly-covered kinds: `products/watsonx-ai/credit-risk-model` covers the watsonx.ai asset chain (`ai_service`, `wml_function`, `wml_script`, `wml_deployment`) on a `space` + custom `software_specification`/`package_extension`; `solutions/credit-risk-governance` adds the OpenScale governance chain (`service_provider`, `data_mart`, `subscription`, `monitor_instance`).
The finance governance group adds 11 newly-covered kinds: `products/knowledge-catalog/data-glossary` covers `category`/`business_term`/`business_terms`/`rule`/`rules`; `products/watsonx-data/sal-enrichment` covers `integration` + the SAL chain (`sal_integration`, `sal_glossary`, `sal_global_settings`, `sal_enrichment_settings`, `sal_enrichment_job`).
**Deferred: 57 of 99** catalog kinds — `adls_container`/`gcs_bucket` are plan-only (no Azure/GCS backend in any reservation); `milvus_service` is skipped; the watsonx.ai `autoai_experiment`/`wml_model`/`notebook`, the Common Core `data_asset`/`project`/`common_core_connection` and the traditional-ML lifecycle kinds `script_asset`/`environment`/`job`/`job_run`/`asset_promotion`, the watsonx.governance kinds `data_set`/`guardrails_policy`/`integrated_system`/`monitor_definition` (OpenScale) and `inventory`/`model_entry`/`model_tracking` (AI Factsheets) ship with schemas and handlers but are not yet exercised by an example; and the newer product families — **IBM Concert** (11 kinds), **IBM Concert Workflows** (5), **IBM Instana** (10), and **IBM Planning Analytics** (10) — are catalog-listed with full handlers but have no public example yet.
