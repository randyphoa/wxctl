# Resource Identification Prompt

This prompt identifies which wxctl resource types are needed for a use case.

## Placeholders

| Marker | Replaced at assembly time by |
|--------|------------------------------|
| `<RESOURCE_CATALOG>` | Compiled resource catalog from `wxctl` |
| `<USER_INPUT>` | User's use case description |

## Output

A `resource_list.yaml` file (carrying a `format: compose/v1` header) listing required resource kinds.

---

## Prompt

```
You are identifying which wxctl resource types are needed for a use case.

## Available Resources

<RESOURCE_CATALOG>

## Instructions

Given the use case below, identify which resource types are needed.
Pick ONLY from the available resources list above.
Output a YAML list with kind and a brief reason for each.
Do not generate configuration — only identify resource types.
Do not wrap output in markdown code fences.
If the use case is ambiguous, list the most likely interpretation.

## Guardrails

- Prefer the minimal set. For an assistant/agent use case the core is usually just
  `project`, `agent`, and the `knowledge_base` / `tool` the request actually names.
- A `knowledge_base` ingests its documents directly — it does NOT need a
  `storage_connection`, a bucket, or `cloud_object_storage`. Only include object-storage
  kinds (`storage_connection`, `*_bucket`, `cloud_object_storage`, object/container kinds)
  when the use case explicitly involves object/file storage, buckets, or a data lake.
- Do not add a resource the use case does not imply. Fewer, correct kinds beat a superset —
  an extra kind may require a service the target environment does not have, making the whole
  config undeployable.

## Output Format

format: compose/v1
resources:
  - kind: <resource_type>
    reason: <one-line justification>

## Use Case

<USER_INPUT>
```
