---
title: Pipeline Tutorial
tags:
  - guide
  - pipeline
  - tutorial
  - orchestration
date: 2026-03-31
status: complete
---

# Pipeline Tutorial

> A step-by-step guide to building and running YAML pipelines in AgentOS — from a two-step chain to production-ready workflows with retries, failure handling, and cost limits.

**Prerequisites:** Complete [[Getting Started]] first. You need a running kernel and at least one connected agent.

---

## 1. Your First Pipeline

Let's build a two-step pipeline that researches a topic and summarises the findings.

Create a file called `summarise.yaml`:

```yaml
name: summarise
version: "1.0"
description: "Research a topic and produce a concise summary."
output: summary

steps:
  - id: research
    agent: researcher
    task: "Search your knowledge and gather detailed information about: {{input}}"
    output_var: raw_research

  - id: summarise
    agent: analyst
    task: "Read the following research and write a concise 3-paragraph summary:\n{{raw_research}}"
    output_var: summary
    depends_on: [research]
```

Install it:

```bash
agentctl pipeline install summarise.yaml
```

Expected output:

```
Pipeline 'summarise' v1.0 installed (2 steps)
```

Run it:

```bash
agentctl pipeline run summarise --input "the history of the Linux kernel"
```

Expected output:

```
Pipeline 'summarise' run: a3f1c2d4-...
Status: complete
  Step research:   OK  (4.1s)
  Step summarise:  OK  (2.8s)

Output:
Linus Torvalds released the first Linux kernel in 1991...
```

---

## 2. Understanding Variable Interpolation

Every `{{double_brace}}` in a task prompt or tool input is replaced at runtime. Variables come from two sources:

**Built-in variables** — always available:

| Variable | Value |
|----------|-------|
| `{{input}}` | The string passed via `--input` |
| `{{run_id}}` | Unique UUID for this run |
| `{{date}}` | Today's date, `YYYY-MM-DD` |
| `{{timestamp}}` | Unix epoch seconds |

**Step outputs** — set via `output_var`:

```yaml
- id: fetch
  agent: researcher
  task: "Fetch info about {{input}}"
  output_var: raw_data        # produces {{raw_data}}

- id: process
  agent: analyst
  task: "Process this:\n{{raw_data}}"   # consumes {{raw_data}}
  depends_on: [fetch]
```

If you reference a variable that hasn't been set yet, the engine substitutes `{{UNRESOLVED:var_name}}` and logs a warning. Check your `depends_on` declarations if you see this.

---

## 3. Adding a Tool Step

Tool steps invoke a tool directly rather than prompting an agent. Let's extend the pipeline to save the summary to disk.

Update `summarise.yaml`:

```yaml
name: summarise
version: "1.1"
description: "Research a topic, summarise it, and save to disk."
output: summary

steps:
  - id: research
    agent: researcher
    task: "Search your knowledge and gather detailed information about: {{input}}"
    output_var: raw_research

  - id: summarise
    agent: analyst
    task: "Read the following research and write a concise 3-paragraph summary:\n{{raw_research}}"
    output_var: summary
    depends_on: [research]

  - id: save
    tool: file-writer
    input:
      path: "/tmp/reports/{{date}}-summary.md"
      content: "# Summary: {{input}}\n\n{{summary}}"
    depends_on: [summarise]
```

Re-install and run:

```bash
agentctl pipeline install summarise.yaml
agentctl pipeline run summarise --input "the history of the Linux kernel"
```

Expected output:

```
Pipeline 'summarise' run: b7e2a1f9-...
Status: complete
  Step research:   OK  (4.1s)
  Step summarise:  OK  (2.8s)
  Step save:       OK  (0.1s)

Output:
Linus Torvalds released the first Linux kernel in 1991...
```

The report is now saved at `/tmp/reports/2026-03-31-summary.md`.

> **Note:** The `file-writer` tool requires `fs.user_data:w` permission. If the step fails with a permission error, grant it:
> ```bash
> agentctl perm grant <agent-name> "fs.user_data:w"
> ```

---

## 4. Failure Handling

By default, any failed step stops the entire pipeline. You can change this per step with `on_failure`.

### Skip a non-critical step

A sentiment tagging step isn't essential — if it fails, continue without it:

```yaml
- id: sentiment
  agent: analyst
  task: "Classify the sentiment of this text as positive/negative/neutral:\n{{raw_research}}"
  output_var: sentiment
  on_failure: skip

- id: report
  agent: writer
  task: "Write a report on {{input}}. Sentiment: {{sentiment}}. Research:\n{{raw_research}}"
  depends_on: [research, sentiment]
```

If `sentiment` fails, `{{sentiment}}` resolves to `{{UNRESOLVED:sentiment}}` — the report step still runs.

### Use a default value

Better: provide a fallback so the downstream prompt stays clean:

```yaml
- id: sentiment
  agent: analyst
  task: "Classify the sentiment: {{raw_research}}"
  output_var: sentiment
  on_failure: use_default
  default_value: "unknown"

- id: report
  agent: writer
  task: "Write a report. Sentiment: {{sentiment}}. Research:\n{{raw_research}}"
  depends_on: [research, sentiment]
```

Now `{{sentiment}}` resolves to `"unknown"` when the step fails, keeping the prompt coherent.

---

## 5. Retries with Backoff

Transient failures (network timeouts, overloaded models) can be retried automatically.

```yaml
- id: fetch-data
  agent: researcher
  task: "Fetch the latest metrics from the data warehouse for: {{input}}"
  output_var: metrics
  timeout_minutes: 3
  retry_on_failure: 2         # up to 3 total attempts
  retry_backoff_ms: 1000      # wait 1s before retry 1, 2s before retry 2
  retry_max_delay_ms: 10000   # cap at 10s
  on_failure: fail
```

Retry behaviour:
- `retry_on_failure: 2` means the step runs once, then retries up to 2 more times (3 total).
- Backoff doubles each attempt: 1s → 2s → 4s (capped by `retry_max_delay_ms`).
- If all attempts fail and `on_failure: fail`, the pipeline stops and records the last error.

View attempts in the step log:

```bash
agentctl pipeline logs summarise --run-id <id> --step fetch-data
```

```
--- Attempt 1 [failed] ---
Error: connection timeout after 180s
--- Attempt 2 [failed] ---
Error: connection timeout after 180s
--- Attempt 3 [complete] ---
Retrieved 142 rows from metrics table.
```

---

## 6. A Realistic Example — Code Review Pipeline

Here's a complete, production-style pipeline. Save it as `code-review.yaml`:

```yaml
name: code-review
version: "1.0"
description: "Static analysis, logic review, and actionable report for a code snippet."
output: report

steps:
  - id: static-analysis
    agent: analyst
    task: |
      Perform a static analysis of the following code. List all:
      - Syntax errors
      - Type issues
      - Unused variables
      - Potential null dereferences
      Code:
      {{input}}
    output_var: static_issues
    timeout_minutes: 2
    on_failure: use_default
    default_value: "No static issues found."

  - id: logic-review
    agent: analyst
    task: |
      Review this code for logic bugs, edge cases, and performance issues.
      Focus on correctness, not style.
      Code:
      {{input}}
    output_var: logic_issues
    timeout_minutes: 3
    on_failure: use_default
    default_value: "No logic issues found."

  - id: summarise
    agent: writer
    task: |
      You are a senior code reviewer. Produce an actionable code review report.
      Static analysis findings: {{static_issues}}
      Logic review findings: {{logic_issues}}
      Format as: severity rating (High/Medium/Low), issue, recommendation.
    output_var: report
    depends_on: [static-analysis, logic-review]
    timeout_minutes: 2

  - id: save-report
    tool: file-writer
    input:
      path: "/tmp/reviews/review-{{run_id}}.md"
      content: "# Code Review — {{date}}\n\n{{report}}"
    depends_on: [summarise]
```

Install and run:

```bash
agentctl pipeline install code-review.yaml
agentctl pipeline run code-review --input "$(cat my_module.rs)"
```

Note that `static-analysis` and `logic-review` have no `depends_on` — they run after `research` finishes but are independent of each other.

> **Dependency ordering:** The engine topologically sorts all steps. Steps with no dependencies run first. Steps that share the same dependency tier run in definition order (sequential for now). Both `static-analysis` and `logic-review` must complete before `summarise` starts.

---

## 7. Budget and Cost Limits

For long-running or expensive pipelines, set a hard cost cap:

```yaml
name: deep-research
version: "1.0"
max_cost_usd: 0.25
max_wall_time_minutes: 15
output: report

steps:
  - id: research
    agent: researcher
    task: "Conduct a thorough investigation of: {{input}}"
    output_var: findings
    timeout_minutes: 10

  - id: report
    agent: writer
    task: "Write an executive report based on:\n{{findings}}"
    output_var: report
    depends_on: [research]
    timeout_minutes: 5
```

If the pipeline's agent exceeds $0.25 before a step begins, the run stops with:

```
Pipeline 'deep-research' run: 9c1d3e7f-...
Status: failed
  Step research:  OK  (9.8s)
  Step report:    SKIPPED — budget exhausted ($0.27 spent, limit $0.25)
```

Budget is checked **before** each step executes — a step that starts will run to completion.

---

## 8. Inspecting Runs

### List all runs for a pipeline

```bash
agentctl pipeline status code-review
```

```
Pipeline: code-review
  run b7e2a1f9   complete   2026-03-31 14:02   3 steps
  run 4d9c1a3e   failed     2026-03-31 13:45   2/3 steps
```

### Check a specific run

```bash
agentctl pipeline status code-review --run-id b7e2a1f9
```

```
Pipeline: code-review
Run ID: b7e2a1f9-...
Status: COMPLETE
  Step static-analysis:   OK  (1.9s)
  Step logic-review:      OK  (2.3s)
  Step summarise:         OK  (1.7s)
  Step save-report:       OK  (0.1s)
```

### View step output

```bash
agentctl pipeline logs code-review --run-id b7e2a1f9 --step summarise
```

```
--- Attempt 1 [complete] ---
## Code Review — 2026-03-31

**High:** Unchecked array index on line 42 — add bounds check before access.
**Medium:** `process_data()` allocates inside a hot loop — move allocation outside.
**Low:** Unused import `std::collections::BTreeMap` on line 3 — remove.
```

---

## 9. What to Try Next

- [[11-Pipeline and Workflows]] — full YAML field reference, all `on_failure` policies, variable resolution rules
- [[12-Event System]] — subscribe to pipeline start/complete events and trigger follow-up tasks
- [[13-Cost Tracking]] — understand how cost attribution works per agent/pipeline
- [[05-Agent Management]] — connect more agents to power different pipeline roles
- [[07-Tool System]] — explore all built-in tools available for tool steps

---

## Quick Reference

```bash
# Install
agentctl pipeline install my-pipeline.yaml

# Run (blocking)
agentctl pipeline run <name> --input "..."

# Run (background)
agentctl pipeline run <name> --input "..." --detach

# Check status
agentctl pipeline status <name> --run-id <id>

# View step logs
agentctl pipeline logs <name> --run-id <id> --step <step-id>

# List installed pipelines
agentctl pipeline list

# Remove
agentctl pipeline remove <name>
```
