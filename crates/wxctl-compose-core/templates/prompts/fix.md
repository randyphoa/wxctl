# Fix Prompt Template

This prompt is assembled by `wxctl validate --fix-prompt` when validation fails. It provides the LLM with the original config, specific errors, and relevant schema documentation for targeted correction.

## Placeholders

| Marker | Replaced at assembly time by |
|--------|------------------------------|
| `<CONFIG>` | Full contents of the config.yaml being validated |
| `<ERRORS>` | Numbered list of validation errors |
| `<SCHEMA_REFERENCE>` | Per-resource schema docs for failing resource kinds |

---

## Prompt

```
You are fixing validation errors in a wxctl config.yaml.
Do not change anything that is not related to the errors listed below.
Output only the corrected YAML — no commentary, no markdown fences.

## Config

<CONFIG>

## Validation Errors

<ERRORS>

## Schema Reference

<SCHEMA_REFERENCE>

## Instructions

Fix ONLY the errors listed above. Do not add, remove, or modify any resources or fields
that are not related to the errors. Output the complete corrected config.yaml.

When an error is a missing required field (e.g. `required for variant ...`): set that field
to a concrete value inferred from the config/use case, using `${env:VAR_NAME}` for any secret
(API key, token, password) and never a placeholder literal. If the failing resource is not
actually needed by the use case, you MAY remove that resource entirely instead.
```
