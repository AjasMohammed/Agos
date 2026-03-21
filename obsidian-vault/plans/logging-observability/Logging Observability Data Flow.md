---
title: Logging Observability Data Flow
tags:
  - observability
  - logging
  - flow
date: 2026-03-21
status: planned
---

# Logging Observability Data Flow

> How log data flows from a CLI command through the kernel to the log subscriber and output destinations.

---

## Diagram

```
┌──────────────────────────────────────────────────────────────────┐
│  agentctl task run                                               │
│  (agentos-cli/src/main.rs)                                       │
│                                                                  │
│  init_logging() ─► tracing::subscriber::set_global_default()    │
│       │                                                          │
│       ├─ stderr layer (text format, always on)                   │
│       └─ file layer  (rolling daily, if log_dir set)             │
└─────────────────────────┬────────────────────────────────────────┘
                          │ BusMessage::SubmitTask { task_id, ... }
                          ▼
┌──────────────────────────────────────────────────────────────────┐
│  run_loop.rs                                                     │
│  #[instrument(skip_all, fields(task_id = %task_id))]  ◄─ Phase 1│
│       │                                                          │
│       │  tracing::info!(task_id=%id, "Task submitted")          │
│       │                                                          │
│       └─► task_executor.rs                                       │
│           #[instrument(skip_all,                      ◄─ Phase 1│
│             fields(task_id=%task.id, agent_id=%task.agent_id))]  │
│                  │                                               │
│                  │  tracing::debug!("Starting tool execution")   │
│                  │                                               │
│                  └─► tool_call.rs                                │
│                      #[instrument(skip_all,           ◄─ Phase 1│
│                        fields(tool = %tool_id))]                 │
│                             │                                    │
│                             └─► agentos-tools/*                  │
│                                 tracing::debug!(     ◄─ Phase 2 │
│                                   path=%p, "File read"           │
│                                 )                                │
└──────────────────────────────────────────────────────────────────┘
                          │
                          │ all events bubble up through span context
                          ▼
┌──────────────────────────────────────────────────────────────────┐
│  tracing::Subscriber (global)                                    │
│                                                                  │
│  EnvFilter ─► filters by level and target                        │
│       │                                                          │
│       ├─ text layer ─► stderr                                    │
│       │    "2026-03-21T10:00:00Z  WARN task_executor{            │
│       │     task_id=task-abc agent_id=agent-xyz}: requeue failed"│
│       │                                                          │
│       └─ json layer ─► /tmp/agentos/logs/agentos.log  ◄─ Phase 4│
│            {"timestamp":"...","level":"WARN",                    │
│             "task_id":"task-abc","agent_id":"agent-xyz",         │
│             "message":"requeue failed","error":"channel closed"} │
└──────────────────────────────────────────────────────────────────┘
```

---

## Steps

### 1. Subscriber Initialization (CLI startup)

`init_logging()` in `crates/agentos-cli/src/main.rs:226` runs before the bus connection is established. It:
- Reads `[logging]` section from `config/default.toml`
- Builds an `EnvFilter` (RUST_LOG env var takes priority)
- Creates a stderr layer with target/file/line metadata
- Optionally creates a rolling file layer if `log_dir` is non-empty
- **Phase 4 addition**: checks `log_format` config key; if `"json"`, uses `tracing_subscriber::fmt::format::Json` formatter on the file layer

### 2. Span Creation (kernel hot paths — Phase 1)

When `run_loop.rs` dispatches a task, `#[instrument]` creates a tracing `Span` capturing `task_id`. All child function calls that also carry `#[instrument]` attach their own child spans (`agent_id`, `tool_id`). This creates a hierarchical span tree:

```
run_loop::dispatch_task{task_id}
  └─ task_executor::execute{task_id, agent_id}
       └─ tool_call::invoke{tool_id}
            └─ file_tool::read{path}
```

### 3. Log Events Within Spans

Any `tracing::warn!`, `tracing::error!`, etc. emitted while inside a span automatically inherit the span's fields. This means a single `tracing::warn!("requeue failed")` inside `task_executor::execute` will emit with `task_id` and `agent_id` in the output without the callsite having to repeat those fields.

### 4. Silent Failure Conversion (Phase 3)

Before (invisible):
```rust
kernel.scheduler.requeue(&waiter_id).await.ok();
```

After (visible):
```rust
if let Err(e) = kernel.scheduler.requeue(&waiter_id).await {
    tracing::warn!(error = %e, waiter_id = %waiter_id, "Requeue failed — waiter will timeout");
}
```

### 5. Output Destinations

| Destination | Format | When |
|-------------|--------|------|
| stderr | text (coloured in TTY) | Always |
| `/tmp/agentos/logs/agentos.log` | text | If `log_dir` set, `log_format = "text"` |
| `/tmp/agentos/logs/agentos.log` | JSON lines | If `log_dir` set, `log_format = "json"` |

---

## Related

- [[Logging Observability Plan]] — master plan
- [[01-span-instrumentation]] — Phase 1
- [[03-silent-failure-elimination]] — Phase 3
- [[04-production-structured-logging]] — Phase 4
