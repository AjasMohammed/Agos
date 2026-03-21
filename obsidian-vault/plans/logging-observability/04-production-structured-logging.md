---
title: Phase 4 — Production Structured Logging
tags:
  - observability
  - logging
  - production
  - phase-4
  - next-steps
date: 2026-03-21
status: planned
effort: 2d
priority: medium
---

# Phase 4 — Production Structured Logging

> Enable JSON log output for production deployments, add a `log-level` runtime CLI command for dynamic log level changes, and wire a `correlation_id` field through all kernel spans so logs can be filtered by request.

---

## Why This Phase

Phases 1–3 make the system observable in development. Phase 4 makes it production-ready:
- **JSON output** enables log aggregation tools (Loki, Elasticsearch, Datadog) to parse structured fields
- **`agentctl log-level set debug`** eliminates the need to restart the kernel to change verbosity
- **`correlation_id`** on inbound bus messages lets you `grep 'correlation_id=req-xyz'` and get every log line for a single request, even across async task boundaries

---

## Current → Target State

**Current:** Single log format (text), set at startup, no correlation IDs.

**Target:**
```toml
# config/default.toml
[logging]
log_dir = "/tmp/agentos/logs"
log_level = "info"
log_format = "json"        # NEW: "text" | "json"
```

```json
{"timestamp":"2026-03-21T10:00:00Z","level":"WARN","target":"agentos_kernel::task_executor","task_id":"task-abc","agent_id":"agent-xyz","correlation_id":"req-001","message":"Requeue failed","error":"channel closed"}
```

---

## Detailed Subtasks

### 1. Add `log_format` to `LoggingSettings`

File: `crates/agentos-kernel/src/config.rs`

```rust
#[derive(Debug, Deserialize, Clone)]
pub struct LoggingSettings {
    pub log_dir: String,
    pub log_level: String,
    #[serde(default = "default_log_format")]
    pub log_format: String,  // "text" | "json"
}

fn default_log_format() -> String {
    "text".to_string()
}
```

File: `config/default.toml`
```toml
[logging]
log_dir = "/tmp/agentos/logs"
log_level = "info"
log_format = "text"     # Change to "json" in production deployments
```

---

### 2. Wire JSON formatter in `init_logging()`

File: `crates/agentos-cli/src/main.rs` — `init_logging()` function (line ~226)

The `tracing-subscriber` crate already has the `json` feature enabled (`tracing-subscriber = { features = ["env-filter", "json"] }`). Use it conditionally:

```rust
use tracing_subscriber::fmt::format::FmtSpan;

pub fn init_logging(config: &LoggingSettings) {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&config.log_level));

    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_file(true)
        .with_line_number(true)
        .with_writer(std::io::stderr);

    if config.log_format == "json" {
        let file_layer = build_file_layer_json(&config.log_dir);
        tracing_subscriber::registry()
            .with(env_filter)
            .with(stderr_layer.json())
            .with(file_layer)
            .init();
    } else {
        let file_layer = build_file_layer_text(&config.log_dir);
        tracing_subscriber::registry()
            .with(env_filter)
            .with(stderr_layer)
            .with(file_layer)
            .init();
    }
}
```

The `.json()` method on a layer switches it to JSON output. Existing `with_target`, `with_file`, `with_line_number` options still apply.

---

### 3. Add `correlation_id` to `BusMessage`

File: `crates/agentos-bus/src/message.rs`

Add an optional `correlation_id` field to the bus message wrapper or to `KernelCommand` variants that trigger task execution:

```rust
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct BusMessage {
    pub id: String,                          // message UUID
    pub correlation_id: Option<String>,      // NEW: caller-assigned trace ID
    pub payload: KernelCommand,
}
```

File: `crates/agentos-bus/src/lib.rs`

When sending a `BusMessage` from CLI, generate a UUID for `correlation_id`:
```rust
let msg = BusMessage {
    id: Uuid::new_v4().to_string(),
    correlation_id: Some(Uuid::new_v4().to_string()),
    payload: command,
};
```

---

### 4. Inject `correlation_id` into the root kernel span

File: `crates/agentos-kernel/src/run_loop.rs`

When a `BusMessage` is received, extract `correlation_id` and inject it into the span created by `#[instrument]` (from Phase 1):

```rust
// In the dispatch function
let span = tracing::info_span!(
    "handle_command",
    correlation_id = msg.correlation_id.as_deref().unwrap_or("none"),
    command = %command_name
);
let _enter = span.enter();
// ... dispatch
```

Because all child spans inherit parent span fields, `correlation_id` will propagate to every log line in the call chain — task_executor, tool_call, tools — automatically.

---

### 5. Add `agentctl log set-level <level>` CLI command

This allows changing the kernel's log level at runtime without restart.

File: `crates/agentos-bus/src/message.rs`

Add kernel command variant:
```rust
pub enum KernelCommand {
    // ... existing variants
    SetLogLevel { level: String },
}
```

File: `crates/agentos-kernel/src/commands/` — create `log.rs`:
```rust
pub async fn handle_set_log_level(level: &str) -> Result<CommandResponse, AgentOSError> {
    // tracing-subscriber's EnvFilter supports reload if wrapped in reload::Layer
    // Use the reload handle stored in kernel state
    kernel.log_reload_handle.modify(|filter| {
        *filter = EnvFilter::new(level);
    })?;
    tracing::info!(new_level = %level, "Log level updated");
    Ok(CommandResponse::success("Log level updated"))
}
```

File: `crates/agentos-cli/src/commands/` — create `log.rs`:
```rust
#[derive(Subcommand)]
pub enum LogCommands {
    /// Set the kernel's active log level
    SetLevel {
        /// Log level: error, warn, info, debug, trace
        level: String,
    },
}
```

File: `crates/agentos-cli/src/main.rs` — add to main command dispatch.
File: `crates/agentos-kernel/src/run_loop.rs` — add dispatch arm for `SetLogLevel`.

**Reload handle setup** — in `init_logging()`, wrap the filter in `reload::Layer`:
```rust
let (filter, reload_handle) = reload::Layer::new(env_filter);
// Store reload_handle in KernelState or pass to kernel init
```

---

### 6. Document log configuration in reference

After implementation, update:
- `obsidian-vault/reference/` — add a "Logging System.md" reference doc
- `docs/guide/07-configuration.md` — add `log_format` key documentation

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/config.rs` | Add `log_format` field to `LoggingSettings` |
| `config/default.toml` | Add `log_format = "text"` under `[logging]` |
| `crates/agentos-cli/src/main.rs` | Branch on `log_format`; use `.json()` formatter; setup reload handle |
| `crates/agentos-bus/src/message.rs` | Add `correlation_id` to `BusMessage`; add `SetLogLevel` variant |
| `crates/agentos-bus/src/lib.rs` | Generate `correlation_id` UUID on send |
| `crates/agentos-kernel/src/run_loop.rs` | Inject `correlation_id` into root span; add `SetLogLevel` dispatch arm |
| `crates/agentos-kernel/src/commands/log.rs` | NEW: `handle_set_log_level()` using reload handle |
| `crates/agentos-cli/src/commands/log.rs` | NEW: `log set-level` subcommand |
| `crates/agentos-cli/src/main.rs` | Register `log` command group |

---

## Dependencies

- [[01-span-instrumentation]] — must be complete; `correlation_id` injection only works if spans exist
- [[02-tools-logging]] — should be complete so JSON output captures tool logs
- [[03-silent-failure-elimination]] — should be complete so JSON output captures all warn lines

---

## Test Plan

1. Set `log_format = "json"` in config; run kernel; confirm log file contains valid JSON lines
2. Parse a log line with `jq` — all expected fields (`level`, `task_id`, `agent_id`, `correlation_id`, `message`) must be present
3. Submit two tasks; confirm their log lines have different `task_id` values in JSON
4. Run `agentctl log set-level debug`; confirm debug lines appear without restart
5. `cargo test --workspace` — all pass

---

## Verification

```bash
# Start kernel with JSON logging
LOG_FORMAT=json agentctl kernel start &

# Submit a task
agentctl task run --agent mock --goal "test" 2>&1

# Parse log file for JSON structure
tail -20 /tmp/agentos/logs/agentos.log | jq '{level, task_id, agent_id, message}'
# Expected: valid JSON with all fields

# Change log level at runtime
agentctl log set-level debug
# Expected: "Log level updated" response; debug lines appear in subsequent log output

cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

---

## Related

- [[Logging Observability Plan]] — master plan
- [[Logging Observability Data Flow]] — updated flow showing JSON output path
- [[01-span-instrumentation]] — Phase 1 prerequisite
- [[02-tools-logging]] — Phase 2 prerequisite
- [[03-silent-failure-elimination]] — Phase 3 prerequisite
