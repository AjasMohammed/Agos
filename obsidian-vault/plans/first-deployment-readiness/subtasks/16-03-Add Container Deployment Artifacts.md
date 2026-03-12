---
title: Add Container Deployment Artifacts
tags:
  - deployment
  - v3
  - plan
date: 2026-03-12
status: planned
effort: 1d
priority: critical
---

# Add Container Deployment Artifacts

> Add canonical Docker deployment files required by the Stage 1 shipping target.

## Why this sub-task

Deployment strategy names Docker-on-Linux as first release target, but repository artifacts are missing. Without concrete artifacts, deployment is not repeatable.

## Current -> Target State

- **Current:** No `Dockerfile` / `docker-compose` in repository root.
- **Target:** production-ready container artifacts with health checks, persistent volumes, non-root runtime, and rollback guidance.

## What to Do

1. Create build/runtime container specification:
   - `Dockerfile`
2. Add orchestration baseline:
   - `docker-compose.yml`
3. Add environment contract:
   - `.env.example` (non-secret values only)
4. Add service health command and startup assumptions:
   - ensure `agentctl status` or kernel health endpoint integration.
5. Document launch and rollback commands in deployment docs.

## Files Changed

| File | Change |
|------|--------|
| `Dockerfile` | Multi-stage build and runtime image |
| `docker-compose.yml` | Persistent volumes and service wiring |
| `.env.example` | Deployment variables template |
| `agentic-os-deployment.md` | Align examples with committed artifacts |
| `README.md` | Add quick deploy instructions |

## Expected Inputs and Outputs

- **Input:** release buildable workspace.
- **Output:** reproducible containerized deployment path.

## Prerequisites

- [[16-01-Restore Quality Gates]]
- [[16-02-Harden Production Config]]

## Verification

```bash
docker build -t agentos/core:local .
docker compose up -d
docker compose ps
docker compose logs --tail=100 agentos
```

Pass criteria:
- Container starts and remains healthy.
- Persistent directories survive restart.
