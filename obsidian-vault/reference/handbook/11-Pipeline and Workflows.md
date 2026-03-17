---
title: Pipeline and Workflows
tags:
  - handbook
  - pipeline
  - orchestration
  - v3
date: 2026-03-17
status: complete
---

# Pipeline and Workflows

> Pipelines are multi-step workflows that chain agent tasks and tool invocations into a single, orchestrated execution — with dependency ordering, variable passing, retries, and budget enforcement built in.

---

## What is a Pipeline

A pipeline is a YAML-defined workflow composed of sequential or parallel steps. Each step is either:

- An **agent step** — sends a prompt to a named agent and waits for its response.
- A **tool step** — directly invokes a tool with a JSON input object.

Steps can declare dependencies on other steps (`depends_on`), pass outputs forward via named variables (`output_var`), and specify what to do if they fail (`on_failure`). The pipeline engine topologically sorts steps to respect dependency order before executing them.

The kernel's `PipelineEngine` (`crates/agentos-pipeline/src/engine.rs`) validates the pipeline, resolves variables via `{{double-brace}}` template syntax, enforces timeouts per step, and retries steps up to `retry_on_failure` times before applying the failure policy.

---

## Pipeline YAML Format

Pipelines are defined in YAML files and installed via `agentctl pipeline install`. Below is a fully annotated example:

```yaml
name: data-analysis
version: "1.0"
description: "Fetch, parse, and summarize a data file"
permissions:
  - "fs.user_data:r"
  - "network.outbound:x"
max_cost_usd: 0.50
max_wall_time_minutes: 10
output: summary

steps:
  - id: fetch
    agent: researcher
    task: "Download the latest data from https://example.com/data.csv"
    output_var: raw_data
    timeout_minutes: 2
    retry_on_failure: 2
    on_failure: fail

  - id: parse
    tool: data-parser
    input: { "data": "{{raw_data}}", "format": "csv" }
    output_var: parsed
    depends_on: [fetch]

  - id: analyze
    agent: analyst
    task: "Analyze this data and produce a summary: {{parsed}}"
    output_var: summary
    depends_on: [parse]
    on_failure: use_default
    default_value: "Analysis could not be completed"
```

### Top-Level Fields

| Field | Type | Description |
|-------|------|-------------|
| `name` | string | Unique pipeline name. Used to reference the pipeline in CLI commands. |
| `version` | string | Version string (e.g. `"1.0"`). Informational; not used for conflict resolution. |
| `description` | string (optional) | Human-readable description shown in `pipeline list`. |
| `permissions` | list (optional) | Permission strings required by the pipeline's steps. Same format as capability tokens. |
| `max_cost_usd` | float (optional) | Hard cost cap in USD for the entire pipeline run. If the budget is exhausted before a step executes, the pipeline fails with a budget error. |
| `max_wall_time_minutes` | integer (optional) | Hard wall-clock timeout for the entire run. Currently used for documentation; per-step `timeout_minutes` is enforced at the step level by `tokio::time::timeout`. |
| `output` | string (optional) | The `output_var` name whose value becomes the final pipeline output. If omitted, the pipeline has no declared output. |

### Per-Step Fields

| Field | Type | Description |
|-------|------|-------------|
| `id` | string | Unique step identifier within the pipeline. Used in `depends_on` and logs. |
| `agent` | string | Agent name to dispatch the task to (mutually exclusive with `tool`). |
| `task` | string | Prompt text sent to the agent. Supports `{{variable}}` interpolation. |
| `tool` | string | Tool name to invoke directly (mutually exclusive with `agent`). |
| `input` | JSON object | Input passed to the tool. Supports `{{variable}}` interpolation in values. |
| `output_var` | string (optional) | Variable name to store this step's output in. Subsequent steps reference it as `{{output_var}}`. |
| `depends_on` | list (optional) | Step IDs this step must wait for. The engine topologically sorts all steps before execution. |
| `timeout_minutes` | integer (optional) | Per-step timeout enforced by `tokio::time::timeout`. The step fails if it exceeds this duration. |
| `retry_on_failure` | integer (optional) | Number of additional attempts after the first failure. `retry_on_failure: 2` means up to 3 total attempts. |
| `on_failure` | enum (optional) | Failure policy after all retries are exhausted. Default: `fail`. |
| `default_value` | string (optional) | Used when `on_failure: use_default`. Inserted as the step's output and execution continues. |

---

## Step Types

### Agent Step

Sends a prompt to a named agent and waits for its string response.

```yaml
- id: research
  agent: researcher
  task: "Research the current state of quantum computing: {{input}}"
  output_var: research_result
```

The agent must be registered and connected to the kernel. The engine calls `PipelineExecutor::run_agent_task(agent_name, rendered_prompt)`.

### Tool Step

Directly invokes a tool with a JSON input object.

```yaml
- id: save
  tool: file-writer
  input:
    path: "/output/report-{{run_id}}.md"
    content: "{{report}}"
  depends_on: [generate]
```

The engine calls `PipelineExecutor::run_tool(tool_name, rendered_input)`. The tool must be installed in the kernel's tool registry.

### Variable Interpolation

The `{{var_name}}` syntax substitutes any variable in the pipeline context. Variables are populated from:

- **`output_var` outputs** — each completed step's output is stored under its declared `output_var` name.
- **Built-in variables** — always available regardless of step outputs:

| Variable | Value |
|----------|-------|
| `{{input}}` | The string passed to `pipeline run --input`. |
| `{{run_id}}` | The unique ID of this pipeline run (UUID). |
| `{{date}}` | Current date in `YYYY-MM-DD` format. |
| `{{timestamp}}` | Current Unix timestamp (seconds). |

If a variable is referenced but not yet populated, the engine substitutes `{{UNRESOLVED:var_name}}` and logs a warning. Single-brace `{var}` syntax is not treated as a variable — only `{{double_brace}}`.

---

## Failure Handling

When a step fails (after all retries), the `on_failure` policy determines what happens next:

| Policy | YAML value | Behavior |
|--------|-----------|----------|
| **Fail** (default) | `fail` | The pipeline immediately stops. The run's status is set to `Failed` and the error is recorded. Subsequent steps are not executed. |
| **Skip** | `skip` | The step is marked as `Skipped`. Its `output_var` is not set (remains unresolved for downstream steps). Execution continues with the next step. |
| **Use Default** | `use_default` | The `default_value` string is inserted as the step's output and stored in `output_var`. Execution continues normally. |

Example — graceful degradation:

```yaml
- id: sentiment
  agent: sentiment-analyzer
  task: "Analyze sentiment: {{text}}"
  output_var: sentiment_label
  on_failure: use_default
  default_value: "unknown"

- id: report
  agent: reporter
  task: "Write a report. Sentiment: {{sentiment_label}}"
  depends_on: [sentiment]
```

If `sentiment` fails, `{{sentiment_label}}` resolves to `"unknown"` and `report` still runs.

---

## CLI Commands

### Install a Pipeline

```bash
agentctl pipeline install <path>
```

Reads the YAML file at `<path>`, sends it to the kernel, and registers the pipeline. Paths containing `..` are rejected to prevent path traversal.

```
Pipeline 'data-analysis' v1.0 installed (3 steps)
```

### List Installed Pipelines

```bash
agentctl pipeline list
```

```
NAME                      VERSION    STEPS    DESCRIPTION
data-analysis             1.0        3        Fetch, parse, and summarize a data file
```

### Run a Pipeline

```bash
agentctl pipeline run <name> --input "<input>" [--agent <agent-name>] [--detach]
```

- `--input` — string input passed as `{{input}}` to all steps.
- `--agent` — agent whose permissions govern pipeline execution (optional).
- `--detach` — run in background and return immediately with the run ID.

Without `--detach`, the command blocks until the pipeline completes and prints per-step status:

```
Pipeline 'data-analysis' run: 3f4a9b2c-...
Status: complete
  Step fetch:   OK  (1.2s)
  Step parse:   OK  (0.4s)
  Step analyze: OK  (3.1s)

Output:
The dataset shows a 15% increase in Q4...
```

### Check Run Status

```bash
agentctl pipeline status <name> --run-id <id>
```

```
Pipeline: data-analysis
Run ID: 3f4a9b2c-...
Status: COMPLETE
  Step fetch:   OK  (1.2s)
  Step parse:   OK  (0.4s)
  Step analyze: OK  (3.1s)
```

### View Step Logs

```bash
agentctl pipeline logs <name> --run-id <id> --step <step-id>
```

Shows per-attempt output and errors for a specific step:

```
--- Attempt 1 [failed] ---
Connection timed out
Error: Step 'fetch' timed out after 2 minutes
--- Attempt 2 [complete] ---
Downloaded 2.4 MB from https://example.com/data.csv
```

### Remove a Pipeline

```bash
agentctl pipeline remove <name>
```

Unregisters the pipeline from the kernel's store. Does not affect runs already in progress or completed.

---

## Related

- [[12-Event System]] — pipelines emit events on start/completion
- [[13-Cost Tracking]] — pipeline runs are subject to per-agent budget limits
- [[02-CLI Reference]] — full CLI command reference
