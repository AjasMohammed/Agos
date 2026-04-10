# Pi Hardware MCP Server

MCP server that exposes Raspberry Pi GPIO, I2C, SPI, UART, and PWM as AgentOS tools.

## Tools Provided

| Tool | Protocol | Description |
|------|----------|-------------|
| `gpio-read` | GPIO | Read a pin value (BCM numbering) |
| `gpio-write` | GPIO | Set a pin HIGH or LOW |
| `gpio-watch` | GPIO | Wait for edge event with timeout |
| `gpio-list` | GPIO | List available GPIO pins |
| `i2c-scan` | I2C | Scan bus for connected devices |
| `i2c-read` | I2C | Read bytes from device register |
| `i2c-write` | I2C | Write bytes to device register |
| `spi-transfer` | SPI | Full-duplex SPI transfer |
| `spi-read` | SPI | Read-only SPI transfer |
| `uart-list` | UART | List available serial ports |
| `uart-send` | UART | Send data over serial port |
| `uart-recv` | UART | Receive data with timeout |
| `pwm-set` | PWM | Set frequency and duty cycle |
| `pwm-stop` | PWM | Stop PWM output on a pin |
| `pwm-list` | PWM | List active PWM outputs |

## Quick Start

### On the Raspberry Pi

```bash
# 1. Run the setup script (enables I2C, SPI, serial; installs deps)
sudo ./setup.sh

# 2. Reboot if prompted
sudo reboot

# 3. Start AgentOS with Pi config
AGENTOS_CONFIG=config/pi.toml ./dev.sh

# 4. Verify MCP connection
agentos mcp status
# → pi-hardware: connected (15 tools)
```

### Grant hardware permissions to an agent

```bash
# GPIO access
agentos permission grant my-agent "hardware.gpio:rw"

# I2C access
agentos permission grant my-agent "hardware.i2c:rwx"

# SPI access
agentos permission grant my-agent "hardware.spi:rwx"

# UART access
agentos permission grant my-agent "hardware.uart:rwx"

# PWM access
agentos permission grant my-agent "hardware.pwm:wx"
```

## Development (non-Pi host)

The server runs in **mock mode** on non-Pi hosts — all tools return simulated data:

```bash
# Install in dev mode
pip install -e .

# Test the server
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' | python3 -m pi_hardware_mcp.server

# Or add to default.toml for testing
# [[mcp.servers]]
# name = "pi-hardware"
# command = "python3"
# args = ["-m", "pi_hardware_mcp.server"]
```

## Prerequisites

| Requirement | Notes |
|-------------|-------|
| Python 3.9+ | Pre-installed on Raspberry Pi OS |
| I2C enabled | `sudo raspi-config nonint do_i2c 0` |
| SPI enabled | `sudo raspi-config nonint do_spi 0` |
| Serial enabled | `sudo raspi-config nonint do_serial_hw 0` |
| User in groups | `gpio`, `i2c`, `spi`, `dialout` |

## Architecture

```
AgentOS Kernel
    │
    ├── McpSupervisor (stdio transport)
    │       │
    │       └── pi-hardware-mcp (Python process)
    │               ├── gpio.py   → gpiozero / RPi.GPIO / mock
    │               ├── i2c.py    → smbus2 / mock
    │               ├── spi.py    → spidev / mock
    │               ├── uart.py   → pyserial / mock
    │               └── pwm.py    → gpiozero / RPi.GPIO / mock
    │
    └── PermissionSet enforcement
            hardware.gpio:rw
            hardware.i2c:rwx
            hardware.spi:rwx
            hardware.uart:rwx
            hardware.pwm:wx
```

## Network LLM

Pi Zero cannot run LLMs locally. Configure `config/pi.toml` to point at a
network Ollama instance or cloud API:

```toml
[ollama]
host = "http://192.168.1.100:11434"   # Your LLM server
default_model = "llama3.2"
```

Or use OpenAI/Anthropic/Gemini API keys via the vault:

```bash
agentos secret set OPENAI_API_KEY sk-...
```
