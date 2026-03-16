---
title: First Deployment Runbook
tags:
  - release
  - deployment
  - operations
  - reference
date: 2026-03-16
status: complete
---

# First Deployment Runbook

> Step-by-step procedures for the first production deployment of AgentOS, including preflight checks, first-boot smoke tests, and the sign-off template.

---

## Overview

This runbook covers the `v0.1.0` initial deployment to a production or staging environment. It must be executed in sequence; do not skip sections. All failures are blockers unless explicitly marked optional.

**Deployment target:** Docker (Stage 1 — see `agentic-os-deployment.md`).

---

## Phase 0 — Prerequisites

Before starting, confirm:

- [ ] A tagged release exists: `git tag --list | grep v0.1.0`
- [ ] All CI checks are green on the tagged commit
- [ ] Target host has Docker 24+ and Docker Compose v2+
- [ ] Target host has ≥ 4 GB RAM, ≥ 20 GB disk free
- [ ] An Ollama instance is reachable (or a cloud LLM API key is available)
- [ ] You have `AGENTOS_VAULT_PASSPHRASE` ready (min 16 chars, stored out of band)

---

## Phase 1 — Preflight Checklist

Run these on the **build machine** before shipping artifacts:

### 1.1 Quality Gate

```bash
cargo fmt --all -- --check       # must be clean
cargo clippy --workspace -- -D warnings  # zero warnings
cargo test --workspace           # all tests pass
cargo build --release --workspace  # release binary builds
```

Record results in the sign-off template below.

### 1.2 Security Gate

```bash
cargo test -p agentos-kernel --test security_acceptance_test
```

Expected: `7 passed; 0 failed`.

### 1.3 Container Build

```bash
docker build -t agentos:v0.1.0 .
docker run --rm agentos:v0.1.0 --help
```

Both must succeed without errors.

### 1.4 Config Review

```bash
# Verify production.toml uses persistent paths (no /tmp)
grep -n "tmp" config/production.toml  # should return nothing
grep -n "vault_path\|log_path\|data_dir" config/production.toml  # confirm /var/lib paths
```

---

## Phase 2 — Environment Setup

On the **target host**:

### 2.1 Create Environment File

```bash
cp .env.example .env
# Edit .env and set AGENTOS_VAULT_PASSPHRASE to a strong random value
# Example: AGENTOS_VAULT_PASSPHRASE=$(openssl rand -base64 32)
```

**Never commit `.env` to version control.**

### 2.2 Create Data Directories (if not using named volumes)

```bash
sudo mkdir -p /var/lib/agentos/{vault,data,tools/core,tools/user}
sudo chown -R 1000:1000 /var/lib/agentos  # match container user UID
```

### 2.3 Install Core Tool Manifests

```bash
# Copy core tool manifests into the mapped tools/core directory
cp tools/core/*.toml /path/to/core-tools-dir/
```

---

## Phase 3 — First Boot

### 3.1 Start Services

```bash
docker compose up -d
```

Watch logs for the first 60 seconds:

```bash
docker compose logs -f agentos
```

Expected log sequence (order may vary):
- `Kernel booting...`
- `AuditLog opened`
- `SecretsVault initialized`
- `CapabilityEngine booted`
- `ToolRegistry loaded N tools`
- `Bus listening on ...`
- `Kernel ready`

Any `ERROR` or `PANIC` line is a hard blocker.

### 3.2 Health Check

```bash
curl -sf http://localhost:9091/healthz
# Expected: HTTP 200 with JSON status body
```

If this fails, check:
- `docker compose ps` — is the container running?
- `docker compose logs agentos | tail -50` — any startup errors?

---

## Phase 4 — First-Boot Smoke Checklist

Run these via `agentctl` (with the kernel running):

```bash
# Set the socket path if non-default
export AGENTOS_SOCKET=/path/to/agentos.sock
```

| # | Command | Expected Result |
|---|---------|----------------|
| S01 | `agentctl status` | Shows uptime, 0 agents, 0 tasks |
| S02 | `agentctl tool list` | Shows ≥ 1 core tool |
| S03 | `agentctl audit logs --last 5` | Shows `KernelStarted` event |
| S04 | `agentctl secret set test-key --scope global` (enter value interactively when prompted) | `Secret set successfully` |
| S05 | `agentctl secret list` | Shows `test-key` |
| S06 | `agentctl secret revoke test-key` | `Secret revoked successfully` |
| S07 | `agentctl agent connect --provider ollama --model llama3.2 --name smoke-agent` | Agent ID returned |
| S08 | `agentctl task run --agent smoke-agent "Echo: hello"` | Task completes with output |
| S09 | `agentctl agent list` | Shows `smoke-agent` |
| S10 | `agentctl agent disconnect <agent-id>` | `Agent disconnected` |
| S11 | `agentctl audit logs --last 20` | Shows agent connect/disconnect + task events |

All 11 checks must pass before marking the deployment complete.

---

## Phase 5 — Post-Deployment Verification

### 5.1 Audit Chain Integrity

```bash
agentctl audit verify
# Expected: "Audit chain VALID (N entries verified)"
```

### 5.2 Vault Passphrase Test

Restart the container and verify the vault reopens without error:

```bash
docker compose restart agentos
docker compose logs agentos | grep -E "Vault|vault|ERROR"
# Expected: "SecretsVault initialized" — no errors
```

### 5.3 Cleanup Smoke Secrets

```bash
agentctl secret list  # confirm no lingering test keys
```

---

## Phase 6 — Sign-Off

The deployment is complete when all phases pass. Fill in and record the sign-off:

```markdown
## First Deployment Sign-Off — v0.1.0

**Date:** YYYY-MM-DD
**Operator:** <name or handle>
**Host:** <environment name>
**Commit:** <git sha>
**Tag:** v0.1.0

### Preflight Results

| Gate | Result | Notes |
|---|---|---|
| cargo test | ✅ / ❌ | N tests |
| cargo clippy | ✅ / ❌ | — |
| cargo build --release | ✅ / ❌ | — |
| security_acceptance_test | ✅ 7/7 / ❌ | — |
| Docker build | ✅ / ❌ | — |
| config production.toml review | ✅ / ❌ | no /tmp paths |

### First-Boot Smoke Results

| Check | Result |
|---|---|
| S01 agentctl status | ✅ / ❌ |
| S02 tool list | ✅ / ❌ |
| S03 audit KernelStarted | ✅ / ❌ |
| S04-S06 secret set/list/revoke | ✅ / ❌ |
| S07 agent connect | ✅ / ❌ |
| S08 task run | ✅ / ❌ |
| S09-S10 agent list/disconnect | ✅ / ❌ |
| S11 audit trail | ✅ / ❌ |
| `agentctl audit verify` — "VALID (N entries verified)" | ✅ / ❌ |
| Vault restart test | ✅ / ❌ |

### Approved
- [ ] Operator
- [ ] (Optional) Second reviewer

### Known Issues / Deferred Items
<!-- List anything that was skipped or deferred with tracking reference -->
```

---

## Rollback Procedure

If the deployment fails at any phase:

1. Stop services: `docker compose down`
2. Identify last-known-good tag: `git tag --sort=-version:refname | head -5`
3. Checkout that tag and rebuild: `git checkout v<prev> && docker build -t agentos:v<prev> .`
4. Update `docker-compose.yml` image tag to `agentos:v<prev>`
5. Restart: `docker compose up -d`
6. Re-run Phase 4 smoke checks
7. Document the rollback in the sign-off as "ROLLBACK to v<prev>" with reason

**Vault data compatibility:** If the rollback crosses a database schema migration, the vault and audit databases must be restored from backup before starting the old image. Never attempt to open a newer-schema DB with an older binary.

---

## Monitoring After Launch

Watch these in the first 24 hours:

```bash
# Live kernel logs
docker compose logs -f agentos

# Health check every 30s
watch -n 30 'curl -sf http://localhost:9091/healthz | jq .'

# Audit log growth
watch -n 60 'agentctl audit logs --limit 5'
```

Alert conditions:
- Health check fails for > 2 consecutive checks
- Any `ERROR` or `PANIC` in kernel logs
- Vault access errors (indicate passphrase/key problem)
- Audit log chain verification fails

---

## Related

- [[Release Process]] — Versioning workflow and cut criteria
- `docs/guide/07-configuration.md` — Production configuration reference
- `docs/guide/06-security.md` — Security model and acceptance scenarios
- `agentic-os-deployment.md` — Deployment architecture stages
