---
title: Deployment Infrastructure
tags:
  - deployment
  - reliability
  - plan
  - v3
date: 2026-03-17
status: planned
effort: 0.5d
priority: medium
---

# Phase 07 -- Deployment Infrastructure

> Create a systemd unit file with watchdog integration, resource limits, and restart policy for production deployment on bare-metal or VM hosts.

---

## Why This Phase

The kernel currently has no external process supervisor. When the internal supervisor exhausts its restart budget, the process exits and nothing brings it back. In production, the kernel must be managed by systemd (bare metal/VM) or a container orchestrator (Docker/Kubernetes). The Dockerfile already exists; this phase adds the systemd path.

The watchdog integration is particularly important: the kernel's health server already runs periodic checks. By pinging the systemd watchdog on each successful health check cycle, systemd can detect kernel hangs (not just crashes) and restart the process.

## Sub-tasks

| # | Task | Files | Detail Doc |
|---|------|-------|------------|
| 09 | Systemd unit and watchdog | `deploy/agentos.service` (new), `health.rs` | [[24-09-Systemd Unit and Watchdog]] |

## Dependencies

All other phases should ideally be complete before deploying with the new systemd unit, since the unit depends on graceful shutdown, pre-flight checks, and restart hardening to work correctly. However, the systemd unit file is safe to create at any time.

## Test Plan

- `systemd-analyze verify deploy/agentos.service` passes (syntax check)
- Manual verification: start kernel with `systemctl start agentos`, verify `systemctl status` shows active, stop with `systemctl stop agentos`, verify `KernelShutdown` audit entry written

## Verification

```bash
# Syntax check (does not require root or running systemd)
systemd-analyze verify deploy/agentos.service 2>&1 || true
```
