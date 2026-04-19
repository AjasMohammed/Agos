---
title: "Phase 1: MCP Server Core + GPIO Tools"
tags:
  - hal
  - mcp
  - iot
  - phase-1
date: 2026-04-08
status: in-progress
effort: 4h
priority: high
---

# Phase 1: MCP Server Core + GPIO Tools

> Build the Python MCP server skeleton with GPIO read/write/watch tools.

---

## Why This Phase

The MCP server is the foundation — it implements the JSON-RPC protocol that AgentOS's `McpSupervisor` expects. GPIO is the simplest and most universally used Pi peripheral, making it the right first tool set.

## Current → Target State

**Current:** No Pi hardware tools exist. MCP config has `servers = []`.

**Target:** A Python package `pi-hardware-mcp` at `connectors/pi-hardware/` that:
- Implements MCP stdio protocol (initialize, tools/list, tools/call)
- Exposes `gpio-read`, `gpio-write`, `gpio-watch` tools
- Works with `gpiozero` (preferred) and falls back to sysfs
- Runs as a child process spawned by kernel

## Detailed Subtasks

### 1. Create Python package structure

```
connectors/pi-hardware/
├── pyproject.toml
├── pi_hardware_mcp/
│   ├── __init__.py
│   ├── server.py          # MCP stdio JSON-RPC server
│   ├── gpio.py            # GPIO tool implementations
│   ├── i2c.py             # Phase 2
│   ├── spi.py             # Phase 2
│   ├── uart.py            # Phase 2
│   └── pwm.py             # Phase 2
└── README.md
```

### 2. Implement MCP stdio server (`server.py`)

The server reads JSON-RPC requests from stdin, dispatches to tool handlers, writes responses to stdout.

Required MCP methods:
- `initialize` → `{ capabilities: { tools: {} }, serverInfo: { name, version } }`
- `tools/list` → array of tool definitions with `name`, `description`, `inputSchema`
- `tools/call` → `{ name, arguments }` → `{ content: [{ type: "text", text: "..." }] }`

### 3. Implement GPIO tools (`gpio.py`)

**gpio-read**: Read pin value
- Input: `{ pin: int, pull: "up"|"down"|"none" }`
- Output: `{ pin: int, value: 0|1, mode: "input" }`

**gpio-write**: Set pin output
- Input: `{ pin: int, value: 0|1 }`
- Output: `{ pin: int, value: 0|1, mode: "output" }`

**gpio-watch**: Wait for edge event (with timeout)
- Input: `{ pin: int, edge: "rising"|"falling"|"both", timeout_ms: int }`
- Output: `{ pin: int, triggered: bool, value: 0|1, edge: str }`

### 4. Add mock/simulation mode

For development on non-Pi hosts, the server should detect the platform and use mock GPIO values. This allows testing the MCP integration without hardware.

## Files Changed

| File | Change |
|------|--------|
| `connectors/pi-hardware/pyproject.toml` | NEW — package definition |
| `connectors/pi-hardware/pi_hardware_mcp/__init__.py` | NEW — package init |
| `connectors/pi-hardware/pi_hardware_mcp/server.py` | NEW — MCP server |
| `connectors/pi-hardware/pi_hardware_mcp/gpio.py` | NEW — GPIO tools |

## Dependencies

- None (this is the first phase)

## Test Plan

1. Run server in mock mode on dev machine:
   ```bash
   echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' | python3 -m pi_hardware_mcp.server
   ```
2. Verify `tools/list` returns gpio tools with correct schemas
3. Verify `tools/call` with `gpio-read` returns mock data on non-Pi hosts
4. Verify invalid pin numbers return proper MCP error responses

## Verification

```bash
cd connectors/pi-hardware
pip install -e .
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' | python3 -m pi_hardware_mcp.server
# Should return: {"jsonrpc":"2.0","id":1,"result":{"capabilities":{"tools":{}},"serverInfo":{"name":"pi-hardware","version":"0.1.0"}}}
```
