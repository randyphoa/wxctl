# Using wxctl from an AI agent

wxctl manages IBM product resources declaratively. You write a `config.yaml`; wxctl validates it, resolves every cross-service identifier, plans a dependency-ordered DAG, and executes it against live APIs. It manages product resources (agents, models, deployments, monitors, catalogs, buckets), not VMs or networks. wxctl ships no LLM: you are the intelligence, wxctl is the deterministic engine.

This page is the runbook for driving wxctl from an agent with only the binary and its help output.

## Mental model

- **Declarative.** You describe the desired end state. Re-running `apply` on an unchanged config is a no-op; drift is detected and corrected. There is no state file; wxctl reconciles against live API state.
- **Late-bound references.** You never paste IDs. `${kind.ref_name}` resolves to whatever identifier each API needs (UUID, GUID, href, CRN) at execution time, so one config works across environments.
- **One config, both deployments.** SaaS or Software (CP4D) is a profile setting, not a config rewrite.

## The canonical loop

Run these in order. Each step is safe to repeat.

1. `wxctl resources` lists every kind wxctl supports (no credentials needed).
2. `wxctl explain <kind>` shows a kind's fields, dependencies, endpoints, and authoring rules. `wxctl explain` with no kind prints the config model plus the full kind list.
3. Author `config.yaml` (see Authoring below).
4. `wxctl validate -f config.yaml` runs schema and reference checks, offline.
5. `wxctl plan -f config.yaml` previews the create/update/delete plan.
6. `wxctl apply -f config.yaml` converges to the desired state.
7. `wxctl test -f config.yaml` runs the config's own `kind: test` suite against the live deployment.
8. `wxctl debug` diagnoses the latest failed run.
9. `wxctl destroy -f config.yaml` tears everything down.

## Authoring rules

- **Envelope.** Separate resources with `---`. Each document has a top-level `kind` and `ref_name`.
- **References.** Use `${kind.ref_name}` for a whole resource's identifier, or `${kind.ref_name.field}` for one field of it. Run `wxctl explain <kind>` to see which fields accept references.
- **Tests.** A `kind: test` resource declares turns to run against the deployed system. Every command except `wxctl test` filters these out.
- **Secrets.** Keep credentials out of YAML. Use `${env:VAR}`; a missing or empty variable is caught during validation, before any API call.

```yaml
kind: tool
ref_name: calculator
description: Arithmetic
---
kind: agent
ref_name: assistant
tools:
  - ${tool.calculator}
url: ${env:WATSONX_URL}
```

## Credential-free discovery

`wxctl resources` and `wxctl explain` need no profile and make no network calls. Use them to learn the catalog and the config model before you author anything. `validate` is offline too.

## Machine-readable output

Add `--output json` to emit exactly one JSON document on stdout; logs stay on stderr. Supported on `validate`, `plan`, `apply`, `test`, and `destroy`, plus `-o json` on `resources` and `explain`.

| Command | JSON top-level keys |
|---|---|
| `plan` | `summary` (`create`, `update`, `delete`, `no_change`), `operations[]` |
| `apply`, `destroy` | `run_id`, `summary`, `succeeded[]`, `failed[]`, `skipped[]` |
| `test` | `run_id`, `passed`, `failed`, `tests[]` (each with `turns[]`) |
| `validate` | `valid`, `errors[]`, `fix_prompt` (present only on failure) |

On failure the JSON document is still written to stdout (with `failed[]` or `errors[]` populated) before the process exits nonzero, so you always get structured detail. Run any command with `--output json` to see its exact shape.

## Exit codes

| Code | Meaning |
|---|---|
| `0` | Success. `plan` returns `0` for any valid plan, including one with pending changes. |
| `1` | Error: a validation failure, a failed apply/destroy/test, or a discovery error. |
| `2` | Usage error: unknown flag, missing argument, or invalid `--output` value. |
| `101` | Internal error (panic). Re-run with `--full-trace`. |
| `130` | Interrupted (Ctrl-C). |

## Profiles and credentials

Endpoints and auth live in a profile at `~/.wxctl/profiles.yaml`. Create one with `wxctl init`, which scaffolds a commented file to edit. Select a profile with `-p <name>` or `WXCTL_PROFILE`; point at a different file with `--profile-path`.

## Structured integration

Two ways to drive wxctl programmatically:

- **`--output json`** on any command above, parsed from your own process.
- **`wxctl mcp serve`** starts a local Model Context Protocol server exposing the same operations as tools, with identical JSON shapes. Apply and destroy are gated by `confirm: true`; start with `--read-only` to disable mutation entirely.

## Error recovery

- **Validation failed?** Run `wxctl validate -f config.yaml --fix-prompt` for a ready-to-apply correction prompt, or read `fix_prompt` from the `--output json` document.
- **Unknown kind?** `wxctl explain <bad-kind>` and `wxctl validate` list the valid kinds.
- **A run failed?** `wxctl debug` prints an agent-ready diagnosis of the latest failed run (`wxctl debug <run_id>` for a specific one; `-o json` for a bundle).
- **Hit the same failure twice?** Write a runbook for it. `wxctl debug` matches Markdown files against the failed run's error codes and message keywords, then lists the hits as "Matched troubleshooting docs" in the diagnosis. It reads `docs/troubleshoot/*.md` relative to the working directory, or `WXCTL_TROUBLESHOOT_DIR` when set. A runbook matches on any wxctl error code it mentions verbatim (such as `WXCTL-V005`), so name the codes in the text. The section is skipped when the directory is absent.
