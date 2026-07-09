### Example 1: Minimal (agent only)

**Input:**
HR FAQ chatbot that answers employee questions about company policies

**Output:**
```yaml
kind: agent
ref_name: hr_faq_chatbot
name: hr_faq_chatbot
display_name: HR FAQ Chatbot
description: |
  Answers employee questions about company policies, benefits,
  and workplace procedures.
instructions: |
  Be friendly but professional.
  Keep answers concise — 2-3 sentences when possible.
  For questions outside your knowledge, direct the employee to HR.
  Never give legal or medical advice.
llm: groq/openai/gpt-oss-120b
style: default
additional_properties:
  icon: '<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 64 64"><rect fill="#8A3FFC" width="64" height="64" rx="8"/><g transform="translate(32,32)"><circle cx="0" cy="-4" r="8" fill="#fff"/><ellipse cx="0" cy="12" rx="12" ry="8" fill="#fff"/></g></svg>'
  welcome_content:
    welcome_message: Hello, I'm your HR FAQ Chatbot
    description: I answer questions about company policies, benefits, and procedures.
  starter_prompts:
    customize:
      - title: PTO Policy
        subtitle: Benefits question
        prompt: How much PTO do I get per year?
        state: "active"
      - title: Remote Work
        subtitle: Workplace policy
        prompt: What is the policy on remote work?
        state: "active"
```

### Example 2: Simple (agent + knowledge base)

**Input:**
HR policy assistant grounded in employee handbook, benefits guide, and leave policies documents

**Output:**
```yaml
kind: knowledge_base
ref_name: hr_policy_kb
name: hr_policy_kb
display_name: HR Policy Knowledge Base
description: |
  Official company policy documents covering employee handbook,
  benefits, and leave policies.
documents:
  - path: ./resources/knowledge_base/employee_handbook.txt
  - path: ./resources/knowledge_base/benefits_guide.txt
  - path: ./resources/knowledge_base/leave_policies.txt
---
kind: agent
ref_name: hr_policy_assistant
name: hr_policy_assistant
display_name: HR Policy Assistant
description: |
  Answers employee questions about company policies using official
  policy documents as the authoritative source.
instructions: |
  Be friendly but professional.
  Keep answers concise — 2-3 sentences when possible.
  Always cite which policy document the answer comes from.
  For questions outside policy scope, direct the employee to HR.
  Never give legal or medical advice.
llm: groq/openai/gpt-oss-120b
style: default
knowledge_base:
  - ${knowledge_base.hr_policy_kb}
additional_properties:
  icon: '<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 64 64"><rect fill="#009D9A" width="64" height="64" rx="8"/><g transform="translate(32,32)"><rect x="-10" y="-14" width="20" height="28" rx="2" fill="#fff"/><line x1="-6" y1="-8" x2="6" y2="-8" stroke="#009D9A" stroke-width="2"/><line x1="-6" y1="-2" x2="6" y2="-2" stroke="#009D9A" stroke-width="2"/><line x1="-6" y1="4" x2="6" y2="4" stroke="#009D9A" stroke-width="2"/></g></svg>'
  welcome_content:
    welcome_message: Hello, I'm your HR Policy Assistant
    description: I answer questions about company policies using official handbook, benefits, and leave documents.
  starter_prompts:
    customize:
      - title: PTO Policy
        subtitle: Benefits question
        prompt: How much PTO do I get per year?
        state: "active"
      - title: Remote Work
        subtitle: Workplace policy
        prompt: What is the policy on remote work?
        state: "active"
      - title: Parental Leave
        subtitle: Leave process
        prompt: How do I request parental leave?
        state: "active"
```

### Example 3: Standard (agent + knowledge base + tools)

**Input:**
Sales analytics assistant that helps managers analyze pipeline performance and compare deals across reps and regions. Has access to a sales playbook and territory maps. Should present pipeline data in tables and comparisons with percentage changes.

**Output:**
```yaml
kind: knowledge_base
ref_name: sales_analytics_kb
name: sales_analytics_kb
display_name: Sales Analytics Knowledge Base
description: |
  Sales best practices, processes, and territory assignments
  for sales performance analysis.
documents:
  - path: ./resources/knowledge_base/sales_playbook.txt
  - path: ./resources/knowledge_base/territory_maps.txt
---
kind: tool
ref_name: pipeline_summary
name: pipeline_summary
display_name: Pipeline Summary
description: |
  Shows current pipeline value, stage breakdown, and expected close dates.
  Use when user asks about pipeline status, deal counts, or forecasts.
permission: read_only
is_async: false
source_path: ./resources/tool/pipeline_summary
binding:
  python:
    function: pipeline_summary:main
---
kind: tool
ref_name: deal_comparison
name: deal_comparison
display_name: Deal Comparison
description: |
  Compares performance across reps, regions, or time periods.
  Use when user asks to compare metrics, analyze trends, or benchmark performance.
permission: read_only
is_async: false
source_path: ./resources/tool/deal_comparison
binding:
  python:
    function: deal_comparison:main
---
kind: agent
ref_name: sales_analytics_assistant
name: sales_analytics_assistant
display_name: Sales Analytics Assistant
description: |
  Helps sales managers analyze performance, identify trends, and get
  quick answers about their pipeline and team metrics.
instructions: |
  Be data-driven and precise.
  Use tables for comparisons.
  Always show your work and cite data sources.
llm: groq/openai/gpt-oss-120b
style: default
tools:
  - ${tool.pipeline_summary}
  - ${tool.deal_comparison}
knowledge_base:
  - ${knowledge_base.sales_analytics_kb}
guidelines:
  - display_name: Pipeline Presentation
    condition: When presenting pipeline data
    action: |
      Use a table showing deals by stage with values and expected close dates.
  - display_name: Metrics Comparison
    condition: When comparing metrics
    action: |
      Use side-by-side comparison with percentage changes highlighted.
additional_properties:
  icon: '<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 64 64"><rect fill="#0F62FE" width="64" height="64" rx="8"/><g transform="translate(32,32)"><rect x="-12" y="2" width="6" height="12" rx="1" fill="#fff"/><rect x="-3" y="-6" width="6" height="20" rx="1" fill="#fff"/><rect x="6" y="-12" width="6" height="26" rx="1" fill="#fff"/><line x1="-12" y1="14" x2="12" y2="14" stroke="#fff" stroke-width="2"/></g></svg>'
  welcome_content:
    welcome_message: Hello, I'm your Sales Analytics Assistant
    description: I help analyze sales performance, pipeline trends, and team metrics.
  starter_prompts:
    customize:
      - title: Regional Performance
        subtitle: Regional analysis
        prompt: How is the West region performing this quarter?
        state: "active"
      - title: Pipeline Comparison
        subtitle: Period comparison
        prompt: Compare Q2 vs Q3 pipeline
        state: "active"
```
