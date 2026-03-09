---
title: Pipeline System
tags: [reference, pipeline]
---

# Pipeline System

Pipelines are multi-step workflows defined in YAML, enabling complex multi-agent operations with dependency management.

**Source:** `crates/agentos-pipeline/src/`

## Pipeline Definition (YAML)

```yaml
name: "analysis-pipeline"
version: "1.0.0"
description: "Multi-agent data analysis workflow"
permissions:
  - "fs.user_data:r"
  - "memory.semantic:rw"

steps:
  - id: "fetch"
    agent: "researcher"
    task: "Fetch latest data from the API"
    output_var: "raw_data"
    timeout_minutes: 5

  - id: "parse"
    tool: "data-parser"
    input:
      data: "{{raw_data}}"
      format: "json"
    output_var: "parsed"
    depends_on: ["fetch"]

  - id: "analyze"
    agent: "analyst"
    task: "Analyze {{parsed}} and generate insights"
    depends_on: ["parse"]
    output_var: "result"
    retry_on_failure: 2

output: "result"
```

## Core Types

### PipelineDefinition
```rust
pub struct PipelineDefinition {
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    pub permissions: Vec<String>,
    pub steps: Vec<PipelineStep>,
    pub output: Option<String>,  // variable name for final result
}
```

### PipelineStep
```rust
pub struct PipelineStep {
    pub id: String,
    pub action: StepAction,
    pub output_var: Option<String>,
    pub depends_on: Vec<String>,
    pub timeout_minutes: Option<u64>,
    pub retry_on_failure: Option<u32>,
}

pub enum StepAction {
    Agent { agent: String, task: String },
    Tool { tool: String, input: Value },
}
```

## Execution Engine

### How It Works

1. **Topological sort** - Steps ordered by dependencies
2. **Template rendering** - `{{variable}}` replaced with outputs from prior steps
3. **Step execution** - Agent tasks or tool calls dispatched to kernel
4. **Result capture** - Step output stored in `output_var`
5. **Retry logic** - Failed steps retried up to `retry_on_failure` times
6. **Final output** - The variable named in `output` returned as pipeline result

### Built-in Variables

| Variable | Value |
|---|---|
| `{{input}}` | Pipeline input string |
| `{{run_id}}` | Unique run identifier |
| `{{date}}` | Current date (YYYY-MM-DD) |
| `{{timestamp}}` | Unix timestamp |

## CLI Usage

```bash
# Install a pipeline
agentctl pipeline install ./my-pipeline.yaml

# Run a pipeline
agentctl pipeline run --name analysis-pipeline --input "Q4 2025 data"

# Run detached (background)
agentctl pipeline run --name analysis-pipeline --input "data" --detach

# Check status
agentctl pipeline status --name analysis-pipeline --run_id <id>

# View step logs
agentctl pipeline logs --name analysis-pipeline --run_id <id> --step parse

# List installed pipelines
agentctl pipeline list

# Remove a pipeline
agentctl pipeline remove analysis-pipeline
```

## Pipeline Storage

- Installed pipelines persisted to disk
- Each run recorded as `PipelineRun` with step-level results
- Status tracking: Pending, Running, Completed, Failed
