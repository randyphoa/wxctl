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
`use-case.txt` and emit the `config.yaml` you see here, the declared resources, the
`${kind.ref}` wiring between them, and the `kind: test` checks at the end. You can
regenerate it from the sentence, edit it, or write one from scratch; the execute step
is identical either way.

## Layout

Examples sit on a three-tier ladder. The path tells you where an example lands, and
each tier is a step up in scope:

- **`primitives/`** — product-neutral building blocks: the base agent, tool, and
  knowledge-base mechanics that everything else composes from.
- **`products/<product>/`** — one product, one capability. Grouped by the IBM product
  the example exercises (`watsonx-orchestrate`, `watsonx-ai`, `watsonx-data`,
  `knowledge-catalog`).
- **`solutions/`** — one config spanning several products end to end.

```
primitives/
  calculator-weather-agents          starter agents: a Python tool + a knowledge base + collaborators
products/
  watsonx-orchestrate/               agents, tools, knowledge bases, gateway models, MCP toolkits
  watsonx-ai/                        model deployment: functions, scripts, custom software specs
  watsonx-data/                      lakehouse: object storage, catalogs, engines, ingestion, SAL
  knowledge-catalog/                 governance taxonomy: categories, business terms, rules
solutions/
  credit-risk-governance             watsonx.ai + watsonx.governance + watsonx Orchestrate
```

## Skeleton

Every example follows one file set:

| Entry | Role |
|---|---|
| `use-case.txt` | the one- or two-sentence brief, the way you'd describe it to a colleague |
| `config.yaml` | the resources that brief becomes, wired with `${kind.ref}` and closed by `kind: test` |
| `resources/` | any source the config references (tool code, knowledge-base documents, notebooks, data) |
| `README.md` | the recipe: the brief, what it provisions, how to run it, expected output |

Config documents live at the example root; every referenced asset lives under
`resources/`, so `-f <example-dir>` loads the config and leaves the assets out of the
scan. Compose-generated examples may additionally carry a `.compose/` directory holding
generator provenance; hand edits stay outside it.

## Examples

Sorted by tier. The **Industry** column is a discovery aid, not a folder; the **Tier**
badge marks complexity (Light → Heavy).

| Example | Industry | Tier | Use case | Resources |
| --- | --- | --- | --- | --- |
| [`primitives/calculator-weather-agents/`](primitives/calculator-weather-agents/) | Starter | Light | The starter pair: a calculator agent doing arithmetic with a Python tool and answering IBM history questions from a knowledge base, plus a weather agent reporting city weather and delegating math to the calculator agent via `collaborators` | `agent` ×2 · `knowledge_base` · `tool` ×2 |
| [`products/watsonx-orchestrate/hr-chatbot/`](products/watsonx-orchestrate/hr-chatbot/) | Cross-industry | Light | HR chatbot answering policy questions from the employee handbook and looking up employee records | `agent` · `knowledge_base` · `tool` |
| [`products/watsonx-orchestrate/ap-processing/`](products/watsonx-orchestrate/ap-processing/) | Finance | Medium | Invoice & AP-processing agent: 3-way matching, duplicate detection, exception routing | `agent` · `knowledge_base` · `tool` ×2 · `orchestrate_connection` |
| [`products/watsonx-orchestrate/retail-bank-support/`](products/watsonx-orchestrate/retail-bank-support/) | Finance | Medium | Retail-bank support agent answering product/fee/disclosure questions and looking up accounts & transactions, on a custom gateway model | `agent` · `model` · `knowledge_base` · `tool` ×2 · `orchestrate_connection` |
| [`products/watsonx-orchestrate/model-router/`](products/watsonx-orchestrate/model-router/) | Cross-industry | Medium | Multi-model AI-gateway routing: two watsonx.ai-served gateway models (gpt-oss-120b + llama-3-3-70b) back a fast triage agent and a deep specialist; a supervisor agent routes by complexity via `collaborators` | `agent` ×3 · `model` ×2 · `space` · `orchestrate_connection` |
| [`products/watsonx-orchestrate/ops-mcp-assistant/`](products/watsonx-orchestrate/ops-mcp-assistant/) | Cross-industry | Heavy | Ops assistant backed by a custom local MCP server that wxctl packages & uploads (`mcp.source: files`), exposing a service-status lookup over bundled mock data | `agent` · `model` · `toolkit` (local MCP) · `space` · `orchestrate_connection` |
| [`products/watsonx-orchestrate/claims-intake-flow/`](products/watsonx-orchestrate/claims-intake-flow/) | Insurance | Medium | Claims-intake assistant that validates a claim ID & member eligibility with a python tool, looks up reference data via a public OpenAPI tool (each operation expands to its own tool), and computes the required insurance rate from a decisions flow, all on a watsonx.ai-served gateway model | `agent` · `model` · `tool` (python) · `tool` (openapi) · `tool` (flow) · `space` · `orchestrate_connection` |
| [`products/watsonx-ai/credit-risk-model/`](products/watsonx-ai/credit-risk-model/) | Finance | Medium | Deploy a transparent credit-risk scoring model on watsonx.ai, a custom software specification built from a package extension, three deployable scorer forms, and an online deployment, then score a loan applicant for an approve/decline decision and probability | `space` · `package_extension` · `software_specification` · `wml_function` · `wml_script` · `ai_service` · `wml_deployment` |
| [`products/watsonx-data/lakehouse-analytics/`](products/watsonx-data/lakehouse-analytics/) | Retail | Heavy | A full SaaS retail sales lakehouse: land sales CSVs in COS, register the bucket as an Iceberg catalog alongside a Db2 federated catalog, query both with Presto and Spark, and run a Spark ingestion job that loads the CSV into an Iceberg table | `storage_connection` · `s3_bucket` · `s3_object` · `catalog` · `storage_registration` · `database_connection` · `database_registration` · `presto_engine` · `spark_engine` · `schema` · `ingestion_job` |
| [`products/watsonx-data/lakehouse-engines/`](products/watsonx-data/lakehouse-engines/) | Retail | Heavy | A Software (CP4D) retail sales lakehouse with the full engine zoo: register a COS bucket as an Iceberg catalog with a Db2 federated catalog, run Presto, Prestissimo, an external Db2 engine, and a generic external engine, and load a CSV with a Spark ingestion job | `storage_connection` · `s3_bucket` · `storage_registration` · `database_connection` · `database_registration` · `presto_engine` · `prestissimo_engine` · `db2_engine` · `other_engine` · `ingestion_job` |
| [`products/watsonx-data/sal-enrichment/`](products/watsonx-data/sal-enrichment/) | Finance | Heavy | Enable the watsonx.data Semantic Automation Layer (SAL) and auto-enrich a customer table against a business glossary: register IBM Knowledge Catalog, enable SAL, upload a glossary, set global + per-project enrichment defaults, and run an enrichment job (Software / CP4D only) | `integration` · `sal_integration` · `sal_glossary` · `sal_global_settings` · `sal_enrichment_settings` · `sal_enrichment_job` |
| [`products/knowledge-catalog/data-glossary/`](products/knowledge-catalog/data-glossary/) | Finance | Heavy | Build a customer-data governance taxonomy on the IBM Knowledge Catalog / watsonx.data common-core governance surface: a "Customer Data" category hierarchy, a "Customer Identifier" business term, a bulk CSV import of further terms, a data-protection rule that restricts access to assets classified with the Customer Identifier, and a bulk set of further access-enforcement rules | `category` ×2 · `business_term` · `business_terms` · `rule` · `rules` |
| [`solutions/credit-risk-governance/`](solutions/credit-risk-governance/) | Finance | Heavy | **Four products in one config:** deploy a credit-risk model on watsonx.ai, govern it with a watsonx.governance OpenScale quality monitor, and front it with a watsonx Orchestrate loan-decision agent that scores applicants by calling the *same* governed deployment, the agent's own chat model is gpt-oss-120b served by watsonx.ai | `space` · `software_specification` · `wml_function` · `wml_deployment` · `service_provider` · `data_mart` · `subscription` · `monitor_instance` · `orchestrate_connection` ×2 · `model` · `tool` · `agent` |

See [`coverage.md`](coverage.md) for which of the 99 catalog kinds the suite exercises.

## Running an example

Every example is self-contained and credential-free — secrets are `${env:VAR}`
placeholders, never literals. To run one, configure a profile in `~/.wxctl/profiles.yaml`
(see the [top-level README](../README.md)), export any env vars the example lists, then
from the example directory:

```bash
cd products/watsonx-orchestrate/hr-chatbot
wxctl plan    -f config.yaml   # preview the DAG — no credentials needed
wxctl apply   -f config.yaml   # create the resources
wxctl test    -f config.yaml   # run the kind: test checks
wxctl destroy -f config.yaml   # tear it all down
```

`wxctl plan` resolves references and validates without calling any service, so it's the
fastest way to see what a use case builds before you commit credentials.

## Adding your own

Copy [`_template/`](_template/) into a new directory on the ladder, picking the tier
that fits: `primitives/<name>/` (product-neutral), `products/<product>/<name>/`
(single product), or `solutions/<name>/` (spans products):

```bash
cp -r _template products/watsonx-orchestrate/my-use-case
```

- `use-case.txt` — the one- or two-sentence brief, the way you'd describe it to a colleague.
- `config.yaml` — the resources that brief becomes. Generate it from the sentence with
  the compose tools, or write it by hand. Keep secrets as `${env:VAR}`; relative file
  paths resolve against the config file's directory.
- `resources/` — any source the config references (tool code, knowledge-base documents).
- `README.md` — the recipe: the brief, what it provisions, how to run it, expected output.
- `log.jsonl` — examples do **not** ship a committed `log.jsonl` by default; generate
  one on a live green run (recipe in the example README) and sanitize any
  account/profile identifiers before committing.
  (`products/watsonx-orchestrate/hr-chatbot/log.jsonl` is a pre-existing sanitized snapshot.)

Then add a row to the table above and check [`coverage.md`](coverage.md) to see whether
any new kinds need to be listed there.
