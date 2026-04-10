"""I2C tool implementations for Raspberry Pi.

Uses smbus2 when available, falls back to mock mode on non-Pi hosts.
"""

from __future__ import annotations

import logging
from typing import Any

logger = logging.getLogger("pi-hardware-mcp.i2c")

_backend: str = "mock"
_smbus = None

try:
    import smbus2  # type: ignore[import-untyped]
    _smbus = smbus2
    _backend = "smbus2"
    logger.info("I2C backend: smbus2")
except ImportError:
    logger.info("I2C backend: mock (smbus2 not installed)")

# ---------------------------------------------------------------------------
# Core functions
# ---------------------------------------------------------------------------

def _scan_bus(bus: int = 1) -> dict:
    """Scan an I2C bus for responding devices."""
    devices = []

    if _backend == "smbus2":
        with _smbus.SMBus(bus) as b:
            for addr in range(0x03, 0x78):
                try:
                    b.read_byte(addr)
                    devices.append({
                        "address": f"0x{addr:02x}",
                        "address_int": addr,
                        "responding": True,
                    })
                except OSError:
                    pass
        return {"bus": bus, "devices": devices, "count": len(devices), "backend": "smbus2"}

    # Mock: return a simulated BME280 + SSD1306.
    devices = [
        {"address": "0x76", "address_int": 0x76, "responding": True},
        {"address": "0x3c", "address_int": 0x3C, "responding": True},
    ]
    return {"bus": bus, "devices": devices, "count": len(devices), "backend": "mock", "simulated": True}


def _read_bytes(bus: int, address: int, register: int, length: int) -> dict:
    """Read bytes from an I2C device register."""
    if _backend == "smbus2":
        with _smbus.SMBus(bus) as b:
            data = b.read_i2c_block_data(address, register, length)
        return {
            "bus": bus,
            "address": f"0x{address:02x}",
            "register": f"0x{register:02x}",
            "data": data,
            "length": len(data),
            "backend": "smbus2",
        }

    # Mock.
    data = [0x00] * length
    return {
        "bus": bus,
        "address": f"0x{address:02x}",
        "register": f"0x{register:02x}",
        "data": data,
        "length": length,
        "backend": "mock",
        "simulated": True,
    }


def _write_bytes(bus: int, address: int, register: int, data: list[int]) -> dict:
    """Write bytes to an I2C device register."""
    if _backend == "smbus2":
        with _smbus.SMBus(bus) as b:
            b.write_i2c_block_data(address, register, data)
        return {
            "bus": bus,
            "address": f"0x{address:02x}",
            "register": f"0x{register:02x}",
            "written": len(data),
            "backend": "smbus2",
        }

    return {
        "bus": bus,
        "address": f"0x{address:02x}",
        "register": f"0x{register:02x}",
        "written": len(data),
        "backend": "mock",
        "simulated": True,
    }


# ---------------------------------------------------------------------------
# MCP tool wrappers
# ---------------------------------------------------------------------------

def handle_i2c_scan(args: dict) -> dict:
    bus = int(args.get("bus", 1))
    if bus < 0 or bus > 10:
        raise ValueError(f"Invalid I2C bus: {bus} (must be 0-10)")
    return _scan_bus(bus)


def handle_i2c_read(args: dict) -> dict:
    bus = int(args.get("bus", 1))
    address = int(args["address"])
    register = int(args.get("register", 0))
    length = int(args.get("length", 1))
    if bus < 0 or bus > 10:
        raise ValueError(f"Invalid I2C bus: {bus} (must be 0-10)")
    if address < 0x03 or address > 0x77:
        raise ValueError(f"Invalid I2C address: 0x{address:02x} (must be 0x03-0x77)")
    if register < 0 or register > 255:
        raise ValueError(f"Invalid register: 0x{register:02x} (must be 0x00-0xFF)")
    if length < 1 or length > 32:
        raise ValueError(f"Invalid read length: {length} (must be 1-32)")
    return _read_bytes(bus, address, register, length)


def handle_i2c_write(args: dict) -> dict:
    bus = int(args.get("bus", 1))
    address = int(args["address"])
    register = int(args.get("register", 0))
    data = [int(b) for b in args["data"]]
    if bus < 0 or bus > 10:
        raise ValueError(f"Invalid I2C bus: {bus} (must be 0-10)")
    if address < 0x03 or address > 0x77:
        raise ValueError(f"Invalid I2C address: 0x{address:02x} (must be 0x03-0x77)")
    if register < 0 or register > 255:
        raise ValueError(f"Invalid register: 0x{register:02x} (must be 0x00-0xFF)")
    if len(data) < 1 or len(data) > 32:
        raise ValueError(f"Invalid write length: {len(data)} (must be 1-32)")
    for b in data:
        if not (0 <= b <= 255):
            raise ValueError(f"Invalid byte value: {b} (must be 0-255)")
    return _write_bytes(bus, address, register, data)


# ---------------------------------------------------------------------------
# Tool definitions
# ---------------------------------------------------------------------------

TOOL_DEFINITIONS = [
    {
        "name": "i2c-scan",
        "description": "Scan an I2C bus for connected devices. Returns list of responding addresses.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "bus": {
                    "type": "integer",
                    "description": "I2C bus number (default 1 for Pi)",
                    "default": 1,
                },
            },
        },
    },
    {
        "name": "i2c-read",
        "description": "Read bytes from an I2C device register. Returns raw byte array.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "bus": {
                    "type": "integer",
                    "description": "I2C bus number",
                    "default": 1,
                },
                "address": {
                    "type": "integer",
                    "description": "I2C device address (0x03-0x77)",
                },
                "register": {
                    "type": "integer",
                    "description": "Register address to read from",
                    "default": 0,
                },
                "length": {
                    "type": "integer",
                    "description": "Number of bytes to read (1-32)",
                    "default": 1,
                },
            },
            "required": ["address"],
        },
    },
    {
        "name": "i2c-write",
        "description": "Write bytes to an I2C device register.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "bus": {
                    "type": "integer",
                    "description": "I2C bus number",
                    "default": 1,
                },
                "address": {
                    "type": "integer",
                    "description": "I2C device address (0x03-0x77)",
                },
                "register": {
                    "type": "integer",
                    "description": "Register address to write to",
                    "default": 0,
                },
                "data": {
                    "type": "array",
                    "items": {"type": "integer"},
                    "description": "Bytes to write (array of 0-255 values)",
                },
            },
            "required": ["address", "data"],
        },
    },
]

TOOL_HANDLERS = {
    "i2c-scan": handle_i2c_scan,
    "i2c-read": handle_i2c_read,
    "i2c-write": handle_i2c_write,
}
