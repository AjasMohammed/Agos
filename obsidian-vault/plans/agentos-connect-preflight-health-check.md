---
title: Connect-Time LLM Pre-flight Health Check
tags:
  - kernel
  - llm
  - cli
  - reliability
  - plan
date: 2026-04-10
status: in-progress
effort: 2h
priority: medium
---

# Connect-Time LLM Pre-flight Health Check

> Ping the LLM backend before registering an agent so unreachable endpoints fail fast instead of producing a half-onboarded agent whose first task crashes.

---

## Problem

`cmd_connect_agent` in `crates/agentos-kernel/src/commands/agent.rs` constructs the provider adapter at line ~226 and then jumps straight to registry insertion, pubkey registration, cost-tracker registration, event emission, and onboarding-task enqueue — without ever calling `LLMCore::health_check()`.

Consequences today:
- An `agent connect` with a wrong `--base-url`, stopped Ollama daemon, or invalid API key still returns `Success`.
- The agent is fully registered (registry row, pubkey, cost budget, event subscriptions) and the caller only discovers the problem when the onboarding task fails inside the run loop.
- Rollback is messy: we'd have to unwind multiple subsystems after the fact. Better to fail before any mutation happens.

Every adapter already implements a real `health_check()` (HTTP probe to `/api/tags` for Ollama, `/v1/models` for OpenAI, etc.) returning a tri-state `HealthStatus` — the plumbing is already there, just unused at connect time.

## Options Considered

1. **Post-connect check, roll back on failure.** Rejected — complicates state management and races with concurrent connects.
2. **Rely on onboarding-task failure to signal unreachability.** Current behavior. Rejected — the operator gets no synchronous feedback, the agent is left in a broken state, and a reconnect without `--test` produces no task at all (silent).
3. **Pre-flight health check before any state mutation.** Chosen. Runs right after adapter construction and before the registry write lock. Zero rollback needed.

## Decision

Add a pre-flight `health_check()` call in `cmd_connect_agent` between adapter construction (line 231) and the registry mutation block (line 233). Behavior:

- `Healthy` → proceed normally.
- `Degraded { reason }` → log a warning and proceed (the backend works, just slower than ideal — e.g. rate-limited Anthropic).
- `Unhealthy { reason }` → write a `LLMConnectionFailed` audit entry and return `KernelResponse::Error` with the reason. No state mutation.

Ergonomics:
- **`--no-health-check` flag** on `agentos agent connect` for operators who know the endpoint will come up momentarily and want to register regardless. Plumbed through `KernelCommand::ConnectAgent { skip_health_check }`.
- **`agentos agent ping --provider X --model Y [--base-url ...]`** — standalone command that builds an adapter and runs a health check without registering anything. Useful for "can I reach this?" probes before committing.

## Consequences

**Enables:**
- Synchronous, clear failure messages when a provider is unreachable.
- A `ping` sub-command that doubles as a config validator (catches typos in `--base-url`, missing API keys, wrong model names).
- A new `LLMConnectionFailed` audit event for forensic traceability.

**Constrains:**
- Adds one HTTP round-trip to every `agent connect`. For Ollama the probe is <50ms; for OpenAI it's a `/v1/models` list (~200-500ms). Acceptable for a human-initiated command.
- `Degraded` is intentionally non-blocking — operators who want strict behavior need to accept that the adapter is already configured to retry.

## Step-by-Step Plan

1. **Add audit event** — new `AuditEventType::LLMConnectionFailed` variant in `crates/agentos-audit/src/log.rs`.
2. **Add `skip_health_check` field** on `KernelCommand::ConnectAgent` in `crates/agentos-bus/src/message.rs` (`#[serde(default)]` so existing clients stay compatible).
3. **Add `PingLLM` kernel command** in `message.rs` — takes provider/model/base_url, returns a new `KernelResponse::LLMHealth { status: String, latency_ms, detail: Option<String> }`.
4. **Extract adapter builder** in `crates/agentos-kernel/src/commands/agent.rs` — factor the `match &provider { … }` block (lines ~52-224) into a private `Kernel::build_llm_adapter(...) -> Result<(Arc<dyn LLMCore>, Option<String>), String>` helper shared by connect and ping.
5. **Wire pre-flight health check** in `cmd_connect_agent`: after the builder, call `health_check().await`, branch on `Healthy` / `Degraded` / `Unhealthy`, audit+error on `Unhealthy` unless `skip_health_check`.
6. **Add `cmd_ping_llm`** in the same file that reuses the builder + health check and returns the new response type.
7. **Dispatch `PingLLM`** in `crates/agentos-kernel/src/run_loop.rs`.
8. **CLI: `--no-health-check` flag** on `AgentCommands::Connect` in `crates/agentos-cli/src/commands/agent.rs`, forwarded as `skip_health_check`.
9. **CLI: `AgentCommands::Ping { provider, model, base_url }`** — issues `KernelCommand::PingLLM`, prints colored Healthy/Degraded/Unhealthy status.
10. **Build + test + clippy + fmt** as per project conventions.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-audit/src/log.rs` | Add `LLMConnectionFailed` variant |
| `crates/agentos-bus/src/message.rs` | Add `skip_health_check` to `ConnectAgent`, new `PingLLM` command + `LLMHealth` response |
| `crates/agentos-kernel/src/commands/agent.rs` | Extract `build_llm_adapter`, add pre-flight health check, add `cmd_ping_llm` |
| `crates/agentos-kernel/src/run_loop.rs` | Dispatch `PingLLM` → `cmd_ping_llm` |
| `crates/agentos-cli/src/commands/agent.rs` | Add `--no-health-check` flag, add `Ping` subcommand |

## Verification

```bash
cargo build --workspace
cargo test -p agentos-kernel -p agentos-bus -p agentos-cli
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check

# Manual smoke tests (with kernel running)
# 1. Healthy case — should succeed as before.
agentos agent connect --provider ollama --model llama3 --name test1

# 2. Unreachable — should fail with actionable message, no agent registered.
agentos agent connect --provider ollama --model llama3 --name test2 \
  --base-url http://127.0.0.1:9 # port with no listener

# 3. Bypass — register even when unreachable.
agentos agent connect --provider ollama --model llama3 --name test3 \
  --base-url http://127.0.0.1:9 --no-health-check

# 4. Standalone ping without registering.
agentos agent ping --provider ollama --model llama3
agentos agent ping --provider openai --model gpt-4o-mini
```

## Related

- [[Agent Connect Flow]] (if added later)
- `crates/agentos-llm/src/traits.rs` — `LLMCore::health_check` contract
- `crates/agentos-llm/src/types.rs` — `HealthStatus` tri-state enum
