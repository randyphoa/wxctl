# Examples — use case → config → execute

Each subdirectory is **one use case**: a plain-English brief, the `config.yaml` it
turns into, and the resources that brief needs. Point `wxctl` at the config and it
plans, applies, tests, and tears the use case back down.

The pattern, every time:

```
1. Describe the use case   →  a sentence (use-case.txt)
2. Turn it into config      →  config.yaml         (compose, or write it by hand)
3. Execute it               →  wxctl plan | apply | test | destroy
```

Step 2 is what `wxctl` does for you: its compose tools read the sentence in
`use-case.txt` and emit the `config.yaml` you see here — the declared resources, the
`${kind.ref}` wiring between them, and the `kind: test` checks at the end. You can
regenerate it from the sentence, edit it, or write one from scratch; the execute step
is identical either way.

## Examples

Grouped by industry; the **Tier** badge marks complexity (Light → Heavy).

| Industry | Example | Tier | Use case | Resources |
| --- | --- | --- | --- | --- |
| Finance | [`finance/retail-bank-support/`](finance/retail-bank-support/) | Medium | Retail-bank support agent answering product/fee/disclosure questions and looking up accounts & transactions, on a custom gateway model | `agent` · `model` · `knowledge_base` · `tool` ×2 · `orchestrate_connection` |
| Finance | [`finance/ap-processing/`](finance/ap-processing/) | Medium | Invoice & AP-processing agent: 3-way matching, duplicate detection, exception routing | `agent` · `knowledge_base` · `tool` ×2 · `orchestrate_connection` |
| Finance | [`finance/credit-risk-model/`](finance/credit-risk-model/) | Medium | Deploy a transparent credit-risk scoring model on watsonx.ai — a custom software specification built from a package extension, three deployable scorer forms, and an online deployment — then score a loan applicant for an approve/decline decision and probability | `space` · `package_extension` · `software_specification` · `wml_function` · `wml_script` · `ai_service` · `wml_deployment` |
| Finance | [`finance/credit-risk-governance/`](finance/credit-risk-governance/) | Heavy | **Four products in one config:** deploy a credit-risk model on watsonx.ai, govern it with a watsonx.governance OpenScale quality monitor, and front it with a watsonx Orchestrate loan-decision agent that scores applicants by calling the *same* governed deployment — the agent's own chat model is gpt-oss-120b served by watsonx.ai | `space` · `software_specification` · `wml_function` · `wml_deployment` · `service_provider` · `data_mart` · `subscription` · `monitor_instance` · `orchestrate_connection` ×2 · `model` · `tool` · `agent` |
| Finance | [`finance/data-glossary/`](finance/data-glossary/) | Heavy | Build a customer-data governance taxonomy on the IBM Knowledge Catalog / watsonx.data common-core governance surface: a "Customer Data" category hierarchy, a "Customer Identifier" business term, a bulk CSV import of further terms, a data-protection rule that restricts access to assets classified with the Customer Identifier, and a bulk set of further access-enforcement rules | `category` ×2 · `business_term` · `business_terms` · `rule` · `rules` |
| Finance | [`finance/sal-enrichment/`](finance/sal-enrichment/) | Heavy | Enable the watsonx.data Semantic Automation Layer (SAL) and auto-enrich a customer table against a business glossary: register IBM Knowledge Catalog, enable SAL, upload a glossary, set global + per-project enrichment defaults, and run an enrichment job (Software / CP4D only) | `integration` · `sal_integration` · `sal_glossary` · `sal_global_settings` · `sal_enrichment_settings` · `sal_enrichment_job` |
| Healthcare | [`healthcare/member-services/`](healthcare/member-services/) | Light | Health-plan member-services agent answering benefits questions and looking up claim status | `agent` · `knowledge_base` · `tool` |
| Insurance | [`insurance/claims-intake-flow/`](insurance/claims-intake-flow/) | Medium | Claims-intake assistant that validates a claim ID & member eligibility with a python tool, looks up reference data via a public OpenAPI tool (each operation expands to its own tool), and computes the required insurance rate from a decisions flow — all on a watsonx.ai-served gateway model | `agent` · `model` · `tool` (python) · `tool` (openapi) · `tool` (flow) · `space` · `orchestrate_connection` |
| Telecommunications | [`telecommunications/customer-care/`](telecommunications/customer-care/) | Light | Telecom customer-care agent answering plan/roaming questions and looking up subscriber plan & usage | `agent` · `knowledge_base` · `tool` |
| Cross-industry | [`cross-industry/hr-chatbot/`](cross-industry/hr-chatbot/) | Light | HR chatbot answering policy questions from the employee handbook and looking up employee records | `agent` · `knowledge_base` · `tool` |
| Cross-industry | [`cross-industry/board-red-team/`](cross-industry/board-red-team/) | Heavy | Multi-agent board "red team": five persona critics + a synthesis coordinator producing a board memo | `agent` ×6 · `knowledge_base` · `tool` · `toolkit` |
| Cross-industry | [`cross-industry/analytics-workspace/`](cross-industry/analytics-workspace/) | Medium | Stand up a deployment-portable analytics workspace: a project with a PostgreSQL data connection, plus a deployment space carrying a custom conda runtime — the same config applies on a watsonx SaaS-WKC profile and on a CP4D / Software profile | `project` · `common_core_connection` · `space` · `package_extension` · `software_specification` |
| Cross-industry | [`cross-industry/model-router/`](cross-industry/model-router/) | Medium | Multi-model AI-gateway routing: two watsonx.ai-served gateway models (gpt-oss-120b + llama-3-3-70b) back a fast triage agent and a deep specialist; a supervisor agent routes by complexity via `collaborators` | `agent` ×3 · `model` ×2 · `space` · `orchestrate_connection` |
| Cross-industry | [`cross-industry/ops-mcp-assistant/`](cross-industry/ops-mcp-assistant/) | Heavy | Ops assistant backed by a custom local MCP server that wxctl packages & uploads (`mcp.source: files`), exposing a service-status lookup over bundled mock data | `agent` · `model` · `toolkit` (local MCP) · `space` · `orchestrate_connection` |
| Retail | [`retail/object-storage-setup/`](retail/object-storage-setup/) | Medium | Stand up a governed Cloud Object Storage landing zone for daily retail sales CSVs: a COS bucket holding the data, surfaced as a Watson Knowledge Catalog so analysts can discover it | `storage_connection` · `s3_bucket` · `s3_object` · `catalog` |
| Retail | [`retail/lakehouse-analytics/`](retail/lakehouse-analytics/) | Heavy | A full SaaS retail sales lakehouse: land sales CSVs in COS, register the bucket as an Iceberg catalog alongside a Db2 federated catalog, query both with Presto and Spark, and run a Spark ingestion job that loads the CSV into an Iceberg table | `storage_connection` · `s3_bucket` · `s3_object` · `catalog` · `storage_registration` · `database_connection` · `database_registration` · `presto_engine` · `spark_engine` · `schema` · `ingestion_job` |
| Retail | [`retail/lakehouse-engines/`](retail/lakehouse-engines/) | Heavy | A Software (CP4D) retail sales lakehouse with the full engine zoo: register a COS bucket as an Iceberg catalog with a Db2 federated catalog, run Presto, Prestissimo, an external Db2 engine, and a generic external engine, and load a CSV with a Spark ingestion job | `storage_connection` · `s3_bucket` · `storage_registration` · `database_connection` · `database_registration` · `presto_engine` · `prestissimo_engine` · `db2_engine` · `other_engine` · `ingestion_job` |

See [`coverage.md`](coverage.md) for which of the 49 catalog kinds the suite exercises.

## Running an example

Every example is self-contained and credential-free — secrets are `${env:VAR}`
placeholders, never literals. To run one, configure a profile in `~/.wxctl/config.json`
(see the [top-level README](../README.md)), export any env vars the example lists, then
from the example directory:

```bash
cd cross-industry/hr-chatbot
wxctl plan    -f config.yaml   # preview the DAG — no credentials needed
wxctl apply   -f config.yaml   # create the resources
wxctl test    -f config.yaml   # run the kind: test checks
wxctl destroy -f config.yaml   # tear it all down
```

`wxctl plan` resolves references and validates without calling any service, so it's the
fastest way to see what a use case builds before you commit credentials.

## Adding your own

Copy [`_template/`](_template/) into a new directory under `examples/<industry>/<name>/`,
picking an existing industry folder (`finance`, `healthcare`,
`telecommunications`, `cross-industry`, `retail`) or creating a new one:

```bash
cp -r _template cross-industry/my-use-case
```

- `use-case.txt` — the one- or two-sentence brief, the way you'd describe it to a colleague.
- `config.yaml` — the resources that brief becomes. Generate it from the sentence with
  the compose tools, or write it by hand. Keep secrets as `${env:VAR}`; relative file
  paths resolve against the config file's directory (two levels under `examples/`).
- `resources/` — any source the config references (tool code, knowledge-base documents).
- `README.md` — the recipe: the brief, what it provisions, how to run it, expected output.
- `log.jsonl` — examples do **not** ship a committed `log.jsonl` by default; generate
  one on a live green run (recipe in the example README) and sanitize any
  account/profile identifiers before committing. (`cross-industry/hr-chatbot/log.jsonl`
  is a pre-existing sanitized snapshot.)

Then add a row to the table above and check [`coverage.md`](coverage.md) to see whether
any new kinds need to be listed there.
