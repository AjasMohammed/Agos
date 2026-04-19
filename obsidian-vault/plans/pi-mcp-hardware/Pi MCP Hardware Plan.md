---
title: Pi MCP Hardware Bridge
tags:
  - hal
  - mcp
  - iot
  - plan
date: 2026-04-08
status: in-progress
effort: 2d
priority: high
---

# Pi MCP Hardware Bridge

> A pluggable MCP server that exposes Raspberry Pi GPIO, I2C, SPI, UART, and PWM as AgentOS tools — no kernel recompile needed.

---

## Why This Matters

AgentOS has 15 HAL drivers but none for embedded protocols (GPIO, I2C, SPI, UART, PWM). The Pi Zero is a prime deployment target for physical-world agent tasks, but agents currently cannot interact with sensors, actuators, or serial peripherals.

Rather than baking Pi support into the kernel (which would add ARM-only dependencies to every build), we use the **existing MCP integration** to bridge hardware access as a separate, pluggable service.

## Current State

| Component | Status |
|-----------|--------|
| MCP supervisor | Fully operational — stdio + HTTP transports, auto-reconnect, health checks |
| MCP tool registration | Auto-discovers tools from servers, registers as `TrustTier::Community` |
| MCP security | Rate limiting, allowed/denied tool lists, output validation, audit logging |
| Pi GPIO/I2C/SPI/UART | Not supported — no drivers, no crates, no sysfs scanning |

## Target Architecture

```
┌─────────────────────────────────────────────┐
│              AgentOS Kernel                  │
│                                              │
│  McpSupervisor ──stdio──► pi-hardware MCP   │
│       │                    server (Python)   │
│       │                    ┌───────────────┐ │
│  ToolRunner registers:     │ gpio.py       │ │
│   - gpio-read              │ i2c.py        │ │
│   - gpio-write             │ spi.py        │ │
│   - gpio-watch             │ uart.py       │ │
│   - i2c-scan               │ pwm.py        │ │
│   - i2c-read               │ system.py     │ │
│   - i2c-write              └───────────────┘ │
│   - spi-transfer                             │
│   - uart-send/recv                           │
│   - pwm-set                                  │
│                                              │
│  PermissionSet enforces:                     │
│   hardware.gpio:rw                           │
│   hardware.i2c:rwx                           │
│   hardware.spi:rwx                           │
│   hardware.uart:rwx                          │
│   hardware.pwm:wx                            │
└─────────────────────────────────────────────┘
```

## Phase Overview

| Phase | Name | Effort | Dependencies | Detail Doc | Status |
|-------|------|--------|-------------|------------|--------|
| 1 | MCP server core + GPIO tools | 4h | None | [[01-mcp-server-and-gpio]] | in-progress |
| 2 | I2C, SPI, UART, PWM tools | 4h | Phase 1 | [[02-i2c-spi-uart-pwm]] | planned |
| 3 | Pi config + deployment | 2h | Phase 1 | [[03-pi-config-and-deploy]] | planned |

## Key Design Decisions

1. **Python over Rust** — Pi GPIO libraries (`gpiozero`, `RPi.GPIO`, `smbus2`, `spidev`) are mature and well-tested in Python. A Rust MCP server would require `rppal` which is less battle-tested.
2. **MCP over HAL driver** — Avoids adding ARM-only dependencies to the workspace. The MCP server is a separate process that can be installed independently on the Pi.
3. **Stdio transport** — Kernel spawns the Python process and communicates via JSON-RPC over stdin/stdout. No network ports needed.
4. **Permission mapping** — Each tool declares its required permission resource (e.g. `hardware.gpio`) so the kernel's `PermissionSet` enforcement works unchanged.

## Risks

| Risk | Mitigation |
|------|------------|
| Pi Zero has limited RAM (512MB) | Python server is lightweight (~10MB); kernel can run with reduced context budget |
| GPIO requires root or gpio group | Setup script adds user to `gpio` group; document in README |
| I2C/SPI need kernel modules enabled | Setup script checks and enables via `raspi-config` non-interactive |
| MCP stdio overhead for high-frequency GPIO | Document that polling >100Hz should use direct `gpiozero` scripts, not MCP |

## Related

- [[MCP configuration]] in `config/default.toml`
- [[Hardware Abstraction Layer]] in `crates/agentos-hal/`
- Kernel MCP boot: `crates/agentos-kernel/src/kernel.rs` lines 2036–2198
