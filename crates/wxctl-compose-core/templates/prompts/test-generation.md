# Generate Tests Prompt

Generate `kind: test` resources to append to a validated `config.yaml` for use with `wxctl test`.

## Input

| File | Description |
|------|-------------|
| Use case description | Original business use case with example conversations |
| `config.yaml` | Validated wxctl configuration |

## Output

`kind: test` YAML documents to append to `config.yaml`.

---

## Prompt

```
You are generating wxctl test resources for a deployed agent.

Given a use case description and a validated config.yaml, produce `kind: test`
YAML documents that validate the agent behaves correctly. Output only the YAML
— no commentary, no markdown fences. Do NOT start the output with a leading `---`
separator; begin directly with `kind: test`. Every document must begin with
`kind: test` — never emit a trailing `---`, an empty document, or any document
without a `kind:`. Separate tests with a single `---` between complete documents.

---

## Configuration

<include config.yaml>

---

## Use Case

<include use case description>

---

## Test Resource Format

Each test is a YAML document with:

```yaml
---
kind: test
ref_name: <test_id>
agent: ${agent.<agent_ref_name>}
turns:
  - message: "<user message>"
    expect_tools:
      - <tool_name>
    expect_answer: "<validation criteria>"
```

### Fields

| Field | Required | Description |
|-------|----------|-------------|
| `kind` | Yes | Must be `test` |
| `ref_name` | No | Test identifier (defaults to `"unnamed"`) |
| `agent` | Yes | Agent reference, e.g. `${agent.my_agent}` |
| `turns` | Yes | Conversation turns (at least one) |

### Turn Fields

| Field | Required | Description |
|-------|----------|-------------|
| `message` | Yes | User message sent to the agent |
| `expect_tools` | No | Tool names the agent must call — test fails if any is missing |
| `expect_answer` | No | Expected response — logged for comparison, not validated |

---

## Test Case Generation Rules

### From Example Conversations

Convert each example conversation in the use case to a test case:

| Use Case Section | Test Field |
|------------------|------------|
| User asks | `turns[].message` |
| Assistant uses tools | `expect_tools` (list of tool names) |
| Assistant responds | `expect_answer` (key validation points) |

### From Starter Prompts

Convert each `starter_prompts` entry in the config to a test case. Use the
prompt as the message and infer expected behavior from the agent's description,
instructions, and available tools.

### Multi-Turn Tests

Group related interactions into multi-turn test cases:
- Single question/answer → single turn
- Follow-up questions → multiple turns in the same test case

### Expected Answers

Write `expect_answer` as validation criteria, not exact matches:
- Key facts that should be present
- Tone/format requirements from the agent's instructions
- Mandatory elements (citations, disclaimers, etc.)

### Expected Tools

`expect_tools` is a HARD assertion — the test FAILS if the agent does not call every listed tool.
Only assert a tool call when calling it is unambiguously the correct behavior.

Set `expect_tools` when BOTH hold:
- The use case explicitly mentions tool usage for that question (or the question clearly requires a
  specific tool, e.g. a data lookup), AND
- Calling the tool is the right action — a normal, authorized request the agent should fulfill.

Omit `expect_tools` when:
- The agent can answer from its instructions, knowledge base, or general knowledge alone.
- The correct behavior is to REFUSE, deflect, or apply a guardrail — privacy/authorization limits
  ("look up another employee's private record"), out-of-scope requests ("approve my vacation"), or
  safety/policy declines. In these cases the agent SHOULD NOT call the tool, so asserting a tool call
  contradicts the expected behavior. Validate the refusal through `expect_answer` instead.

Never put a guardrail/refusal scenario and a tool-call assertion in the same turn — they are
mutually exclusive. When unsure whether a tool will fire, omit `expect_tools` and assert the
behavior via `expect_answer`; a not-strictly-required tool call should never fail a test.

---

## Naming Conventions

- `ref_name`: `test_<number>_<short_description>` (e.g., `test_01_pto_policy`)
- One test per distinct scenario; use multi-turn for follow-up conversations

---

## Guardrails

- Only reference agents and tools that exist in the config
- Do not invent tool names — use exact `ref_name` values from tool resources
- Agent references must use `${agent.<ref_name>}` syntax
- Generate at least one test per agent in the config
- Include at least one multi-turn test if the agent has conversational context

---

## Example

### Input Config (excerpt)

```yaml
kind: knowledge_base
ref_name: hr_policy_kb
name: hr_policy_kb
---
kind: agent
ref_name: hr_policy_assistant
name: hr_policy_assistant
tools:
  - ${tool.policy_search}
knowledge_base:
  - ${knowledge_base.hr_policy_kb}
```

### Output

```yaml
kind: test
ref_name: test_01_pto_policy
agent: ${agent.hr_policy_assistant}
turns:
  - message: "How much PTO do I get per year?"
    expect_tools:
      - policy_search
    expect_answer: "Should explain PTO policy, cite Benefits Guide"
---
kind: test
ref_name: test_02_remote_work
agent: ${agent.hr_policy_assistant}
turns:
  - message: "What is the policy on remote work?"
    expect_tools:
      - policy_search
    expect_answer: "Should summarize remote work policy, cite Employee Handbook"
---
kind: test
ref_name: test_03_benefits_followup
agent: ${agent.hr_policy_assistant}
turns:
  - message: "What health insurance options do we have?"
    expect_tools:
      - policy_search
    expect_answer: "Should list available health insurance plans, cite Benefits Guide"
  - message: "Which one covers dental?"
    expect_answer: "Should explain dental coverage for previously mentioned plans"
```

---

## WML Deployment Tests

When the config contains `kind: wml_deployment` (not agents), generate payload-based tests instead of conversation tests.

### WML Test Format

```yaml
kind: test
ref_name: <test_id>
deployment: ${wml_deployment.<deployment_ref_name>}
turns:
  - payload:
      input_data:
        - values:
            - - <input_value_1>
              - <input_value_2>
    expect_response:
      predictions:
        - values:
            - - <expected_output_1>
              - <expected_output_2>
```

### WML Test Fields

| Field | Required | Description |
|-------|----------|-------------|
| `kind` | Yes | Must be `test` |
| `ref_name` | No | Test identifier |
| `deployment` | Yes | Deployment reference, e.g. `${wml_deployment.my_deployment}` |
| `turns` | Yes | Test turns (at least one) |

### WML Turn Fields

| Field | Required | Description |
|-------|----------|-------------|
| `payload` | Yes | Input data sent to the scoring endpoint |
| `expect_response` | No | Expected response structure for validation |

### WML Test Example

```yaml
kind: test
ref_name: test_scoring_basic
deployment: ${wml_deployment.scoring_deployment}
turns:
  - payload:
      input_data:
        - values:
            - - 10
              - 20
              - 30
    expect_response:
      predictions:
        - values:
            - - 10
              - 20
              - 30
---
kind: test
ref_name: test_scoring_single
deployment: ${wml_deployment.scoring_deployment}
turns:
  - payload:
      input_data:
        - values:
            - - 42
    expect_response:
      predictions:
        - values:
            - - 42
```

### WML Test Rules

- Use `deployment:` (not `agent:`) for WML tests
- Use `payload:` and `expect_response:` (not `message:` and `expect_answer:`)
- Generate at least 2 tests: one basic case and one edge case
- Test with realistic input data matching the use case description

---

Now generate the `kind: test` YAML documents for the provided use case and configuration.
```
