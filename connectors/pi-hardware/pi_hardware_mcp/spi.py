"""SPI tool implementations for Raspberry Pi.

Uses spidev when available, falls back to mock mode on non-Pi hosts.
"""

from __future__ import annotations

import logging
from typing import Any

logger = logging.getLogger("pi-hardware-mcp.spi")

_backend: str = "mock"
_spidev = None

try:
    import spidev as _spidev_mod  # type: ignore[import-untyped]
    _spidev = _spidev_mod
    _backend = "spidev"
    logger.info("SPI backend: spidev")
except ImportError:
    logger.info("SPI backend: mock (spidev not installed)")

# ---------------------------------------------------------------------------
# Core functions
# ---------------------------------------------------------------------------

def _transfer(bus: int, device: int, data: list[int], speed_hz: int, mode: int) -> dict:
    """Full-duplex SPI transfer."""
    if _backend == "spidev":
        spi = _spidev.SpiDev()
        spi.open(bus, device)
        spi.max_speed_hz = speed_hz
        spi.mode = mode
        received = spi.xfer2(data)
        spi.close()
        return {
            "bus": bus,
            "device": device,
            "sent": data,
            "received": received,
            "speed_hz": speed_hz,
            "backend": "spidev",
        }

    # Mock: echo back the sent data.
    return {
        "bus": bus,
        "device": device,
        "sent": data,
        "received": [0x00] * len(data),
        "speed_hz": speed_hz,
        "backend": "mock",
        "simulated": True,
    }


def _read(bus: int, device: int, length: int, speed_hz: int, mode: int) -> dict:
    """Read-only SPI transfer (sends zeros, reads response)."""
    data = [0x00] * length

    if _backend == "spidev":
        spi = _spidev.SpiDev()
        spi.open(bus, device)
        spi.max_speed_hz = speed_hz
        spi.mode = mode
        received = spi.xfer2(data)
        spi.close()
        return {
            "bus": bus,
            "device": device,
            "data": received,
            "length": len(received),
            "speed_hz": speed_hz,
            "backend": "spidev",
        }

    return {
        "bus": bus,
        "device": device,
        "data": [0x00] * length,
        "length": length,
        "speed_hz": speed_hz,
        "backend": "mock",
        "simulated": True,
    }


# ---------------------------------------------------------------------------
# MCP tool wrappers
# ---------------------------------------------------------------------------

def handle_spi_transfer(args: dict) -> dict:
    bus = int(args.get("bus", 0))
    device = int(args.get("device", 0))
    data = list(args["data"])
    speed_hz = int(args.get("speed_hz", 1000000))
    mode = int(args.get("mode", 0))
    if bus < 0 or bus > 1:
        raise ValueError(f"Invalid SPI bus: {bus} (must be 0 or 1)")
    if device < 0 or device > 2:
        raise ValueError(f"Invalid SPI device: {device} (must be 0-2)")
    if len(data) < 1 or len(data) > 4096:
        raise ValueError(f"Invalid data length: {len(data)} (must be 1-4096)")
    if speed_hz < 1000 or speed_hz > 125000000:
        raise ValueError(f"Invalid speed: {speed_hz}Hz (must be 1kHz-125MHz)")
    if mode not in (0, 1, 2, 3):
        raise ValueError(f"Invalid SPI mode: {mode} (must be 0-3)")
    for b in data:
        if not (0 <= b <= 255):
            raise ValueError(f"Invalid byte value: {b} (must be 0-255)")
    return _transfer(bus, device, data, speed_hz, mode)


def handle_spi_read(args: dict) -> dict:
    bus = int(args.get("bus", 0))
    device = int(args.get("device", 0))
    length = int(args.get("length", 1))
    speed_hz = int(args.get("speed_hz", 1000000))
    mode = int(args.get("mode", 0))
    if bus < 0 or bus > 1:
        raise ValueError(f"Invalid SPI bus: {bus} (must be 0 or 1)")
    if device < 0 or device > 2:
        raise ValueError(f"Invalid SPI device: {device} (must be 0-2)")
    if length < 1 or length > 4096:
        raise ValueError(f"Invalid read length: {length} (must be 1-4096)")
    if speed_hz < 1000 or speed_hz > 125000000:
        raise ValueError(f"Invalid speed: {speed_hz}Hz (must be 1kHz-125MHz)")
    if mode not in (0, 1, 2, 3):
        raise ValueError(f"Invalid SPI mode: {mode} (must be 0-3)")
    return _read(bus, device, length, speed_hz, mode)


# ---------------------------------------------------------------------------
# Tool definitions
# ---------------------------------------------------------------------------

TOOL_DEFINITIONS = [
    {
        "name": "spi-transfer",
        "description": "Full-duplex SPI transfer. Sends data and returns the simultaneously received bytes.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "bus": {
                    "type": "integer",
                    "description": "SPI bus number (0 or 1)",
                    "default": 0,
                },
                "device": {
                    "type": "integer",
                    "description": "SPI chip select (CE) number (0-2)",
                    "default": 0,
                },
                "data": {
                    "type": "array",
                    "items": {"type": "integer"},
                    "description": "Bytes to send (array of 0-255 values)",
                },
                "speed_hz": {
                    "type": "integer",
                    "description": "SPI clock speed in Hz (default 1MHz)",
                    "default": 1000000,
                },
                "mode": {
                    "type": "integer",
                    "enum": [0, 1, 2, 3],
                    "description": "SPI mode (CPOL/CPHA combination)",
                    "default": 0,
                },
            },
            "required": ["data"],
        },
    },
    {
        "name": "spi-read",
        "description": "Read bytes from an SPI device (sends zeros, captures response).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "bus": {
                    "type": "integer",
                    "description": "SPI bus number",
                    "default": 0,
                },
                "device": {
                    "type": "integer",
                    "description": "SPI chip select number",
                    "default": 0,
                },
                "length": {
                    "type": "integer",
                    "description": "Number of bytes to read (1-4096)",
                    "default": 1,
                },
                "speed_hz": {
                    "type": "integer",
                    "description": "SPI clock speed in Hz",
                    "default": 1000000,
                },
                "mode": {
                    "type": "integer",
                    "enum": [0, 1, 2, 3],
                    "description": "SPI mode",
                    "default": 0,
                },
            },
        },
    },
]

TOOL_HANDLERS = {
    "spi-transfer": handle_spi_transfer,
    "spi-read": handle_spi_read,
}
