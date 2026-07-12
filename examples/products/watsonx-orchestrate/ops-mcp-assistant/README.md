# ops-mcp-assistant — ops agent on a custom local MCP server

> An ops assistant backed by a **custom local MCP server** that `wxctl` packages
> into a ZIP and uploads to the tools-runtime (`mcp.source: files`). The server
> exposes a `service_status` lookup over bundled mock data; the agent calls it
> to answer service-health questions. This is the "custom local MCP server
> (`source: files`) + toolkit + agent" shape, end to end.

**Tier:** Heavy

## The brief

The plain-English use case this example is built from (kept verbatim in
[`use-case.txt`](use-case.txt)):

> Ops assistant backed by a custom local MCP server that wxctl packages and uploads, exposing a service-status lookup tool over bundled mock data on a custom gateway model

## What it provisions

[`config.yaml`](config.yaml) declares **five resources** plus one test:

- `kind: orchestrate_connection` — `ops-gateway`, credentials for the gateway
  model; its secret is a `${env:GATEWAY_API_KEY}` reference.
- `kind: space` — `ops-mcp-assistant-space`, the deployment space that anchors
  the tools-runtime and scopes gateway inference.
- `kind: model` — the **Ops Assistant Gateway Model**, `gpt-oss-120b` served by
  watsonx.ai through the AI gateway (`custom_host: ${env:WATSONX_URL}`), scoped by
  the space.
- `kind: toolkit` — `ops_toolkit`, a **custom local MCP server**
  ([`resources/server/ops/server.py`](resources/server/ops/server.py)) with
  `mcp.source: files`, `command: python`, and a `server_path` pointing at the
  server dir; `tools: ["*"]` binds every tool it exposes.
- `kind: agent` — the **Ops Assistant**, bound to the toolkit's `service_status`
  tool.
- `kind: test` — a service-status lookup that asserts the MCP tool fires.

## Run it

This example runs live on a **SaaS watsonx Orchestrate profile** (the local-MCP
build runs on the SaaS tools-runtime). The gateway model is `gpt-oss-120b` served
by watsonx.ai, so it needs two `${env:VAR}` references — `GATEWAY_API_KEY` (the
gateway connection credential, a watsonx API key) and `WATSONX_URL` (the
watsonx.ai inference host). Both must be **set to any value** before running any
command — including `plan`, which errors (`WXCTL-V301`) if a referenced env var is
unset. They do **not** need to be real for `plan`.

Set `WXCTL_REQUEST_TIMEOUT=300` for the live `apply`: packaging and uploading the
server dir (the runtime builds a venv) can exceed the 30s default. Configure a
profile in `~/.wxctl/profiles.yaml` (see the [top-level README](../../../../README.md)),
then from this directory:

```bash
export GATEWAY_API_KEY=mock WATSONX_URL=https://us-south.ml.cloud.ibm.com   # placeholders; real values needed only for a live apply/test
wxctl plan -f config.yaml      # preview the DAG; no live credentials needed
WXCTL_REQUEST_TIMEOUT=300 wxctl apply   -f config.yaml   # package + upload the MCP server, create the model + agent
wxctl test    -f config.yaml   # run the service-status check (the MCP tool fires)
wxctl destroy -f config.yaml   # tear it all down
```

### Generating `log.jsonl`

By default wxctl writes no log file. To capture the structured JSON log while
running any command, prefix it with two env vars — `RUST_LOG` turns the
operator-log layer on, `WXCTL_LOG_PATH` sends it to a file:

```bash
RUST_LOG=wxctl=info WXCTL_LOG_PATH=log.jsonl WXCTL_REQUEST_TIMEOUT=300 wxctl apply -f config.yaml
```

`WXCTL_LOG_PATH` truncates the file each run; add `WXCTL_LOG_APPEND=1` to stream
a full `apply → test → destroy` lifecycle into one file.
