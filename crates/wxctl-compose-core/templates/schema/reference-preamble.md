# wxctl Configuration Reference

Compact reference for generating valid wxctl configuration files. For detailed field descriptions, nested structures, and examples, see `resources/<service>/<resource>.md` (e.g., `resources/watsonx_orchestrate/agent.md`).

## Configuration File Format

Multi-document YAML with `---` separators:

```yaml
kind: <resource_type>
ref_name: <unique_local_identifier>
name: <api_resource_name>
# ... fields
---
kind: <another_resource_type>
ref_name: <unique_local_identifier>
name: <api_resource_name>
```

- **ref_name**: Local identifier for `${kind.ref_name}` cross-references
- **name**: Resource name sent to the API
- **References**: `${kind.ref_name}` syntax creates dependency edges; plain strings do not
- **Dependencies**: Automatically ordered by references during execution

---

## Available Resources

| Kind | Service | Description |
|------|---------|-------------|
| `catalog` | common_core | Metadata repository for data lake operations |
| `project` | common_core | Collaborative workspace for data science and AI workflows |
| `space` | common_core | Deployment environment for ML models and AI assets |
| `python_script` | local | Local Python script execution |
| `business_term` | watsonx_data | Glossary entry for data governance |
| `business_terms` | watsonx_data | Bulk creation of business terms |
| `category` | watsonx_data | Hierarchical classification for governance artifacts |
| `common_core_connection` | common_core | Data source connection for databases/storage |
| `rule` | watsonx_data | Automated governance policy enforcement |
| `rules` | watsonx_data | Bulk creation of governance rules |
| `agent` | watsonx_orchestrate | AI assistant with tools, knowledge bases, and LLM |
| `knowledge_base` | watsonx_orchestrate | Document-based RAG context for agents |
| `model` | watsonx_orchestrate | Custom LLM configuration with provider settings |
| `orchestrate_connection` | watsonx_orchestrate | Auth credentials for external services |
| `tool` | watsonx_orchestrate | Callable function/service for agents |

---

## Common Nested Structures

**tool `binding`:**
```yaml
binding:
  python:
    function: module_name:function_name
    requirements: []
    connections: {}
```

**orchestrate_connection `credentials`:**
```yaml
# key_value_creds
credentials:
  api_key: your-api-key
# basic_auth
credentials:
  username: user
  password: pass
```

**model `provider_config`:** (author keys snake_case — the API returns camelCase but wxctl normalizes it; camelCase configs re-plan forever)
```yaml
provider_config:
  provider: watsonx               # watsonx, openai, anthropic, etc.
  custom_host: https://...        # optional non-default endpoint
  watsonx_space_id: ${space.<ref>}  # optional; scopes watsonx.ai inference (literal GUID or ref)
```

**knowledge_base `documents`:**
```yaml
documents:
  - path: ./docs/guide.pdf
  - path: ./docs/runbook.txt
```

**agent `additional_properties`:**
```yaml
additional_properties:
  icon: '<svg ...>...</svg>'
  starter_prompts:
    customize:
      - title: "Title"           # required
        subtitle: "Subtitle"
        prompt: "Prompt text"    # required
        state: "active"          # active or inactive
  welcome_content:
    welcome_message: "Welcome"
    description: "Description"
```

**agent `chat_with_docs`:**
```yaml
chat_with_docs:
  enabled: true
```

**agent `guidelines`:**
```yaml
guidelines:
  - display_name: "Guideline Name"
    condition: "When this condition is met"   # required
    action: "Do this action"                  # required
```

---

## Reference Syntax

| Source Kind | Field | Target Kind | Syntax |
|-------------|-------|-------------|--------|
| `agent` | `llm` | `model` | `${model.<ref>}` (or plain string) |
| `agent` | `tools` | `tool` | `${tool.<ref>}` |
| `agent` | `collaborators` | `agent` | `${agent.<ref>}` |
| `agent` | `knowledge_base` | `knowledge_base` | `${knowledge_base.<ref>}` |
| `model` | `connection_id` | `orchestrate_connection` | `${orchestrate_connection.<ref>}` |
| `common_core_connection` | `catalog_id` | `catalog` | `${catalog.<ref>}` |
| `category` | `parent_category` | `category` | `${category.<ref>}` |

---

## Complete Example

```yaml
kind: orchestrate_connection
ref_name: bob_proxy
app_id: bob-proxy
connection_type: key_value_creds
environment: draft
preference: team
credentials:
  api_key: your-api-key
---
kind: model
ref_name: bob_premium
name: virtual-model/openai/bob-premium
display_name: Bob Premium
model_type: chat
connection_id: ${orchestrate_connection.bob_proxy}
provider_config:
  custom_host: https://bob.example.com
---
kind: knowledge_base
ref_name: ops_kb
name: ops_kb
description: Runbooks and incident history
documents:
  - path: ./knowledge/runbooks.txt
---
kind: tool
ref_name: search_tool
name: search_tool
display_name: Log Search
description: Search logs for events
permission: read_only
source_path: ./tools/search_tool
binding:
  python:
    function: search:main
---
kind: agent
ref_name: ops_agent
name: ops_agent
display_name: Operations Assistant
description: |
  An AI assistant for incident resolution.
llm: virtual-model/openai/bob-premium
style: default
tools:
  - ${tool.search_tool}
knowledge_base:
  - ${knowledge_base.ops_kb}
additional_properties:
  icon: '<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 64 64"><rect fill="#0f62fe" width="64" height="64" rx="8"/></svg>'
  welcome_content:
    welcome_message: "Operations Assistant"
    description: "Diagnose and resolve incidents."
chat_with_docs:
  enabled: true
```
