---
title: Containerization and Runtime
tags:
  - deployment
  - v3
  - plan
date: 2026-03-12
status: planned
effort: 1.5d
priority: critical
---

# Containerization and Runtime

> Add canonical repository deployment artifacts for Stage 1 Docker release.

## Why this phase

A deployment strategy is not actionable until build and orchestration artifacts are committed, versioned, and testable. Multi-stage Rust Docker builds require careful handling of static linking, seccomp dependencies, and build caching.

## Current -> Target state

- **Current:** deployment docs describe Docker strategy, repository lacks committed artifacts.
- **Target:** reproducible Docker build and compose deployment with persistence, health checks, and configurable LLM endpoints.

## Detailed subtasks

1. Create `Dockerfile` multi-stage build:
   - builder stage compiles release binaries (consider `cargo-chef` for layer caching).
   - runtime stage runs as non-root and includes required runtime dependencies.
   - Handle seccomp-bpf dependency (Linux-only, needs `libseccomp-dev` in builder).
   - Ensure static or dynamically-linked binary works in minimal runtime image.
2. Create `docker-compose.yml`:
   - persistent volume mounts for vault/data/logs/tools.
   - `healthcheck` command for container health (use `/healthz` endpoint on port 9091).
   - readonly root filesystem where possible.
   - Environment variable pass-through for LLM endpoints (`AGENTOS_LLM_URL`, `OLLAMA_HOST`).
3. Add `.env.example` with non-secret deployment values.
4. Add `.dockerignore` to exclude `target/`, `.git/`, `obsidian-vault/`, `v*-plans/`.
5. Align docs:
   - `agentic-os-deployment.md`
   - `README.md`
6. Validate boot lifecycle:
   - first start,
   - restart,
   - data persistence across restarts.

## Files changed

| File | Change |
|------|--------|
| `Dockerfile` | Canonical multi-stage image build |
| `docker-compose.yml` | Runtime orchestration with volumes and health |
| `.env.example` | Environment template |
| `.dockerignore` | Exclude build artifacts, docs, git |
| `agentic-os-deployment.md` | Updated deploy commands |
| `README.md` | Deployment quick start |

## Dependencies

- **Requires:** [[01-quality-gates-stabilization]], [[02-production-config-baseline]].
- **Blocks:** [[04-security-gate-closure]], [[05-release-process-and-cutover]].

## Test plan

- Build image locally — must complete without errors.
- Start stack and verify health endpoint responds on port 9091.
- Restart stack and verify persisted state is intact (vault DB, audit DB).
- Verify LLM endpoint is configurable via environment variable.

## Verification

```bash
docker build -t agentos/core:local .
docker compose up -d
docker compose ps
curl -s http://localhost:9091/healthz
docker compose logs --tail=100 agentos
docker compose down
docker compose up -d
# Verify data persisted
docker compose exec agentos ls /var/lib/agentos/data/
```
