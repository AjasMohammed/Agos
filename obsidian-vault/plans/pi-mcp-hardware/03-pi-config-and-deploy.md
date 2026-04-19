---
title: "Phase 3: Pi Config and Deployment"
tags:
  - hal
  - mcp
  - iot
  - phase-3
date: 2026-04-08
status: planned
effort: 2h
priority: medium
---

# Phase 3: Pi Config and Deployment

> Create a Pi-specific AgentOS config and setup script for one-command deployment.

---

## Why This Phase

Even with the MCP server built, deploying on a Pi Zero requires enabling kernel modules (I2C, SPI), adding the user to hardware groups, and configuring AgentOS with appropriate resource limits for the Pi's constrained hardware.

## Current → Target State

**Current:** Only `config/default.toml` exists (desktop-oriented, 128K context budget).
**Target:** `config/pi.toml` with Pi-optimized settings + `connectors/pi-hardware/setup.sh` for one-command setup.

## Detailed Subtasks

### 1. Create `config/pi.toml`

Key differences from default:
- Reduced `context_budget.total_tokens` (32K for Pi Zero's 512MB RAM)
- MCP server entry for `pi-hardware`
- Lower `max_concurrent_tasks` (2 instead of 4)
- Ollama host pointing to network LLM server (Pi Zero can't run LLMs locally)

### 2. Create `connectors/pi-hardware/setup.sh`

- Enable I2C, SPI kernel modules via `raspi-config nonint`
- Add current user to `gpio`, `i2c`, `spi`, `dialout` groups
- Install Python dependencies
- Verify hardware access

### 3. Add deployment README

Document: prerequisites, installation, config, first run, troubleshooting.

## Files Changed

| File | Change |
|------|--------|
| `config/pi.toml` | NEW — Pi-optimized kernel config |
| `connectors/pi-hardware/setup.sh` | NEW — Pi setup script |
| `connectors/pi-hardware/README.md` | NEW — deployment guide |

## Dependencies

- Phase 1 complete (MCP server exists)

## Verification

```bash
# On Pi:
./connectors/pi-hardware/setup.sh
./dev.sh AGENTOS_CONFIG=config/pi.toml
agentos mcp status  # should show pi-hardware: connected
```
