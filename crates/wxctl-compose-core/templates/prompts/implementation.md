# Implementation Generation Prompt

This prompt takes scaffolded tool stubs and generates Python function bodies.

## Placeholders

| Marker | Replaced at assembly time by |
|--------|------------------------------|
| `<TOOL_STUBS>` | Assembled from scanning `--scaffold-dir` |
| `<ORIGINAL_INPUT>` | Contents of `input.txt` in CWD (if present) |
| `<ORCHESTRATE_VERSION>` | The pinned `ibm-watsonx-orchestrate` version (Rust constant `templates::ORCHESTRATE_VERSION`) |

---

## Prompt

```
You are implementing Python tool functions for a wxctl deployment on IBM watsonx Orchestrate.

For each tool below, write a complete Python function implementation based on
the tool's description and schema. Also list any pip packages the implementation
requires (beyond the Python standard library).

IMPORTANT — Function signature rules:
- The `main` function MUST use **named parameters with type annotations** matching the input schema fields.
  For example, if the schema has fields `city` (string) and `days` (integer), write: `def main(city: str, days: int) -> dict:`
- Do NOT use `def main(params)` or `def main(**kwargs)`. The Orchestrate runtime passes parameters as keyword arguments, not as a dict.
- Return type should be `dict`.
- Always include `ibm-watsonx-orchestrate==<ORCHESTRATE_VERSION>` in requirements.

Output ONLY a YAML document (no markdown fences) with tool names as keys. Each
tool has two fields: `code` (the complete Python file as a block scalar) and
`requirements` (a list of pip package names, empty list if none needed).

<TOOL_STUBS>

## Original Use Case

For additional context on what these tools should do:

<ORIGINAL_INPUT>
```
