---
title: Troubleshooting and FAQ
tags:
  - docs
  - handbook
  - troubleshooting
date: 2026-03-17
status: complete
---

# 19 — Troubleshooting and FAQ

> Diagnostic procedures, common error solutions, and platform-specific notes for AgentOS operators.

---

## Common Errors and Solutions

### Startup and Connection

| # | Problem | Likely Cause | Solution |
|---|---------|-------------|----------|
| 1 | `Config file not found: config/default.toml` | Running binary from wrong directory | Run from project root, or pass `--config /path/to/default.toml` |
| 2 | `Connection refused` / `BusError: …` | Kernel not running | Start the kernel first: `agentctl start` in a separate terminal |
| 3 | `Socket path already in use` | Previous kernel instance still alive | Kill the old process (`pkill agentos`) or change `[bus].socket_path` in `config/default.toml` |
| 4 | `KernelShutdown` returned on any command | Kernel is mid-shutdown or crashed | Restart: `agentctl start` |

### Agents

| # | Problem | Likely Cause | Solution |
|---|---------|-------------|----------|
| 5 | `AgentNotFound: <name>` | Agent never registered or was evicted | Verify with `agentctl agent list`; re-register if missing |
| 6 | Agent unresponsive to messages | LLM not connected or overloaded | Check `agentctl status`; verify LLM adapter health with `agentctl llm status` |
| 7 | `NoLLMConnected` | No adapter configured or all health checks failed | Add a provider in `[llm]` section or start Ollama (`ollama serve`) |

### Tasks

| # | Problem | Likely Cause | Solution |
|---|---------|-------------|----------|
| 8 | Task stuck in `Running` state | Pending escalation or LLM hang | Check `agentctl escalation list`; approve, deny, or cancel the task |
| 9 | `TaskTimeout: <id>` | Task exceeded configured timeout | Increase `[kernel].task_timeout_secs` or break work into smaller tasks |
| 10 | `BudgetExceeded` — task stopped | Per-agent cost limit reached | Review with `agentctl cost show`; increase budget or switch to a cheaper model |
| 11 | Task immediately fails with `PermissionDenied` | Agent lacks required capability | Grant the permission: `agentctl perm grant <agent> <resource> <op>` |

### Tools

| # | Problem | Likely Cause | Solution |
|---|---------|-------------|----------|
| 12 | `ToolBlocked: <name>` on install | Manifest sets `trust_tier = "blocked"` | Change the tier in the manifest or use a different tool |
| 13 | `ToolSignatureInvalid: <name>` on install | Signature does not match `author_pubkey` | Re-sign: `agentctl tool sign --key author.key manifest.toml`; verify: `agentctl tool verify manifest.toml` |
| 14 | `ToolNotFound: <name>` | Tool directory not on scan path | Add directory to `[tools].tool_dirs` in config; reload tools |
| 15 | `ToolExecutionFailed` | Runtime error inside the tool | Check tool logs; run with `RUST_LOG=agentos=debug` to see detail |
| 16 | WASM tool timeout (`SandboxTimeout`) | Tool CPU limit exceeded | Increase `max_cpu_ms` in the WASM tool manifest |
| 17 | `SandboxSpawnFailed` on non-Linux | Seccomp is Linux-only | Disable the sandbox in config (`[kernel].enable_sandbox = false`) when running on macOS or Windows |
| 18 | `SandboxFilterError` | Seccomp policy rejected a syscall | Review seccomp allow-list in `agentos-sandbox`; file a bug if a legitimate syscall is blocked |
| 19 | `FileLocked` — file held by another task | Concurrent tasks hold a file lock | Wait for the other task to finish, or cancel it: `agentctl task cancel <id>` |

### Secrets and Vault

| # | Problem | Likely Cause | Solution |
|---|---------|-------------|----------|
| 20 | `SecretNotFound: <key>` | Secret not set for this scope | Add it: `agentctl secret set <key> --scope <scope>` |
| 21 | `VaultError: wrong passphrase` | Wrong master passphrase | No recovery path — if passphrase is lost, delete the vault DB and recreate all secrets |
| 22 | OpenAI / Anthropic / Gemini API errors | Key not in vault or endpoint unreachable | Check `agentctl secret list`; set key if missing; verify network access to the API endpoint |
| 23 | Ollama `LLMError: connection refused` | Ollama server not running | Run `ollama serve` and ensure the model is pulled: `ollama pull <model>` |

### Memory

| # | Problem | Likely Cause | Solution |
|---|---------|-------------|----------|
| 24 | Memory model download is slow | First-run cache miss | Embedding model is cached at `[memory].model_cache_dir`; ensure sufficient disk space (≥ 2 GB) |
| 25 | Semantic search returns irrelevant results | Empty or cold memory store | Let the agent complete a few tasks to populate episodic memory before relying on retrieval |

### Events and Pipelines

| # | Problem | Likely Cause | Solution |
|---|---------|-------------|----------|
| 26 | Event subscription not firing | Subscription disabled or throttled | List subscriptions: `agentctl event subscriptions list`; check `enabled` flag and throttle policy |
| 27 | `EventLoopDetected` error | Triggered task emits the same event that triggered it | Add a depth guard or break the feedback loop in the subscription filter |
| 28 | Pipeline step shows `skipped` status | `depends_on` prerequisite failed | Inspect step results with `agentctl pipeline status <id>`; fix the failing predecessor step |
| 29 | Pipeline hangs at a step | Step agent is awaiting escalation | Check `agentctl escalation list` and respond |

### Resources and Audit

| # | Problem | Likely Cause | Solution |
|---|---------|-------------|----------|
| 30 | Resource deadlock detected | Two tasks holding locks in opposite order | Inspect contention: `agentctl resource contention`; force-release: `agentctl resource release <resource>` |
| 31 | Escalation auto-expired | No response within 5 minutes | Escalations expire after `[kernel].escalation_timeout_secs` (default 300 s); respond faster or increase the timeout |
| 32 | Audit chain verification fails | Possible log corruption or tampering | Export the chain for forensics: `agentctl audit export --output audit.json`; then review the broken link |
| 33 | `SchemaValidation` error on intent | Intent payload does not match declared schema | Validate the JSON against the tool manifest schema before submission |

---

## Debug Logging

### Enable verbose output

```bash
RUST_LOG=agentos=debug cargo run --bin agentos-cli -- start
```

For trace-level output from a single crate:

```bash
RUST_LOG=agentos_kernel=trace,agentos_llm=debug cargo run --bin agentos-cli -- start
```

### Correlate log entries with audit records

Every kernel log line at `debug` level includes a `trace_id`. Use it to find the corresponding audit entry:

```bash
agentctl audit logs --last 100 | grep <trace_id>
```

### Useful investigation commands

```bash
# Recent audit entries
agentctl audit logs --last 50

# Tail live audit log
agentctl audit logs --follow

# Verify audit chain integrity
agentctl audit verify

# Export full chain for offline analysis
agentctl audit export --output audit.json
```

---

## Checking System Health

Run this sequence when something seems wrong:

```bash
# 1. Overall kernel status
agentctl status

# 2. Registered agents
agentctl agent list

# 3. Loaded tools
agentctl tool list

# 4. Active tasks
agentctl task list

# 5. Pending escalations
agentctl escalation list

# 6. Audit chain integrity
agentctl audit verify
```

All commands should return without error. Any `ERROR` or `WARN` lines indicate the component that needs attention.

---

## Resetting AgentOS

### Full reset (loses all state)

```bash
# Stop the kernel
agentctl stop

# Remove runtime state (sockets, task state, agent registrations)
rm -rf /tmp/agentos/

# Remove audit and memory databases (paths from config/default.toml)
rm -f data/audit.db data/memory.db

# Restart
agentctl start
```

### Preserve secrets while resetting other state

The vault database is separate from runtime state. To keep secrets intact while resetting everything else:

```bash
agentctl stop

# Back up vault
cp data/vault.db vault.db.bak

# Remove runtime state and other databases
rm -rf /tmp/agentos/ data/audit.db data/memory.db

# Restart — vault.db is still in place, secrets are preserved
agentctl start
```

---

## Platform Notes

### Linux (fully supported)

- Full feature set including seccomp-BPF sandboxing (`agentos-sandbox`)
- All tool trust tiers enforced
- Primary CI and deployment target

### macOS

- Sandboxing is unavailable — `agentos-sandbox` is gated behind `#[cfg(target_os = "linux")]`
- Set `[kernel].enable_sandbox = false` in config; the kernel will refuse to start otherwise
- All other features (tools, vault, memory, pipelines, events) work normally

### Windows

- Not a supported target for production use
- Sandboxing and seccomp are Linux-only
- Wasmtime WASM execution requires a compatible host binary

### Wasmtime and WASM tools

- WASM tools require a Wasmtime-compatible host (Linux x86_64 recommended)
- If `SandboxSpawnFailed` occurs, ensure Wasmtime runtime libraries are installed
- WASM tool `max_cpu_ms` defaults are conservative; increase in the tool manifest for long-running tools

---

## Error Reference

Key `AgentOSError` variants and what they mean:

| Variant | Meaning |
|---------|---------|
| `AgentNotFound(name)` | Named agent is not registered in the kernel |
| `TaskNotFound(id)` | Task ID does not exist or was already garbage-collected |
| `TaskTimeout(id)` | Task exceeded its configured execution deadline |
| `PermissionDenied { resource, operation }` | Agent's capability token does not cover this resource+operation pair |
| `InvalidToken { reason }` | Capability token signature check failed |
| `TokenExpired` | Capability token's TTL elapsed |
| `ToolBlocked { name }` | Tool manifest declares `trust_tier = "blocked"` |
| `ToolSignatureInvalid { name, reason }` | Ed25519 signature over the manifest does not verify |
| `ToolNotFound(name)` | No tool with that name is registered |
| `ToolExecutionFailed { tool_name, reason }` | The tool ran but returned an error |
| `FileLocked { path, … }` | Another task holds a write lock on the file |
| `LLMError { provider, reason }` | The LLM adapter returned an error (network, quota, etc.) |
| `NoLLMConnected` | No LLM adapter is configured or all are unhealthy |
| `SecretNotFound(key)` | Secret key not present in the vault for this scope |
| `VaultError(msg)` | Vault encryption/decryption or DB error |
| `BusError(msg)` | Unix socket IPC failure between CLI and kernel |
| `SandboxSpawnFailed { reason }` | Could not create the sandboxed subprocess |
| `SandboxTimeout { tool_name, timeout_ms }` | Tool exceeded its CPU time budget |
| `SandboxFilterError { reason }` | Seccomp policy violation |
| `EventSubscriptionNotFound(id)` | Subscription ID does not exist |
| `EventLoopDetected { event_type, depth }` | Event triggered itself recursively |
| `SchemaValidation(msg)` | Intent JSON failed schema validation |

---

## Getting Help

### Filing issues

Report bugs and feature requests at the project issue tracker. Include:
1. AgentOS version (`agentctl --version`)
2. Operating system and kernel version
3. Relevant log output (`RUST_LOG=agentos=debug`)
4. Audit export if the issue involves task execution (`agentctl audit export`)

### Reading audit logs for diagnostics

The audit log is the ground truth for what AgentOS did and why. For any unexpected behaviour:

```bash
# Last 100 events with full JSON
agentctl audit logs --last 100 --format json

# Filter to a specific agent
agentctl audit logs --agent <agent-id> --last 50

# Filter to a specific task
agentctl audit logs --task <task-id>
```

Look for `SecurityViolation`, `EscalationRequired`, `BudgetExceeded`, and `ToolExecutionFailed` event types — these are the most common root causes of unexpected stops.
