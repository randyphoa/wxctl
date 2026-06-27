<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/wxctl-wordmark-dark@3x.png">
    <img alt="wxctl" src="assets/wxctl-wordmark@3x.png" width="320">
  </picture>
</p>

<p align="center">
  <strong>Declarative CLI for managing IBM product resources.</strong>
</p>

<p align="center">
  <a href="LICENSE"><img alt="License: Apache 2.0" src="https://img.shields.io/badge/license-Apache--2.0-blue.svg"></a>
  <img alt="Rust" src="https://img.shields.io/badge/rust-1.88%2B-orange.svg">
  <img alt="Platform" src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey.svg">
  <img alt="Status" src="https://img.shields.io/badge/status-alpha-yellow.svg">
</p>

<p align="center">
  <em>The LLM writes the config. The engine guarantees it: reproducible every run, dependencies resolved up front, not by trial and error.</em>
</p>

Declare the IBM product resources you want. wxctl discovers every prerequisite, resolves every cross-service identifier, and executes in dependency order. Think **Terraform for IBM products** — `plan` previews, `apply` converges, `destroy` tears down, `test` verifies the live system — but pointed at *product resources* (agents, models, governance policies, catalogs, buckets) rather than VMs and networks. Coverage starts with watsonx and is expanding across the IBM portfolio.

## Table of contents

- [Features](#features)
- [How it works](#how-it-works)
- [Installation](#installation)
- [Quick start](#quick-start)
- [Commands](#commands)
- [Supported resources](#supported-resources)
- [Configuration](#configuration)
- [Documentation](#documentation)
- [License](#license)

## Features

- **Reproducible by construction.** One `config.yaml`, the same system every run and every environment; re-applying an unchanged config is a no-op, and drift is detected and reported. No console clicking, no snowflake environments.
- **Dependencies correct on the first pass.** wxctl resolves the full dependency graph and every `${kind.ref_name}` reference up front, in topological order, into the exact ID each API expects, so deployments converge without trial-and-error API calls.
- **One config, many products.** Cross-product wiring is the home turf: an Orchestrate agent backed by a watsonx.ai model, grounded in a watsonx.data lakehouse, governed in watsonx.governance. The flagship [`examples/finance/credit-risk-governance`](examples/finance/credit-risk-governance/) spans **watsonx.ai + watsonx.governance + watsonx Orchestrate** — the agent scores loan applicants by calling the very deployment OpenScale is monitoring.
- **Deployment-agnostic.** SaaS and Software (CP4D) are a runtime axis, not separate configs; the same config runs against either, gated by profile. One config can even span both.
- **Discoverable without credentials.** `wxctl resources` and `wxctl explain <kind>` document every kind, field, dependency, and endpoint with no auth required.
- **Built for automation.** JSON logging, structured events and error codes, redacted run records, an embedded MCP server (`wxctl mcp serve`) so LLM agents can drive it, and a programmatic SDK.

## How it works

Turning intent into a live deployment has a creative half and an exact half. An LLM handles the creative half well — a sentence becomes a first-draft `config.yaml` — and the exact half badly. Point a generic MCP server at the raw product APIs and the agent deploys by trial and error: call an endpoint, hit a missing-prerequisite error, retry, map an ID by hand. It may converge once, but the result is neither repeatable nor guaranteed.

wxctl splits the work at that seam. The model's job ends at the `config.yaml`; a deterministic engine does the rest — buying two guarantees a trial-and-error agent cannot: the same system on every run, and every dependency resolved on the first pass.

Every run compiles the config into a dependency DAG and converges it:

```
config.yaml  ->  Validate  ->  Closure  ->  Reconcile  ->  Plan  ->  Execute (late-bound IDs)
```

1. **Validate** — check the config against the schemas and extract its references.
2. **Closure** — compute every transitive prerequisite from the resources you declared.
3. **Reconcile** — diff that desired state against what already exists remotely.
4. **Plan** — order the create, update, and delete operations topologically.
5. **Execute** — run them concurrently, resolving each `${kind.ref_name}` into the exact ID its API expects, at call time.

Write the `config.yaml` by hand, or generate it from a sentence with the compose tools over wxctl's MCP server — the engine treats both identically.

## Installation

### macOS and Linux

```bash
curl -fsSL https://raw.githubusercontent.com/randyphoa/wxctl/main/install.sh | sh
```

Downloads the latest release binary for your platform (macOS and Linux, x86_64 and arm64), verifies its SHA-256 checksum, and installs to `~/.local/bin`. Override with `WXCTL_VERSION` and `WXCTL_INSTALL_DIR`.

### Windows

Download the latest `x86_64-pc-windows-msvc` (or `aarch64-pc-windows-msvc`) `.zip` from the [Releases page](https://github.com/randyphoa/wxctl/releases), verify it against `SHA256SUMS` (every release is checksummed and provenance-attested), extract `wxctl.exe`, and add it to your `PATH`. The install script above is macOS and Linux only; under WSL, follow those steps.

### Build from source

Requires [Rust](https://rustup.rs) 1.88 or newer.

```bash
git clone https://github.com/randyphoa/wxctl.git
cd wxctl
cargo build --release
cp target/release/wxctl /usr/local/bin/   # or add target/release to PATH
```

## Quick start

### 1. Configure a profile

```bash
wxctl init -f config.yaml             # scan a config to detect the services it needs
wxctl init                            # configure all services interactively
wxctl init -f config.yaml -p staging  # write a named profile
wxctl init -f config.yaml --template  # write a placeholder profile, no prompts
```

`init` prompts for each service's URL, auth type, and credentials, then writes `~/.wxctl/config.json`. `--template` skips the prompts and emits a placeholder profile — handy for piped or LLM-readable output.

### 2. Define resources

```yaml
kind: tool
ref_name: calculator
name: calculator
display_name: Calculator
description: A calculator tool
source_path: ./calculator
binding:
  python:
    function: calculator:main
---
kind: agent
ref_name: math_agent
name: math_agent
display_name: Math Agent
description: An agent that does math
llm: groq/openai/gpt-oss-120b
style: default
tools:
  - ${tool.calculator}
```

Resources reference each other with `${kind.ref_name}`; wxctl builds a DAG from these references and resolves each into whatever format the target API expects.

| Syntax | Resolves to |
|---|---|
| `${kind.ref_name}` | The resource ID after creation |
| `${kind.ref_name.field}` | A specific field from the created resource |

### 3. Run

```bash
wxctl plan -f config.yaml      # dry run, show what would change
wxctl apply -f config.yaml     # converge to the config
```

`-f` accepts files, directories, or `-` for stdin, and is repeatable:

```bash
wxctl apply -f base.yaml -f overrides/            # file plus directory
cat */config.yaml | wxctl apply -f -              # glob via stdin
cat extras.yaml | wxctl apply -f base.yaml -f -   # mixed
```

## Commands

The typical workflow is `init` -> `validate` -> `plan` -> `apply` -> `test` -> `destroy`.

| Command | Description |
|---|---|
| `wxctl init [-f <file>] [--template]` | Configure service URLs, auth, and credentials |
| `wxctl validate -f <file> [--fix-prompt] [--output json]` | Check configs against schemas |
| `wxctl plan -f <file>` | Dry run that shows what would change |
| `wxctl apply -f <file>` | Validate, plan, and execute |
| `wxctl test -f <file>` | Run tests against deployed resources |
| `wxctl destroy -f <file>` | Tear down the resources in a config |

**Discovery and inspection** (no auth required for `resources` and `explain`):

| Command | Description |
|---|---|
| `wxctl resources [--service <s>] [--deployment saas\|software] [-o table\|json\|yaml\|markdown]` | List supported resource kinds |
| `wxctl explain <kind> [-o table\|json\|yaml\|markdown]` | Show a kind's fields, dependencies, and endpoints |
| `wxctl runs list` and `wxctl runs show <run_id> [--full]` | Browse run records |
| `wxctl debug [<run_id>] [-o json]` | Diagnose a failed run: error code, failing exchange, fix, and triage class |

**Profiles and advanced:**

| Command | Description |
|---|---|
| `wxctl profile list` | List all configured profiles |
| `wxctl profile show [name]` | Show service details for a profile |
| `wxctl profile use <name>` | Set the active profile |
| `wxctl profile validate [name] [--no-connect]` | Validate URLs, credentials, and connectivity |
| `wxctl compose ...` | (advanced) LLM-assisted config-authoring pipeline: `identify`, `paths`, `prompt`, `scaffold` |
| `wxctl mcp serve [--read-only]` | (hidden) Run an MCP server exposing wxctl as tools |

**Global flags:** `-p, --profile <name>` selects a profile, `--profile-path <path>` uses a custom config file, and `--full-trace` captures full-fidelity run records. `-f, --filename` is repeatable.

Commands that talk to remote services (`plan`, `apply`, `destroy`, `test`) use the active profile, resolved in order: `-p` flag, then `WXCTL_PROFILE` env var, then `~/.wxctl/active_profile`, then `default`.

## Supported resources

Supported today — coverage is expanding across IBM's catalog:

| Product or service | Resource kinds |
|---|---|
| watsonx Orchestrate | `agent`, `tool`, `toolkit`, `knowledge_base`, `model`, `orchestrate_connection` |
| watsonx.ai | `ai_service`, `autoai_experiment`, `wml_deployment`, `wml_function`, `wml_model`, `wml_script` |
| watsonx.data, engines | `presto_engine`, `prestissimo_engine`, `db2_engine`, `spark_engine`, `milvus_service`, `other_engine` |
| watsonx.data, data | `storage_registration`, `database_registration`, `database_connection`, `schema`, `ingestion_job` |
| watsonx.data, governance and SAL | `category`, `business_term`, `business_terms`, `rule`, `rules`, `integration`, `sal_integration`, `sal_global_settings`, `sal_enrichment_settings`, `sal_enrichment_job`, `sal_glossary` |
| watsonx.governance, OpenScale | `data_mart`, `service_provider`, `subscription`, `data_set`, `monitor_definition`, `monitor_instance`, `integrated_system`, `guardrails_policy` |
| watsonx.governance, factsheets | `model_entry`, `inventory` |
| IBM Cloud Object Storage | `s3_bucket`, `s3_object`, `adls_container`, `gcs_bucket`, `storage_connection` |
| Cloud Pak for Data, Data and AI common core | `space`, `project`, `catalog`, `data_asset`, `common_core_connection`, `software_specification`, `package_extension` |
| Local | `python_script` |

## Configuration

### Profiles

Credentials and endpoints live in `~/.wxctl/config.json`, configured through `wxctl init`. Relative paths in a config resolve against the directory of the config file. The active profile is resolved in order: `-p` flag, then `WXCTL_PROFILE`, then `~/.wxctl/active_profile`, then `default`. Override the file location with `--profile-path`.

### Authentication

| Type | `auth_type` | Config fields |
|---|---|---|
| IBM Cloud API key | `apikey` | `apikey` |
| Basic | `basic` | `username`, `password` |
| Cloud Pak for Data | `cp4d` or `icp4d` | `username`, `password` |
| None (local) | `none` | (none) |

### Color themes

wxctl supports dark, light, and plain color modes, resolved in order:

| Method | Example | Effect |
|---|---|---|
| `NO_COLOR` env var | `NO_COLOR=1 wxctl plan -f c.yaml` | Plain, no ANSI codes ([no-color.org](https://no-color.org)) |
| `WXCTL_COLOR` env var | `WXCTL_COLOR=light wxctl plan -f c.yaml` | `dark`, `light`, `never`, `always`, or `auto` |
| Config preference | `"preferences": { "color_theme": "light" }` in `~/.wxctl/config.json` | Persisted default |
| Auto-detect | (default) | Plain when piped, dark otherwise |

`always` forces color even when piped. `never` is equivalent to `NO_COLOR`.

## Documentation

The CLI is self-documenting and most of it needs no credentials:

```bash
wxctl --help                  # all commands and global flags
wxctl <command> --help        # flags for a single command
wxctl resources               # every supported resource kind
wxctl explain agent           # fields, dependencies, and endpoints for a kind
wxctl runs list               # browse recorded runs
wxctl debug                   # diagnose the most recent failed run
```

## License

[Apache License 2.0](https://www.apache.org/licenses/LICENSE-2.0). See [LICENSE](LICENSE) for details.
