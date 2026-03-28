---
title: OpenTelemetry Export
tags:
  - observability
  - opentelemetry
  - enterprise
  - plan
  - v3
date: 2026-03-25
status: completed
effort: 3d
priority: medium
---

# Phase 6 — OpenTelemetry Export

> Export AgentOS task execution as OpenTelemetry (OTLP) traces so enterprise users can integrate with Grafana, Datadog, Jaeger, LangSmith, or any OTel-compatible observability platform.

---

## Why This Phase

Enterprise teams deploying AI agents have existing observability infrastructure (Datadog, Grafana, Honeycomb, Jaeger). They cannot adopt a system that creates a separate silo of agent metrics. The research confirms:

> "Integration with tools like Logfire or LangSmith is essential for real-time performance monitoring and debugging opaque error messages."
> "OpenTelemetry integration (OTLP) is an enterprise requirement for observability integration."

AgentOS already has rich instrumentation data (audit log, cost tracker, task trace from Phase 1). This phase is purely about **exporting** that data in the standard format — not adding new instrumentation.

---

## Current → Target State

| Area | Current | Target |
|------|---------|--------|
| Task traces | SQLite traces.db (Phase 1) | Exported as OTLP spans to any compatible backend |
| LLM call metrics | audit log entries | OTLP spans with model, token counts, latency attributes |
| Tool call metrics | audit log entries | OTLP child spans per tool call with duration, outcome |
| Cost metrics | CostTracker in kernel | OTLP gauge metrics: cost_usd, input_tokens, output_tokens |
| Error tracking | AgentOSError logs | OTLP span error events with exception info |
| Agent health | HealthMonitor | OTLP health check metrics |

---

## OpenTelemetry Span Model

```
Span: task.run [task_id=abc, agent_id=xyz, model=claude-sonnet-4-6]
  │  attributes: task.status, task.iterations, task.cost_usd, agent.name
  │
  ├── Span: task.iteration [iter=1]
  │     attributes: iter.model, iter.input_tokens, iter.output_tokens, iter.stop_reason
  │
  │     ├── Span: tool.call [tool=file-reader]
  │     │     attributes: tool.name, tool.duration_ms, tool.success, tool.trust_tier
  │     │     events: permission_check{granted=true}, injection_scan{score=0.1}
  │     │
  │     └── Span: tool.call [tool=memory-write]
  │           attributes: tool.name, tool.duration_ms, tool.success
  │
  ├── Span: task.iteration [iter=2]
  │     ...
  │
  └── Span: task.completion
        attributes: final_status, total_cost_usd, total_tokens
```

Metric instruments:
- `agentos.task.duration_ms` (histogram)
- `agentos.task.cost_usd` (counter)
- `agentos.task.tokens.input` (counter)
- `agentos.task.tokens.output` (counter)
- `agentos.tool.call.duration_ms` (histogram, by tool name)
- `agentos.llm.request.duration_ms` (histogram, by model)
- `agentos.agent.active_tasks` (gauge)

---

## Detailed Subtasks

## Implementation Notes

- Added a feature-gated `OtelExporter` in `crates/agentos-kernel/src/otel_exporter.rs` with OTLP trace and metric export support.
- Added `[otel]` config parsing plus `AGENTOS_OTEL_*` and standard `OTEL_*` environment override support.
- Wired task, iteration, tool-call, cost, health, and active-task telemetry into the kernel execution paths, including synchronous and RPC child-task execution.
- Added config and exporter smoke tests, and verified the kernel compiles both with and without `--features otel`.

## Verification

- `cargo test -p agentos-kernel --no-run`
- `cargo test -p agentos-kernel --features otel --no-run`
- `cargo test -p agentos-kernel config::tests::otel_defaults_when_section_omitted -- --nocapture`
- `cargo test -p agentos-kernel --features otel config::tests::otel_rejects_invalid_sample_rate -- --nocapture`
- Reviewer agent pass: clean after follow-up fixes

### Subtask 6.1 — Add opentelemetry dependencies

**File:** `crates/agentos-kernel/Cargo.toml`

```toml
[dependencies]
opentelemetry = { version = "0.24", features = ["trace", "metrics"] }
opentelemetry-otlp = { version = "0.17", features = ["grpc-tonic", "http-proto"] }
opentelemetry_sdk = { version = "0.24", features = ["rt-tokio"] }
opentelemetry-semantic-conventions = { version = "0.16" }
tracing-opentelemetry = { version = "0.25" }
```

Keep this **optional** — gated behind a `otel` Cargo feature flag so users without an OTel backend don't pull in the gRPC stack:

```toml
[features]
otel = ["dep:opentelemetry", "dep:opentelemetry-otlp", ...]
```

---

### Subtask 6.2 — OtelExporter struct

**File:** `crates/agentos-kernel/src/otel_exporter.rs` (new)

```rust
#[cfg(feature = "otel")]
use opentelemetry::{global, trace::Tracer, KeyValue};
#[cfg(feature = "otel")]
use opentelemetry_otlp::WithExportConfig;

pub struct OtelExporter {
    tracer: BoxedTracer,
    meter: Meter,
    enabled: bool,
}

impl OtelExporter {
    pub fn new(config: &OtelConfig) -> Result<Self> {
        // Initialize OTLP exporter (gRPC or HTTP)
        // Target: OTEL_EXPORTER_OTLP_ENDPOINT env var or config value
        let exporter = opentelemetry_otlp::new_exporter()
            .tonic()  // gRPC
            .with_endpoint(&config.endpoint);

        let tracer = opentelemetry_otlp::new_pipeline()
            .tracing()
            .with_exporter(exporter)
            .with_trace_config(
                opentelemetry_sdk::trace::config()
                    .with_resource(Resource::new(vec![
                        KeyValue::new("service.name", "agentos"),
                        KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
                    ]))
            )
            .install_batch(opentelemetry_sdk::runtime::Tokio)?;

        Ok(Self { tracer, enabled: true })
    }

    pub fn disabled() -> Self {
        Self { enabled: false, ... }
    }

    pub fn start_task_span(&self, task_id: &str, agent_id: &str, model: &str) -> BoxedSpan { ... }

    pub fn start_iteration_span(&self, parent: &BoxedSpan, iter: u32, model: &str) -> BoxedSpan { ... }

    pub fn start_tool_span(&self, parent: &BoxedSpan, tool_name: &str) -> BoxedSpan { ... }

    pub fn record_cost(&self, agent_id: &str, model: &str, cost_usd: f64, input_tokens: u64, output_tokens: u64) { ... }

    pub fn record_error(&self, span: &mut BoxedSpan, err: &AgentOSError) {
        span.record_error(err);
        span.set_status(StatusCode::Error, err.to_string());
    }
}
```

---

### Subtask 6.3 — Wire OtelExporter into task execution

**File:** `crates/agentos-kernel/src/context.rs`

Add `otel: Arc<OtelExporter>` to `KernelContext`. Initialize in kernel boot based on config.

**File:** `crates/agentos-kernel/src/task_executor.rs`

```rust
// At task start:
let task_span = ctx.otel.start_task_span(&task_id, &agent_id, &model);

// At each iteration start:
let iter_span = ctx.otel.start_iteration_span(&task_span, iteration, &model);

// After each tool call:
let tool_span = ctx.otel.start_tool_span(&iter_span, &tool_name);
// ... execute tool ...
tool_span.set_attribute(KeyValue::new("tool.success", !result.is_err()));
tool_span.set_attribute(KeyValue::new("tool.duration_ms", duration.as_millis() as i64));
drop(tool_span); // ends span

// At iteration end:
ctx.otel.record_cost(agent_id, model, cost, input_tokens, output_tokens);
drop(iter_span);

// At task end:
task_span.set_attribute(KeyValue::new("task.status", status));
drop(task_span);
```

This is **additive only** — the `task_executor.rs` changes add OTel calls without changing existing logic.

---

### Subtask 6.4 — Configuration

**File:** `config/default.toml`

```toml
[otel]
enabled = false
endpoint = "http://localhost:4317"   # OTLP gRPC
protocol = "grpc"                    # grpc | http
service_name = "agentos"
sample_rate = 1.0                    # 1.0 = 100% sampling
scrub_tool_inputs = true             # never export raw tool inputs (may contain secrets)
scrub_tool_outputs = true            # never export raw tool outputs
```

**Security note:** `scrub_tool_inputs` and `scrub_tool_outputs` default to `true`. We export span attributes about tool name, duration, and success/failure — never the actual input or output values. This prevents secrets leaking into observability backends.

---

### Subtask 6.5 — Kernel config struct

**File:** `crates/agentos-kernel/src/config.rs`

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct OtelConfig {
    pub enabled: bool,
    pub endpoint: String,
    pub protocol: OtelProtocol,
    pub service_name: String,
    pub sample_rate: f64,
    pub scrub_tool_inputs: bool,
    pub scrub_tool_outputs: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub enum OtelProtocol { Grpc, Http }
```

---

### Subtask 6.6 — CI: integration test against Jaeger

Add an optional CI job (`otel-test`) that starts a Jaeger all-in-one Docker container, runs a test task with OTel enabled, and queries the Jaeger API to verify spans were received:

```yaml
# .github/workflows/otel-test.yml (optional, not blocking)
services:
  jaeger:
    image: jaegertracing/all-in-one:latest
    ports:
      - 4317:4317   # OTLP gRPC
      - 16686:16686  # Jaeger UI

steps:
  - run: AGENTOS_OTEL_ENABLED=true cargo test -p agentos-kernel --features otel -- otel
  - run: |
      # Verify spans received
      curl http://localhost:16686/api/traces?service=agentos | jq '.data | length > 0'
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/Cargo.toml` | Modified — add OTel deps behind `otel` feature |
| `crates/agentos-kernel/src/otel_exporter.rs` | New — OtelExporter struct |
| `crates/agentos-kernel/src/task_executor.rs` | Modified — emit spans at task/iteration/tool boundaries |
| `crates/agentos-kernel/src/context.rs` | Modified — add `otel: Arc<OtelExporter>` |
| `crates/agentos-kernel/src/config.rs` | Modified — add OtelConfig struct |
| `config/default.toml` | Modified — add `[otel]` section |

---

## Dependencies

- Phase 1 (Task Trace Debugger) — OTel spans use the same `TaskTrace` / `IterationTrace` data structures

---

## Test Plan

1. **OtelExporter disabled** — kernel boots with `otel.enabled = false`, no OTel dependencies initialized, no performance regression
2. **Span structure** — enable OTel with mock exporter, run a 2-iteration task with 3 tool calls, assert: 1 task span, 2 iteration spans, 3 tool spans, parent-child relationships correct
3. **Cost metrics** — run task, assert `agentos.task.cost_usd` counter incremented by correct amount
4. **Scrub tool I/O** — enable with `scrub_tool_inputs = true`, run a task, inspect exported spans, assert no `tool.input` or `tool.output` attributes present
5. **Error recording** — run a task that fails with permission denied, assert task span has `status=ERROR` and error event

---

## Verification

```bash
# Build with otel feature
cargo build -p agentos-kernel --features otel
cargo test -p agentos-kernel --features otel -- otel

# Manual: export to Jaeger
docker run -p 4317:4317 -p 16686:16686 jaegertracing/all-in-one &
# Set config: otel.enabled = true, otel.endpoint = "http://localhost:4317"
agentctl task run --agent myagent "List files in /tmp"
# Open http://localhost:16686 → search for service "agentos"
```

---

## Related

- [[Real World Adoption Roadmap Plan]]
- [[01-task-trace-debugger]] — OTel spans are built on the same trace data structures
