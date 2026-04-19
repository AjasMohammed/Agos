---
title: "Phase 2: I2C, SPI, UART, PWM Tools"
tags:
  - hal
  - mcp
  - iot
  - phase-2
date: 2026-04-08
status: planned
effort: 4h
priority: high
---

# Phase 2: I2C, SPI, UART, PWM Tools

> Extend the MCP server with I2C, SPI, UART, and PWM tool implementations.

---

## Why This Phase

GPIO alone covers digital pins, but real Pi projects need:
- **I2C** — temperature/humidity sensors (BME280), IMUs (MPU6050), displays (SSD1306)
- **SPI** — ADCs (MCP3008), TFT displays, SD cards
- **UART** — GPS modules, LoRa radios, RS-485 devices
- **PWM** — servo motors, LED brightness, fan speed

## Current → Target State

**Current:** MCP server has GPIO tools only.
**Target:** 10+ additional tools covering all four protocols.

## Tools to Implement

### I2C (`i2c.py`)

- **i2c-scan**: Scan bus for connected devices → `{ bus: int, devices: [{ address: "0x48", responding: true }] }`
- **i2c-read**: Read bytes from device → `{ bus: int, address: int, register: int, length: int }` → `{ data: [int] }`
- **i2c-write**: Write bytes to device → `{ bus: int, address: int, register: int, data: [int] }` → `{ written: int }`

### SPI (`spi.py`)

- **spi-transfer**: Full-duplex transfer → `{ bus: int, device: int, data: [int], speed_hz: int }` → `{ received: [int] }`
- **spi-read**: Read-only transfer → `{ bus: int, device: int, length: int }` → `{ data: [int] }`

### UART (`uart.py`)

- **uart-send**: Send data over serial → `{ port: str, baud: int, data: str }` → `{ bytes_sent: int }`
- **uart-recv**: Receive data with timeout → `{ port: str, baud: int, timeout_ms: int, max_bytes: int }` → `{ data: str, bytes_read: int }`
- **uart-list**: List available serial ports → `{ ports: [{ device: str, description: str }] }`

### PWM (`pwm.py`)

- **pwm-set**: Set PWM duty cycle → `{ pin: int, frequency: float, duty_cycle: float }` → `{ pin: int, active: true }`
- **pwm-stop**: Stop PWM output → `{ pin: int }` → `{ pin: int, active: false }`

## Files Changed

| File | Change |
|------|--------|
| `connectors/pi-hardware/pi_hardware_mcp/i2c.py` | NEW |
| `connectors/pi-hardware/pi_hardware_mcp/spi.py` | NEW |
| `connectors/pi-hardware/pi_hardware_mcp/uart.py` | NEW |
| `connectors/pi-hardware/pi_hardware_mcp/pwm.py` | NEW |
| `connectors/pi-hardware/pi_hardware_mcp/server.py` | Register new tools |
| `connectors/pi-hardware/pyproject.toml` | Add smbus2, spidev, pyserial deps |

## Dependencies

- Phase 1 complete (server skeleton + GPIO working)

## Test Plan

1. `i2c-scan` in mock mode returns simulated device list
2. `spi-transfer` in mock mode returns echo data
3. `uart-list` returns available ports (or mock list)
4. `pwm-set` validates frequency/duty_cycle ranges

## Verification

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}' | python3 -m pi_hardware_mcp.server | python3 -c "import sys,json; tools=json.load(sys.stdin)['result']['tools']; print(f'{len(tools)} tools registered'); assert len(tools) >= 13"
```
