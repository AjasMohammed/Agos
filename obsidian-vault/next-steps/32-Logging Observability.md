---
title: Logging & Observability
tags:
  - observability
  - logging
  - debugging
  - production
  - next-steps
date: 2026-03-21
status: planned
effort: 5d
priority: high
---

# Logging & Observability

> Eliminate silent failures and blind spots: add `#[instrument]` spans to kernel hot paths, structured logging to all tools, convert 27+ silent `.ok()` discards to warnings, and enable production JSON output with correlation IDs.

---

## Current State

- Logging framework exists (tracing + rolling files) but coverage is poor
- Zero `#[instrument]` attributes — call chains are untraceable
- Only 6 tracing calls in entire `agentos-tools` crate — tool execution is invisible
- 27+ `.ok()` discards in `task_executor.rs` — requeue failures are completely silent
- No correlation IDs — cannot filter logs by task or request
- JSON output is supported by the library but not wired up

## Goal / Target State

- Every kernel hot path carries a span with `task_id` and `agent_id`
- All tool executions log at `debug` level (entry + result + errors)
- No non-metric `.ok()` discards remain in kernel crate
- Production deployments can set `log_format = "json"` for structured log aggregation
- `agentctl log set-level <level>` changes verbosity without restart

---

## Sub-tasks

| # | Task | Files | Status |
|---|------|-------|--------|
| 01 | [[01-span-instrumentation\|Span instrumentation on kernel hot paths]] | `run_loop.rs`, `task_executor.rs`, `tool_call.rs`, `scheduler.rs` | planned |
| 02 | [[02-tools-logging\|Structured logging for all tools]] | `agentos-tools/src/*.rs` | planned |
| 03 | [[03-silent-failure-elimination\|Eliminate silent .ok() failures]] | `task_executor.rs`, `run_loop.rs`, `scheduler.rs`, `commands/` | planned |
| 04 | [[04-production-structured-logging\|Production JSON output + correlation IDs + CLI log-level command]] | `config.rs`, `main.rs`, `message.rs`, new `commands/log.rs` | planned |

---

## Step-by-Step Plan

### Phase 1 — Span Instrumentation (1d)

1. Add `#[instrument(skip_all, fields(task_id=%, agent_id=%))]` to `execute_task` in `task_executor.rs`
2. Add `#[instrument(skip_all, fields(tool=%))]` to `invoke_tool` in `tool_call.rs`
3. Add `#[instrument(skip_all, fields(command=%))]` to dispatch function in `run_loop.rs`
4. Add restart `warn!` in subsystem restart loop (`run_loop.rs`)
5. Add `#[instrument]` to `schedule`, `requeue`, `complete` in `scheduler.rs`
6. Add `#[instrument]` to `validate` in `intent_validator.rs`
7. Add kernel boot `info!` lines in `kernel.rs`
8. `cargo build -p agentos-kernel && cargo clippy -p agentos-kernel -- -D warnings`

### Phase 2 — Tools Logging (1d)

1. Add entry/exit/error tracing to `file_editor.rs` and `file_reader.rs`
2. Add command/exit-code/stderr tracing to `runner.rs`
3. Add request/response/error tracing to `http_client.rs` and `web_fetch.rs`
4. Add operation tracing to `memory.rs` and `data_parser.rs`
5. Add query tracing to `agent_manual.rs`
6. `cargo build -p agentos-tools && cargo clippy -p agentos-tools -- -D warnings`

### Phase 3 — Silent Failure Elimination (1d)

1. `grep -n '\.ok();' crates/agentos-kernel/src/task_executor.rs` — find all 27 instances
2. Convert each to `if let Err(e) = ... { tracing::warn!(...) }`
3. Apply same treatment to `run_loop.rs` (3 instances) and `scheduler.rs`
4. Triage `commands/` subdirectory for remaining silent discards
5. Check `crates/agentos-bus/src/lib.rs` for send failures
6. `cargo build --workspace && cargo test --workspace`

### Phase 4 — Production Structured Logging (2d)

1. Add `log_format: String` to `LoggingSettings` in `config.rs`; add to `default.toml`
2. Branch `init_logging()` on `log_format`; use `.json()` formatter; wrap filter in `reload::Layer`
3. Add `correlation_id: Option<String>` to `BusMessage`; generate UUID on send
4. Inject `correlation_id` into root span in `run_loop.rs`
5. Add `SetLogLevel` variant to `KernelCommand`
6. Create `crates/agentos-kernel/src/commands/log.rs` with reload handler
7. Create `crates/agentos-cli/src/commands/log.rs` with `log set-level` subcommand
8. Register command in CLI main and kernel dispatch
9. `cargo build --workspace && cargo test --workspace && cargo clippy --workspace -- -D warnings`

---

## Files Changed (All Phases)

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/run_loop.rs` | `#[instrument]`, restart warn, correlation_id span injection, SetLogLevel arm |
| `crates/agentos-kernel/src/task_executor.rs` | `#[instrument]`, convert 27 `.ok()` to warn |
| `crates/agentos-kernel/src/tool_call.rs` | `#[instrument]` |
| `crates/agentos-kernel/src/scheduler.rs` | `#[instrument]`, triage `.ok()` |
| `crates/agentos-kernel/src/intent_validator.rs` | `#[instrument]` |
| `crates/agentos-kernel/src/kernel.rs` | Boot info lines |
| `crates/agentos-kernel/src/config.rs` | Add `log_format` field |
| `crates/agentos-kernel/src/commands/log.rs` | NEW: set_log_level handler |
| `crates/agentos-tools/src/file_editor.rs` | Entry/exit/error tracing |
| `crates/agentos-tools/src/runner.rs` | Command/exit/stderr tracing |
| `crates/agentos-tools/src/http_client.rs` | Request/response/error tracing |
| `crates/agentos-tools/src/web_fetch.rs` | Fetch/error tracing |
| `crates/agentos-tools/src/memory.rs` | Store/retrieve tracing |
| `crates/agentos-tools/src/data_parser.rs` | Parse/error tracing |
| `crates/agentos-tools/src/agent_manual.rs` | Query tracing |
| `crates/agentos-bus/src/message.rs` | Add `correlation_id`, `SetLogLevel` variant |
| `crates/agentos-bus/src/lib.rs` | Generate correlation_id UUID on send |
| `crates/agentos-cli/src/main.rs` | JSON branch in init_logging; reload handle; log command |
| `crates/agentos-cli/src/commands/log.rs` | NEW: log set-level subcommand |
| `config/default.toml` | Add `log_format = "text"` |

---

## Verification

```bash
# After all phases
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check

# Runtime: verify span context
RUST_LOG=debug agentctl task run --agent mock --goal "test" 2>&1 | grep "task_id="

# Runtime: verify JSON output
# In config: log_format = "json"
tail -5 /tmp/agentos/logs/agentos.log | jq '{level, task_id, message}'

# Runtime: verify log level change
agentctl log set-level debug
```

---

## Related

- [[Logging Observability Plan]] — design decisions and phase dependency graph
- [[Logging Observability Data Flow]] — data flow diagram
- [[01-span-instrumentation]] — Phase 1 detail
- [[02-tools-logging]] — Phase 2 detail
- [[03-silent-failure-elimination]] — Phase 3 detail
- [[04-production-structured-logging]] — Phase 4 detail
