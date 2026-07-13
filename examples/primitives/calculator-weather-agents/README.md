# calculator-weather-agents — the starter pair: tools, a knowledge base, delegation

> The smallest config that shows every core watsonx Orchestrate primitive at
> once: two Python **tools**, a **knowledge base**, and two **agents** wired
> together — one grounded in documents, one delegating to the other. Start
> here before the industry examples.

## The brief

The plain-English use case this example is built from (kept verbatim in
[`use-case.txt`](use-case.txt)):

> A calculator agent that does arithmetic with a Python tool and answers IBM history questions from a knowledge base, plus a weather agent that reports city weather and delegates any math to the calculator agent

That sentence is what `wxctl`'s compose tools turn into the [`config.yaml`](config.yaml)
below — the resources, their `${kind.ref}` wiring, and the `kind: test` checks. You can
regenerate it from the sentence, edit it, or write it by hand; the execute step is the
same either way.

## What it provisions

[`config.yaml`](config.yaml) declares five resources plus tests:

- `kind: knowledge_base` — indexes [`resources/knowledge_base/ibm_history.txt`](resources/knowledge_base/ibm_history.txt),
  a short IBM company-history reference.
- `kind: tool` (Python) ×2 — [`calculator`](resources/tool/calculator/calculator.py)
  (add / multiply / divide) and [`weather`](resources/tool/weather/weather.py)
  (bundled mock forecasts for a handful of cities, with a fair-weather fallback).
- `kind: agent` ×2 — the **Calculator Agent** (calculator tool + knowledge base, with
  `chat_with_docs` enabled) and the **Weather Agent** (weather tool, with the Calculator
  Agent as a `collaborators` entry, so math questions get delegated).
- `kind: test` ×4 — a calculation that must call the calculator tool, a history question
  answered from the knowledge base, a Tokyo forecast that must call the weather tool,
  and a math question to the Weather Agent that exercises delegation.

## Run it

No external dependencies: both tools are self-contained Python functions and the
knowledge base document is bundled. Configure a profile in `~/.wxctl/profiles.yaml`
(see the [top-level README](../../../README.md)), then from this directory:

```bash
wxctl plan    -f config.yaml           # preview the DAG; no credentials needed
wxctl apply   -f config.yaml           # create the five resources
wxctl test    -f config.yaml           # run the four kind: test checks
wxctl destroy -f config.yaml           # tear it all down
```

### Generating `log.jsonl`

By default wxctl writes no log file — it just renders to the terminal. To capture the
structured JSON log while running any command, prefix it with two env vars: `RUST_LOG`
turns the operator-log layer on, `WXCTL_LOG_PATH` sends it to a file instead of stderr:

```bash
RUST_LOG=wxctl=info WXCTL_LOG_PATH=log.jsonl wxctl apply -f config.yaml
```

`WXCTL_LOG_PATH` truncates the file on each run; add `WXCTL_LOG_APPEND=1` to capture a
full `plan → apply → test → destroy` lifecycle into one file.

## Expected output

- `apply` creates the knowledge base, the two Python tools, and the two agents
  (5 resources).
- `test` confirms the Calculator Agent calls `calculator_tool` for 42 × 17 (714) and
  answers the founding question (1911, as CTR) from the knowledge base, and the
  Weather Agent calls `weather_tool` for Tokyo and answers 19 + 23 (42) by delegating
  to its collaborator.
