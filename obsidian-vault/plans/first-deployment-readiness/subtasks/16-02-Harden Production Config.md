---
title: Harden Production Config
tags:
  - config
  - v3
  - plan
date: 2026-03-12
status: planned
effort: 6h
priority: high
---

# Harden Production Config

> Replace development defaults with a persistent, production-safe configuration baseline and configurable LLM endpoints.

## Why this sub-task

Default configuration currently targets `/tmp`, which is not durable and is unsuitable for production state (vault, audit data, tool registry, bus socket). Additionally, LLM endpoints are hardcoded to `localhost` — `config/default.toml` has `host = "http://localhost:11434"` and `crates/agentos-kernel/src/commands/agent.rs` (~line 83) falls back to `http://localhost:8000/v1`. Both break container and remote deployments.

## Current -> Target State

- **Current:** `config/default.toml` uses temporary paths and hardcoded localhost LLM endpoints; no deployment profile guidance.
- **Target:** documented production profile with persistent paths, configurable LLM endpoints (via config or env vars), permissions model, and migration guidance.

## What to Do

1. Audit current runtime path and endpoint usage:
   - `config/default.toml`
   - `crates/agentos-kernel/src/config.rs`
   - `crates/agentos-kernel/src/commands/agent.rs` (~line 83, hardcoded `http://localhost:8000/v1` fallback)
   - `docs/guide/07-configuration.md`
2. Define production path contract:
   - `/var/lib/agentos/vault/secrets.db`
   - `/var/lib/agentos/data/audit.db`
   - `/var/lib/agentos/tools/core`
   - `/var/lib/agentos/tools/user`
   - `/run/agentos/agentos.sock`
3. **Make LLM endpoints configurable:**
   - Remove hardcoded `http://localhost:8000/v1` fallback in `agent.rs` — require explicit config or env var (`AGENTOS_LLM_URL`)
   - Production config should reference non-localhost endpoints
   - Add comments to `config/default.toml` noting these are dev-only defaults
4. Add a production profile file: `config/production.toml`
5. Document migration checklist from `/tmp` defaults to persistent storage.
6. Add operational validation: warn on startup if paths are under `/tmp`; log configured LLM endpoint.

## Files Changed

| File | Change |
|------|--------|
| `config/production.toml` | New production baseline config |
| `config/default.toml` | Add comments marking dev-only defaults |
| `crates/agentos-kernel/src/commands/agent.rs` | Replace hardcoded localhost fallback |
| `crates/agentos-kernel/src/config.rs` | Validation warnings for unsafe paths; LLM endpoint config |
| `docs/guide/07-configuration.md` | Production profile, LLM config, and migration notes |
| `README.md` | Reference production config usage |

## Expected Inputs and Outputs

- **Input:** Existing development defaults with hardcoded localhost endpoints.
- **Output:** Production config profile, configurable LLM endpoints, documented migration procedure.

## Prerequisites

- [[16-00-Code Safety Hardening]]
- [[16-01-Restore Quality Gates]]

## Verification

```bash
agentctl start --config config/production.toml
agentctl status
# Verify no localhost in production config
grep -c 'localhost' config/production.toml  # should be 0
```

Pass criteria:
- Kernel boots with production profile.
- Data paths are persistent and writable by runtime user.
- LLM endpoint is configurable (not hardcoded localhost).
