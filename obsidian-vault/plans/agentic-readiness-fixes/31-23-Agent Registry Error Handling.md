---
title: "Agent Registry JSON Parse Error Handling"
tags:
  - next-steps
  - kernel
  - reliability
  - agentic-readiness
date: 2026-03-19
status: complete
effort: 1h
priority: medium
---

# Agent Registry JSON Parse Error Handling

> Fix silent data loss when agent registry JSON file is corrupted — currently returns an empty registry with no warning.

## What to Do

In `agent_registry.rs`, if the JSON file fails to parse, the code silently returns an empty registry. All registered agents are lost without any indication to the operator.

### Steps

1. **Log a critical warning** on parse failure in `crates/agentos-kernel/src/agent_registry.rs`:
   - When `serde_json::from_str()` fails, log the error at `tracing::error` level
   - Include the file path and error message

2. **Emit a security event:**
   - `SystemEvent::AgentRegistryCorrupted { path, error }`
   - This goes to the audit log for post-incident analysis

3. **Create a backup before overwriting:**
   - Before writing a new registry JSON, copy the existing file to `{path}.bak`
   - On parse failure, check if `.bak` exists and try that
   - If both fail, then proceed with empty registry + critical log

4. **Add a `recover_registry()` method** that an operator can call via CLI to attempt recovery from backup

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/agent_registry.rs` | Add error logging, backup on write, recovery from backup |

## Prerequisites

None.

## Verification

```bash
cargo test -p agentos-kernel
cargo clippy --workspace -- -D warnings
```

Test: corrupt the registry JSON → boot kernel → critical warning logged, backup attempted. Write valid registry → verify `.bak` file exists.
