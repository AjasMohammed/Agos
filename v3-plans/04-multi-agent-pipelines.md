# Plan 04 — Multi-Agent Pipelines (`agentos-pipeline`)

## Goal

Enable users to define **reusable multi-agent workflows** in YAML where the output of one agent step automatically becomes the input for the next. Implement the pipeline engine, YAML format, CLI commands, and a `collaborative-task` tool for dynamic in-task spawning.

---

## Why Pipelines

Individual agent tasks are powerful, but many real-world workflows require staged work:

- Researcher gathers raw data → Analyst extracts key findings → Summarizer writes the report
- Log aggregator collects events → Classifier tags by severity → Notifier sends alerts

Pipelines make these workflows **declarative, repeatable, and auditable**. A user defines them once and runs them on demand or on a cron schedule.

---

## Dependencies

```toml
# New workspace dependency
serde_yaml = "0.9"   # YAML pipeline definition parsing
```

---

## New Crate: `agentos-pipeline`

```
crates/agentos-pipeline/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── definition.rs    # PipelineDefinition, PipelineStep — deserialized from YAML
    ├── engine.rs        # PipelineEngine — orchestrates execution
    ├── runner.rs        # StepRunner — executes one step, routes output to next
    ├── store.rs         # Persistent pipeline registry (SQLite)
    └── types.rs         # PipelineRun, PipelineRunStatus, StepResult
```

---

## Pipeline YAML Format

```yaml
# research-and-report.yaml
name: "research-and-report"
version: "1.0.0"
description: "Research a topic, analyse findings, write an executive summary."

# Optional: default permissions granted to every step
permissions:
    - "network.outbound:x"
    - "fs.user_data:rw"
    - "memory.semantic:rw"

steps:
    - id: research
      agent: researcher
      task: "Search the web and gather information about: {input}"
      output_var: raw_research
      timeout_minutes: 10

    - id: analyse
      agent: analyst
      task: "Analyse this research and extract the 5 most important findings:\n{raw_research}"
      output_var: analysis
      depends_on: [research]
      timeout_minutes: 5

    - id: summarise
      agent: summarizer
      task: "Write a concise executive summary (max 500 words) based on:\n{analysis}"
      output_var: final_report
      depends_on: [analyse]
      timeout_minutes: 3

    - id: save
      tool: file-writer
      input:
          path: "/output/reports/report-{run_id}.md"
          content: "{final_report}"
      depends_on: [summarise]

output: final_report
```

### Template Variables

Variables use `{name}` syntax and are resolved by `PipelineEngine::render_template()` before each step executes. Unresolved variables are left as-is (not stripped) so that errors are visible in logs.

| Variable               | Description                                |
| ---------------------- | ------------------------------------------ |
| `{input}`              | The pipeline's initial input string        |
| `{run_id}`             | Unique UUID for this pipeline run          |
| `{output_var_name}`    | Output of a previous step's named `output_var` (e.g. `{raw_research}`) |
| `{date}`               | Current date in `YYYY-MM-DD` format        |
| `{timestamp}`          | Current Unix timestamp                     |

---

## Core Types

```rust
#[derive(Debug, Deserialize, Serialize)]
pub struct PipelineDefinition {
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    pub permissions: Vec<String>,
    pub steps: Vec<PipelineStep>,
    pub output: Option<String>,   // Which output_var is the final result
}

/// A step is either an agent task or a direct tool invocation — never both.
#[derive(Debug, Deserialize, Serialize)]
pub struct PipelineStep {
    pub id: String,
    #[serde(flatten)]
    pub action: StepAction,              // Agent or Tool — exactly one required
    pub output_var: Option<String>,      // Variable name for this step's output
    pub depends_on: Vec<String>,         // Step IDs this step depends on
    pub timeout_minutes: Option<u64>,    // Enforced via tokio::time::timeout; step fails on expiry
    pub retry_on_failure: Option<u32>,   // Max retries — engine re-runs the step up to N times on failure
}

/// Exactly one of agent or tool must be specified per step.
#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum StepAction {
    Agent {
        agent: String,
        task: String,            // Template string with {var} references
    },
    Tool {
        tool: String,
        input: serde_json::Value, // Static JSON input for tool execution
    },
}

#[derive(Debug, Serialize)]
pub struct PipelineRun {
    pub id: RunID,
    pub pipeline_name: String,
    pub input: String,
    pub status: PipelineRunStatus,
    pub step_results: HashMap<String, StepResult>,
    pub output: Option<String>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub enum PipelineRunStatus {
    Running,
    Complete,
    Failed,
    Cancelled,
}
```

---

## Pipeline Engine

```rust
pub struct PipelineEngine {
    kernel: Arc<Kernel>,
    store: Arc<PipelineStore>,
}

impl PipelineEngine {
    pub async fn run(
        &self,
        definition: &PipelineDefinition,
        input: &str,
        run_id: RunID,
    ) -> Result<PipelineRun, AgentOSError>;

    /// Execute a single step — either dispatch to an agent or call a tool.
    async fn execute_step(
        &self,
        step: &PipelineStep,
        context: &HashMap<String, String>, // Variable context (output of previous steps)
        run: &mut PipelineRun,
    ) -> Result<StepResult, AgentOSError>;

    /// Resolve all `{var}` references in a template string.
    fn render_template(template: &str, context: &HashMap<String, String>) -> String;

    /// Topologically sort steps to respect `depends_on` constraints.
    fn topological_sort(steps: &[PipelineStep]) -> Result<Vec<&PipelineStep>, AgentOSError>;
}
```

### Execution Model

1. Parse pipeline YAML → `PipelineDefinition`
2. Validate: all `depends_on` IDs exist, no circular deps, all agents/tools are registered
3. Topological sort of steps → DAG walk
4. For each step in order:
    - Render template with current variable context
    - If `agent` step: dispatch a new kernel task to the specified agent, wait for result
    - If `tool` step: execute tool directly via `ToolRunner` (subject to the pipeline's declared `permissions`; the engine mints a scoped capability token per step)
    - Store output in variable context under `output_var`
5. Return final pipeline run with all step results

---

## CLI Commands

```bash
# Install a pipeline from YAML
agentctl pipeline install /path/to/research-and-report.yaml

# List installed pipelines
agentctl pipeline list
# NAME                    VERSION    STEPS    DESCRIPTION
# research-and-report     1.0.0      4        Research a topic...
# daily-report            2.0.0      3        Compile daily health report

# Run a pipeline
agentctl pipeline run research-and-report \
  --input "latest advances in quantum computing"

# Run in background (detached) — returns immediately with a run_id.
# The pipeline executes in the kernel's background pool.
# Use `pipeline status` to poll for completion.
agentctl pipeline run research-and-report \
  --input "latest advances in quantum computing" \
  --detach

# Get run status
agentctl pipeline status research-and-report --run-id abc123
# STATUS: Complete
# Step research: ✅ (47s)
# Step analyse:  ✅ (12s)
# Step summarise:✅ (8s)
# Step save:     ✅ (0.1s)
# Output: saved to /output/reports/report-abc123.md

# View step-level logs
agentctl pipeline logs research-and-report --run-id abc123 --step research

# Remove a pipeline
agentctl pipeline remove research-and-report
```

---

## PipelineStore (SQLite)

```sql
CREATE TABLE pipelines (
    name        TEXT PRIMARY KEY,
    version     TEXT NOT NULL,
    definition  TEXT NOT NULL,   -- Raw YAML
    installed_at TEXT NOT NULL
);

CREATE TABLE pipeline_runs (
    id              TEXT PRIMARY KEY,
    pipeline_name   TEXT NOT NULL REFERENCES pipelines(name) ON DELETE CASCADE,
    input           TEXT NOT NULL,
    status          TEXT NOT NULL,    -- "running" | "complete" | "failed" | "cancelled"
    step_results    TEXT NOT NULL,    -- JSON
    output          TEXT,
    started_at      TEXT NOT NULL,
    completed_at    TEXT,
    error           TEXT
);

CREATE INDEX idx_runs_pipeline ON pipeline_runs(pipeline_name);
CREATE INDEX idx_runs_status ON pipeline_runs(status);

-- Per-step execution log for detailed status queries
CREATE TABLE step_executions (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id      TEXT NOT NULL REFERENCES pipeline_runs(id) ON DELETE CASCADE,
    step_id     TEXT NOT NULL,
    status      TEXT NOT NULL,       -- "pending" | "running" | "complete" | "failed" | "skipped"
    output      TEXT,
    error       TEXT,
    started_at  TEXT,
    completed_at TEXT,
    attempt     INTEGER NOT NULL DEFAULT 1   -- retry attempt number
);

CREATE INDEX idx_step_run ON step_executions(run_id);
```

---

## Integration Points

### `agentos-kernel/src/kernel.rs`

```rust
// Add to Kernel struct:
pub pipeline_engine: Arc<PipelineEngine>,

// Add command handler:
KernelCommand::InstallPipeline { yaml } => { ... }
KernelCommand::RunPipeline { name, input } => { ... }
KernelCommand::PipelineStatus { name, run_id } => { ... }
KernelCommand::PipelineList => { ... }
KernelCommand::RemovePipeline { name } => { ... }
```

### `agentos-cli/src/commands/pipeline.rs`

New CLI command group handling the above commands.

---

## Tests

```rust
#[test]
fn test_pipeline_yaml_parses_correctly() {
    let yaml = include_str!("../../tests/fixtures/research-and-report.yaml");
    let def: PipelineDefinition = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(def.steps.len(), 4);
    assert_eq!(def.steps[0].id, "research");
}

#[test]
fn test_topological_sort_respects_deps() {
    // Steps: C depends on B, B depends on A
    // Expected order: A → B → C
    let sorted = PipelineEngine::topological_sort(&steps).unwrap();
    assert_eq!(sorted[0].id, "a");
    assert_eq!(sorted[2].id, "c");
}

#[test]
fn test_circular_dependency_rejected() {
    // A → B → A (cycle)
    let result = PipelineEngine::topological_sort(&circular_steps);
    assert!(result.is_err());
}

#[test]
fn test_template_rendering() {
    let ctx = HashMap::from([
        ("input".into(), "quantum computing".into()),
        ("raw_research".into(), "Some research text".into()),
    ]);
    let result = PipelineEngine::render_template("Research about {input}: {raw_research}", &ctx);
    assert_eq!(result, "Research about quantum computing: Some research text");
}
```

---

## Verification

```bash
# Install a test pipeline
agentctl pipeline install v3-plans/fixtures/test-pipeline.yaml

# Run it end-to-end
agentctl pipeline run test-pipeline \
  --input "current state of Rust async runtimes"

# Confirm all steps complete
agentctl pipeline status test-pipeline --run-id <run-id>
```

> [!NOTE]
> Step execution is sequential by default (respecting `depends_on`). Parallel step execution (where two steps have no dependency on each other) is a Phase 4 enhancement — use tokio's `join!` over independent branches of the DAG.
