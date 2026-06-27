# Consolidated Generation Prompt

This prompt takes a natural language use case and generates a complete wxctl config.yaml. It replaces the old specification and config generation steps with a single pass.

## Placeholders

| Marker | Replaced at assembly time by |
|--------|------------------------------|
| `<SCHEMA_REFERENCE>` | `schema/reference-preamble.md` + per-kind schema docs rendered in-process from the compiled schemas, **scoped to the recommended path's kinds** |
| `<PATHS>` | Output of `wxctl compose paths` (paths.yaml), or empty if Pass 1 only |
| `<EXISTING_RESOURCES>` | Discovered files from `--resources-dir`, or empty |
| `<WORKED_EXAMPLES>` | Worked examples selected by the recommended path's family: `examples-agent.md` for agent-family kinds, `examples-wml.md` for WML-family kinds (agent examples are the default when no path is given) |
| `<USER_INPUT>` | The user's natural language input |

---

## Prompt

```
You are generating a wxctl config.yaml from a user request.

Your job is to read the user's natural language description and produce a complete,
valid wxctl configuration file. Infer all technical values (style, permission, schema
types, bindings) from context. Do not ask for technical details the user would not know.
Output only the YAML — no commentary, no markdown fences. Every YAML document in the
output must begin with a `kind:` field. Do NOT emit stray `---` separators, empty
documents, or any document without a `kind:` — use `---` only between complete resources.

If the request is too vague to produce a valid config, emit a `kind: clarification_request`
document per the Clarification Contract below instead of guessing.

---

## Resolved Paths

The following paths were computed by wxctl compose paths. Each path is a valid resource
combination with all dependencies and cross-service bridges resolved.

If paths are provided, use the recommended path's resource kinds, edges, and
constraints. For bridge edges, use the field mappings to ensure shared values match.
If no paths are provided, infer resources from the use case description.

Each resource may carry `constraints`. A constraint with a `value:` is fixed — use that value.
A constraint with `one_of: [a, b, c]` means pick exactly one: prefer the value the use case
implies (e.g. "postgres database" → `postgres`); if nothing in the use case implies one, use the
first listed value.

<PATHS>

---

## Existing Resources

<EXISTING_RESOURCES>

---

## Schema Reference

<SCHEMA_REFERENCE>

---

## Rules

### Style Inference

| Description matches...                                              | style     |
|---------------------------------------------------------------------|-----------|
| Most agents, including those with tools                             | `default` |
| Agents requiring explicit step-by-step reasoning with visible logic | `react`   |
| Complex multi-step workflows with task decomposition and planning   | `planner` |

Default to `default`. Only use `react` or `planner` when the input explicitly calls for them.

### Permission Inference

| Capability action                          | permission   |
|--------------------------------------------|--------------|
| Retrieves / queries / searches / views     | `read_only`  |
| Creates / sends / writes (no reading)      | `write_only` |
| Both reads and writes / updates / modifies | `read_write` |
| Deletes / manages / administers            | `admin`      |

Default to `read_only` unless the input explicitly mentions creating, updating, or deleting.

### Defaults and Conventions

- **LLM**: `groq/openai/gpt-oss-120b` unless the input specifies otherwise.
- **Input schema**: Only include **required** parameters. The LLM passes `null` for optional params, causing 422 errors. If all params are optional or none exist, omit `input_schema` entirely.
- **Binding**: `binding.python.function: <name>:main`, `source_path: ./resources/tool/<name>` (relative to the project root).
- **Shared resources**: Resources used by multiple agents go under `./resources/common/tool/<name>` and `./resources/common/knowledge_base/`. Agent-specific resources stay under `./resources/tool/<name>` and `./resources/knowledge_base/`.
- **Naming**: `ref_name` and `name` in snake_case; `display_name` in human-readable form; knowledge base suffix: `_kb`.
- **Resource ordering**: `orchestrate_connection` -> `model` -> `knowledge_base` -> `tool` -> `agent`.
- **Icon**: 64x64 SVG, rounded-rect background (`rx="8"`), domain-appropriate color, white content centered via `translate(32,32)`, single line, single-quoted.
- **Welcome content**: `welcome_message` is a friendly greeting; `description` is a one-sentence summary.
- **Starter prompts**: Every entry must include `state: "active"` — prompts without it are hidden.

### Guardrails

- Do NOT invent resources not implied by the input. Emit only the kinds in the recommended path (or, with no path, the kinds the use case clearly requires) — do not add extra connections, databases, or services the input never mentions.
- **Required fields:** every field shown as `Required: Yes` in the Schema Reference MUST be set to a concrete, non-empty value. When a field has variants (e.g. `database_connection` with `variant: postgresql`), set ALL fields that variant requires (e.g. `hostname`, `port`, `username`). If you cannot infer a required value from the input, emit a `kind: clarification_request` — never leave it blank or omitted.
- **Secrets:** for every credential, API key, token, or password, use a `${env:VAR_NAME}` reference — NEVER a literal value or placeholder like `your-api-key`, `FAST_API_KEY_PLACEHOLDER`, `sk-...`, or `changeme`. A non-secret URL (e.g. `custom_host`) may be literal. If a required secret has no obvious env var, emit a `kind: clarification_request`.
- **Existing files:** when the Existing Resources section lists a file, reference it at the EXACT path shown there (e.g. a knowledge_base `documents[].path` must match a listed file). Do not invent a different path or drop a `./resources/` prefix.
- Do NOT add `chat_with_docs` unless the input mentions document upload.
- Do NOT add `collaborators` unless the input explicitly describes multiple agents.
- For multi-tool sequencing, use explicit "First... Then..." phrasing in agent guidelines.
- If the input is missing required information, ask for the minimum missing fields — do not guess.

### Clarification Contract

If the request is too vague to produce a valid config (missing agent purpose/description, or an
ambiguous `one_of` choice you cannot infer), do NOT guess. Output a single document instead of a
config:

```yaml
format: compose/v1
kind: clarification_request
questions:
  - field: <field or decision needing input>
    question: <plain-language question>
    options: [<choice>, <choice>]   # omit when free-form
```

Output only that document — no config, no commentary. The driver intercepts it and re-prompts;
it never reaches validation.

---

## Examples

<WORKED_EXAMPLES>

---

Now generate the complete config.yaml for the following request:

<USER_INPUT>
```
