"""UART/Serial tool implementations for Raspberry Pi.

Uses pyserial when available, falls back to mock mode on non-Pi hosts.
"""

from __future__ import annotations

import logging
import os
import re
from typing import Any

logger = logging.getLogger("pi-hardware-mcp.uart")

_backend: str = "mock"
_serial = None

try:
    import serial  # type: ignore[import-untyped]
    import serial.tools.list_ports  # type: ignore[import-untyped]
    _serial = serial
    _backend = "pyserial"
    logger.info("UART backend: pyserial")
except ImportError:
    logger.info("UART backend: mock (pyserial not installed)")

# ---------------------------------------------------------------------------
# Core functions
# ---------------------------------------------------------------------------

def _list_ports() -> dict:
    """List available serial ports."""
    if _backend == "pyserial":
        ports = []
        for port in _serial.tools.list_ports.comports():
            ports.append({
                "device": port.device,
                "description": port.description,
                "hwid": port.hwid,
            })
        return {"ports": ports, "count": len(ports), "backend": "pyserial"}

    # Mock.
    ports = [
        {"device": "/dev/ttyAMA0", "description": "Pi UART0 (mock)", "hwid": "mock"},
        {"device": "/dev/ttyS0", "description": "Pi mini UART (mock)", "hwid": "mock"},
    ]
    return {"ports": ports, "count": len(ports), "backend": "mock", "simulated": True}


def _send(port: str, baud: int, data: str, timeout: float) -> dict:
    """Send string data over a serial port."""
    encoded = data.encode("utf-8")

    if _backend == "pyserial":
        with _serial.Serial(port, baud, timeout=timeout) as ser:
            written = ser.write(encoded)
        return {
            "port": port,
            "baud": baud,
            "bytes_sent": written,
            "backend": "pyserial",
        }

    return {
        "port": port,
        "baud": baud,
        "bytes_sent": len(encoded),
        "backend": "mock",
        "simulated": True,
    }


def _recv(port: str, baud: int, timeout_ms: int, max_bytes: int) -> dict:
    """Receive data from a serial port with timeout."""
    timeout_s = timeout_ms / 1000.0

    if _backend == "pyserial":
        with _serial.Serial(port, baud, timeout=timeout_s) as ser:
            raw = ser.read(max_bytes)
        try:
            text = raw.decode("utf-8", errors="replace")
        except Exception:
            text = raw.hex()
        return {
            "port": port,
            "baud": baud,
            "data": text,
            "bytes_read": len(raw),
            "backend": "pyserial",
        }

    return {
        "port": port,
        "baud": baud,
        "data": "",
        "bytes_read": 0,
        "backend": "mock",
        "simulated": True,
    }


# ---------------------------------------------------------------------------
# MCP tool wrappers
# ---------------------------------------------------------------------------

ALLOWED_BAUDS = {300, 1200, 2400, 4800, 9600, 19200, 38400, 57600, 115200, 230400, 460800, 921600}

# Only allow known Pi serial device patterns — blocks path traversal and
# access to the controlling terminal (/dev/tty itself).
_SAFE_PORT_RE = re.compile(r"^/dev/tty(AMA|S|USB|ACM)\d+$")


def _validate_port(port: str) -> None:
    """Validate that port is a real serial device path with no traversal."""
    if ".." in port:
        raise ValueError(f"Path traversal detected in port: {port}")
    if not _SAFE_PORT_RE.match(port):
        raise ValueError(
            f"Invalid serial port: {port} "
            "(must match /dev/tty{{AMA,S,USB,ACM}}N, e.g. /dev/ttyAMA0)"
        )
    # Resolve symlinks and verify still under /dev/.
    real = os.path.realpath(port)
    if not real.startswith("/dev/"):
        raise ValueError(f"Port resolves outside /dev/: {real}")


def handle_uart_list(args: dict) -> dict:
    return _list_ports()


def handle_uart_send(args: dict) -> dict:
    port = str(args["port"])
    baud = int(args.get("baud", 9600))
    data = str(args["data"])
    timeout = float(args.get("timeout_s", 1.0))
    _validate_port(port)
    if baud not in ALLOWED_BAUDS:
        raise ValueError(f"Invalid baud rate: {baud} (allowed: {sorted(ALLOWED_BAUDS)})")
    if len(data) > 4096:
        raise ValueError(f"Data too large: {len(data)} bytes (max 4096)")
    if timeout < 0.0 or timeout > 30.0:
        raise ValueError(f"Invalid timeout: {timeout}s (must be 0-30)")
    return _send(port, baud, data, timeout)


def handle_uart_recv(args: dict) -> dict:
    port = str(args["port"])
    baud = int(args.get("baud", 9600))
    timeout_ms = int(args.get("timeout_ms", 1000))
    max_bytes = int(args.get("max_bytes", 256))
    _validate_port(port)
    if baud not in ALLOWED_BAUDS:
        raise ValueError(f"Invalid baud rate: {baud}")
    if timeout_ms < 0 or timeout_ms > 30000:
        raise ValueError(f"Invalid timeout: {timeout_ms}ms (must be 0-30000)")
    if max_bytes < 1 or max_bytes > 4096:
        raise ValueError(f"Invalid max_bytes: {max_bytes} (must be 1-4096)")
    return _recv(port, baud, timeout_ms, max_bytes)


# ---------------------------------------------------------------------------
# Tool definitions
# ---------------------------------------------------------------------------

TOOL_DEFINITIONS = [
    {
        "name": "uart-list",
        "description": "List available serial/UART ports on the Raspberry Pi.",
        "inputSchema": {
            "type": "object",
            "properties": {},
        },
    },
    {
        "name": "uart-send",
        "description": "Send string data over a serial/UART port.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "port": {
                    "type": "string",
                    "description": "Serial port device path (e.g. /dev/ttyAMA0)",
                },
                "baud": {
                    "type": "integer",
                    "description": "Baud rate (default 9600)",
                    "default": 9600,
                },
                "data": {
                    "type": "string",
                    "description": "String data to send (max 4096 bytes)",
                },
                "timeout_s": {
                    "type": "number",
                    "description": "Write timeout in seconds",
                    "default": 1.0,
                },
            },
            "required": ["port", "data"],
        },
    },
    {
        "name": "uart-recv",
        "description": "Receive data from a serial/UART port with a timeout.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "port": {
                    "type": "string",
                    "description": "Serial port device path (e.g. /dev/ttyAMA0)",
                },
                "baud": {
                    "type": "integer",
                    "description": "Baud rate (default 9600)",
                    "default": 9600,
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "Read timeout in milliseconds (max 30000)",
                    "default": 1000,
                },
                "max_bytes": {
                    "type": "integer",
                    "description": "Maximum bytes to read (max 4096)",
                    "default": 256,
                },
            },
            "required": ["port"],
        },
    },
]

TOOL_HANDLERS = {
    "uart-list": handle_uart_list,
    "uart-send": handle_uart_send,
    "uart-recv": handle_uart_recv,
}
