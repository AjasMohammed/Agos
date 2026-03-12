---
title: Production Config Baseline
tags:
  - config
  - v3
  - plan
date: 2026-03-12
status: complete
effort: 6h
priority: high
---

# Production Config Baseline

> Define persistent runtime paths, configurable LLM endpoints, and operational defaults for first deployment.

## Why this phase

Default `/tmp`-based paths are useful for development but unsafe for deployment. Hardcoded `localhost` LLM endpoints in `config/default.toml` and `crates/agentos-kernel/src/commands/agent.rs` break any containerized or remote deployment. This phase creates a production-safe config contract and migration path.

## Current -> Target state

- **Current:** `config/default.toml` defaults to temporary paths; `ollama.host` hardcoded to `http://localhost:11434`; agent connect fallback hardcoded to `http://localhost:8000/v1`.
- **Target:** production profile with durable paths, environment-variable-driven LLM endpoints, and startup validation.

## Detailed subtasks

1. Review current config usage:
   - `config/default.toml`
   - `crates/agentos-kernel/src/config.rs`
   - `crates/agentos-kernel/src/commands/agent.rs` (line ~83, hardcoded localhost fallback)
2. Define production path layout:
   - vault DB: `/var/lib/agentos/vault/secrets.db`
   - audit DB: `/var/lib/agentos/data/audit.db`
   - tools dirs: `/var/lib/agentos/tools/core`, `/var/lib/agentos/tools/user`
   - bus socket: `/run/agentos/agentos.sock`
3. **Make LLM endpoints configurable:**
   - `config/production.toml` should use environment variable references or explicit non-localhost URLs
   - Remove hardcoded `http://localhost:8000/v1` fallback in `agent.rs` — require explicit config or env var (`AGENTOS_LLM_URL`)
   - Document all LLM provider configuration in the production profile
4. Create `config/production.toml` with explicit deployment values.
5. Update docs:
   - `docs/guide/07-configuration.md`
   - `README.md` quick-start section for production config.
6. Add migration checklist from old paths to new layout.
7. Add startup validation: warn if paths are under `/tmp`, log configured LLM endpoint on boot.

## Files changed

| File | Change |
|------|--------|
| `config/production.toml` | New production profile with persistent paths and LLM config |
| `config/default.toml` | Add comments noting these are dev defaults |
| `crates/agentos-kernel/src/commands/agent.rs` | Replace hardcoded localhost fallback with config/env lookup |
| `crates/agentos-kernel/src/config.rs` | Add validation warnings for unsafe temp paths; add LLM endpoint config fields |
| `docs/guide/07-configuration.md` | Production path, LLM endpoint config, and migration docs |
| `README.md` | Production profile usage notes |

## Dependencies

- **Requires:** [[01-quality-gates-stabilization]].
- **Blocks:** [[03-containerization-and-runtime]], [[05-release-process-and-cutover]].

## Test plan

- Start kernel with production profile.
- Validate file creation in persistent locations.
- Verify runtime user has required access permissions.
- Verify LLM endpoint is read from config (not hardcoded).
- Verify startup warning when paths are under `/tmp`.

## Verification

```bash
agentctl start --config config/production.toml
agentctl status
# Verify no localhost in production config
grep -c 'localhost' config/production.toml  # should be 0
```
