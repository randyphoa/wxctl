# Generate Data Prompt

Synthesize the data each resource in a validated `config.yaml` needs — as a fixture
file of the declared shape — using a fixed seed and matching the field schema.

## Output

A YAML map keyed by resource `ref_name`; each value has a `content` string. Output only
the YAML — no commentary, no markdown fences. Each data need is tagged `[fixture]` or
`[embedded]`:

- `[fixture]` — `content` is the file bytes of the declared shape.
- `[embedded]` — `content` is source code that synthesizes the data in-code at run time.

---

## Prompt

```
You are synthesizing placeholder data for a set of resources.

Each need under "Data needs" is tagged with a delivery mode: `[fixture]` or `[embedded]`.
Use a fixed random seed so every run is reproducible. Keep entities consistent across
resources (the same population, referenced by every resource that reads it). Do not
invent domain specifics beyond what the use case states; prefer generic, neutral values.

For a `[fixture]` need, generate one file matching its shape. A CSV fixture must have a
header row and at least two data rows. A JSON fixture must be a non-empty array of objects.

For an `[embedded]` need, generate source code (not file bytes) that synthesizes its
dataset in-code when the resource runs. Seed the generator with a fixed constant at the
top of the module so the data is identical on every run. Preserve the resource's existing
entry point and signature, and return records matching the declared shape.

Output a YAML map: each top-level key is a resource `ref_name`, each value is
`{ content: "<file bytes or source code>" }`.

---

## Use Case

<USE_CASE>

---

## Config Context

The full config these resources belong to is below. Treat it as one scenario: keep
every entity consistent across resources — the same seeded population that one resource
produces is the population every other resource reads, references, monitors, or scores.
Cross-reference `ref_name`s to see which resources share data.

<CONFIG_CONTEXT>

---

## Data needs

<DATA_NEEDS>

---

## Resource Schema Reference

<SCHEMA_REFERENCE>

Now output the YAML map of synthesized fixtures.
```
