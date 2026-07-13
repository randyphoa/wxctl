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
  <em>wxctl is the engine, not the intelligence. Your AI agent writes the config; wxctl validates, plans, and executes it deterministically.</em>
</p>

Describe a scenario (demo, POC, project, use case) in plain language. Your AI coding agent turns it into a `config.yaml`; wxctl discovers the prerequisites, resolves every cross-service identifier, and executes in dependency order. `plan` previews, `apply` converges, `test` proves, `destroy` tears down. It manages product resources (agents, models, deployments, monitors, catalogs, buckets), not VMs and networks.

Full documentation: **[wxctl.randyphoa.com](https://wxctl.randyphoa.com)**

## Features

- **Reproducible by construction.** Re-applying an unchanged config is a no-op; drift is detected and reported.
- **No trial-and-error IDs.** Every `${kind.ref_name}` reference resolves to the exact identifier each API expects: UUID, GUID, href, or CRN.
- **Cross-product by design.** One config wires an Orchestrate agent to the watsonx.ai deployment OpenScale monitors: [`examples/solutions/credit-risk-governance`](examples/solutions/credit-risk-governance/).
- **Proven live.** A config declares its own `kind: test` suite; `wxctl test` runs it against the deployed system.
- **One config, both deployments.** SaaS or Software (CP4D) is a profile setting, not a config rewrite.
- **Agent-native.** wxctl ships no LLM; any MCP client (Claude Code, Cursor, Claude Desktop) writes the config, and the deterministic engine executes it.

## How it works

Every run compiles the config into a dependency DAG and converges it:

```
config.yaml  ->  Validate  ->  Closure  ->  Reconcile  ->  Plan  ->  Execute (late-bound IDs)
```

Write the `config.yaml` by hand, or generate it from a sentence with the compose tools over wxctl's MCP server; the engine treats both identically. Re-running reconciles against live state and changes only what drifted. Details: [declarative model](https://wxctl.randyphoa.com/concepts/declarative-model) and [pipeline](https://wxctl.randyphoa.com/concepts/pipeline).

## Installation

macOS and Linux:

```bash
curl -fsSL https://raw.githubusercontent.com/randyphoa/wxctl/main/install.sh | sh
```

Windows (PowerShell):

```powershell
irm https://raw.githubusercontent.com/randyphoa/wxctl/main/install.ps1 | iex
```

Or install globally with npm (macOS, Linux, and Windows), which skips the macOS Gatekeeper and Windows SmartScreen prompts:

```bash
npm install -g wxctl
```

Run it once without installing with `npx wxctl --help`.

npm covers glibc Linux, macOS, and Windows. Alpine (musl) is not supported over npm; use the install script or build from source.

<details>
<summary>Or download a binary from the latest GitHub Release.</summary>

Each [release](https://github.com/randyphoa/wxctl/releases) ships a per-platform archive. Pick the one for your machine:

- macOS
  - Apple Silicon (arm64): `wxctl-<version>-aarch64-apple-darwin.tar.gz`
  - Intel (x86_64): `wxctl-<version>-x86_64-apple-darwin.tar.gz`
- Linux
  - x86_64: `wxctl-<version>-x86_64-unknown-linux-gnu.tar.gz`
  - arm64: `wxctl-<version>-aarch64-unknown-linux-gnu.tar.gz`
- Windows
  - x86_64: `wxctl-<version>-x86_64-pc-windows-msvc.zip`
  - arm64: `wxctl-<version>-aarch64-pc-windows-msvc.zip`

Verify the archive against `SHA256SUMS`, extract it, and move the `wxctl` binary onto your `PATH`.

</details>

Build from source, upgrade, and uninstall: [installation guide](https://wxctl.randyphoa.com/installation).

## Quick start

Resources are plain YAML and reference each other with `${kind.ref_name}`; wxctl resolves the order and the identifiers:

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

```bash
wxctl init -f config.yaml     # scaffold a profiles.yaml for the services the config needs
wxctl plan -f config.yaml     # dry run, show what would change
wxctl apply -f config.yaml    # converge to the config
wxctl test -f config.yaml     # prove the deployed system works
```

Full walkthrough: [quickstart](https://wxctl.randyphoa.com/quickstart).

## Supported resources

wxctl manages **100 resource kinds** across **11 IBM products**: watsonx Orchestrate, watsonx.ai, watsonx.data, OpenScale, AI Factsheets, IBM Concert, IBM Concert Workflows, IBM Instana, IBM Planning Analytics, Cloud Object Storage, and Data & AI Common Core.

The full catalog, with per-kind fields, dependencies, and SaaS/Software availability, is generated from the CLI: [resource kinds](https://wxctl.randyphoa.com/reference/resources). Or run `wxctl resources` and `wxctl explain <kind>` locally, no credentials needed.

## Documentation

| Topic | Page |
|---|---|
| Install on macOS, Linux, or Windows | [Installation](https://wxctl.randyphoa.com/installation) |
| First deployment, end to end | [Quickstart](https://wxctl.randyphoa.com/quickstart) |
| Every command and global flag | [Command reference](https://wxctl.randyphoa.com/reference/commands) |
| Every resource kind and its fields | [Resource kinds](https://wxctl.randyphoa.com/reference/resources) |
| Profiles, credentials, and authentication | [Profiles and credentials](https://wxctl.randyphoa.com/concepts/profiles-and-credentials) |
| Generate configs from a sentence | [Compose](https://wxctl.randyphoa.com/guides/compose) |
| Use wxctl from an MCP client | [MCP clients](https://wxctl.randyphoa.com/guides/mcp-clients) |
| Runnable example configs | [Examples](https://wxctl.randyphoa.com/guides/examples) |
| CI/CD and scripting | [Automation](https://wxctl.randyphoa.com/guides/automation) |
| Debug a failed run | [Troubleshooting](https://wxctl.randyphoa.com/guides/troubleshooting) |
| How wxctl relates to Terraform, Pulumi, and Ansible | [How wxctl compares](https://wxctl.randyphoa.com/concepts/how-wxctl-compares) |

The CLI is also self-documenting: `wxctl --help`, `wxctl resources`, `wxctl explain <kind>`, and `wxctl debug` work offline or without credentials.

## License

[Apache License 2.0](https://www.apache.org/licenses/LICENSE-2.0). See [LICENSE](LICENSE) for details.
