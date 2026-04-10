"""GPIO tool implementations for Raspberry Pi.

Uses gpiozero when available, falls back to sysfs, or runs in mock mode
on non-Pi hosts.
"""

from __future__ import annotations

import logging
import threading
import time
from typing import Any

logger = logging.getLogger("pi-hardware-mcp.gpio")

# ---------------------------------------------------------------------------
# Backend detection
# ---------------------------------------------------------------------------

_backend: str = "mock"
_Device = None
_LED = None
_Button = None

try:
    from gpiozero import Device, LED, Button  # type: ignore[import-untyped]
    _Device = Device
    _LED = LED
    _Button = Button
    _backend = "gpiozero"
    logger.info("GPIO backend: gpiozero")
except ImportError:
    try:
        import RPi.GPIO as _rpi_gpio  # type: ignore[import-untyped]
        _backend = "rpigpio"
        _rpi_gpio.setmode(_rpi_gpio.BCM)
        _rpi_gpio.setwarnings(False)
        logger.info("GPIO backend: RPi.GPIO")
    except (ImportError, RuntimeError, ValueError):
        logger.info("GPIO backend: mock (no Pi hardware detected)")

# Track active outputs for cleanup.  Protected by lock for thread safety
# when the server dispatches tool calls to a thread pool.
_active_outputs: dict[int, Any] = {}
_active_outputs_lock = threading.Lock()

# ---------------------------------------------------------------------------
# Core functions
# ---------------------------------------------------------------------------

def _read_pin(pin: int, pull: str = "none") -> dict:
    """Read a GPIO pin value."""
    if _backend == "gpiozero":
        from gpiozero import DigitalInputDevice  # type: ignore[import-untyped]
        pull_up = True if pull == "up" else (False if pull == "down" else None)
        device = DigitalInputDevice(pin, pull_up=pull_up)
        value = device.value
        device.close()
        return {"pin": pin, "value": int(value), "mode": "input", "backend": "gpiozero"}

    if _backend == "rpigpio":
        import RPi.GPIO as GPIO  # type: ignore[import-untyped]
        pull_map = {"up": GPIO.PUD_UP, "down": GPIO.PUD_DOWN, "none": GPIO.PUD_OFF}
        GPIO.setup(pin, GPIO.IN, pull_up_down=pull_map.get(pull, GPIO.PUD_OFF))
        value = GPIO.input(pin)
        return {"pin": pin, "value": int(value), "mode": "input", "backend": "rpigpio"}

    # Mock mode — return simulated values.
    return {"pin": pin, "value": 0, "mode": "input", "backend": "mock", "simulated": True}


def _write_pin(pin: int, value: int) -> dict:
    """Set a GPIO pin output value."""
    value = 1 if value else 0

    if _backend == "gpiozero":
        with _active_outputs_lock:
            # Close any previous output on this pin.
            if pin in _active_outputs:
                _active_outputs[pin].close()

            led = _LED(pin)
            if value:
                led.on()
            else:
                led.off()
            _active_outputs[pin] = led
        return {"pin": pin, "value": value, "mode": "output", "backend": "gpiozero"}

    if _backend == "rpigpio":
        import RPi.GPIO as GPIO  # type: ignore[import-untyped]
        GPIO.setup(pin, GPIO.OUT)
        GPIO.output(pin, value)
        return {"pin": pin, "value": value, "mode": "output", "backend": "rpigpio"}

    return {"pin": pin, "value": value, "mode": "output", "backend": "mock", "simulated": True}


def _watch_pin(pin: int, edge: str = "both", timeout_ms: int = 5000) -> dict:
    """Wait for an edge event on a GPIO pin."""
    if _backend == "gpiozero":
        from gpiozero import DigitalInputDevice  # type: ignore[import-untyped]
        timeout_s = timeout_ms / 1000.0
        device = DigitalInputDevice(pin)
        if edge == "falling":
            triggered = device.wait_for_inactive(timeout=timeout_s)
        elif edge == "rising":
            triggered = device.wait_for_active(timeout=timeout_s)
        else:  # "both" — wait for either transition
            triggered = (
                device.wait_for_active(timeout=timeout_s / 2)
                or device.wait_for_inactive(timeout=timeout_s / 2)
            )
        val = int(device.value)
        device.close()
        return {
            "pin": pin, "triggered": bool(triggered), "value": val,
            "edge": edge, "backend": "gpiozero",
        }

    if _backend == "rpigpio":
        import RPi.GPIO as GPIO  # type: ignore[import-untyped]
        edge_map = {
            "rising": GPIO.RISING,
            "falling": GPIO.FALLING,
            "both": GPIO.BOTH,
        }
        GPIO.setup(pin, GPIO.IN)
        channel = GPIO.wait_for_edge(
            pin, edge_map.get(edge, GPIO.BOTH), timeout=timeout_ms,
        )
        triggered = channel is not None
        val = GPIO.input(pin) if triggered else 0
        return {
            "pin": pin, "triggered": triggered, "value": int(val),
            "edge": edge, "backend": "rpigpio",
        }

    # Mock: simulate no event within timeout.
    time.sleep(min(timeout_ms / 1000.0, 0.1))
    return {
        "pin": pin, "triggered": False, "value": 0,
        "edge": edge, "backend": "mock", "simulated": True,
    }


def _list_pins() -> dict:
    """List available GPIO pins and their current state."""
    # Standard BCM pins on Pi Zero / Zero 2 W (40-pin header).
    bcm_pins = [2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
                16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27]
    pins = []
    for p in bcm_pins:
        pins.append({"pin": p, "available": True})
    return {"pins": pins, "backend": _backend, "count": len(bcm_pins)}


# ---------------------------------------------------------------------------
# MCP tool wrappers (called by server.py)
# ---------------------------------------------------------------------------

def handle_gpio_read(args: dict) -> dict:
    pin = int(args["pin"])
    pull = str(args.get("pull", "none"))
    if pin < 0 or pin > 27:
        raise ValueError(f"Invalid BCM pin number: {pin} (must be 0-27)")
    if pull not in ("up", "down", "none"):
        raise ValueError(f"Invalid pull mode: {pull} (must be up/down/none)")
    return _read_pin(pin, pull)


def handle_gpio_write(args: dict) -> dict:
    pin = int(args["pin"])
    value = int(args["value"])
    if pin < 0 or pin > 27:
        raise ValueError(f"Invalid BCM pin number: {pin} (must be 0-27)")
    return _write_pin(pin, value)


def handle_gpio_watch(args: dict) -> dict:
    pin = int(args["pin"])
    edge = str(args.get("edge", "both"))
    timeout_ms = int(args.get("timeout_ms", 5000))
    if pin < 0 or pin > 27:
        raise ValueError(f"Invalid BCM pin number: {pin} (must be 0-27)")
    if edge not in ("rising", "falling", "both"):
        raise ValueError(f"Invalid edge: {edge} (must be rising/falling/both)")
    if timeout_ms < 0 or timeout_ms > 30000:
        raise ValueError(f"Invalid timeout: {timeout_ms}ms (must be 0-30000)")
    return _watch_pin(pin, edge, timeout_ms)


def handle_gpio_list(args: dict) -> dict:
    return _list_pins()


# ---------------------------------------------------------------------------
# Tool definitions (imported by server.py)
# ---------------------------------------------------------------------------

TOOL_DEFINITIONS = [
    {
        "name": "gpio-read",
        "description": "Read the value of a Raspberry Pi GPIO pin (BCM numbering). Returns 0 or 1.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "pin": {
                    "type": "integer",
                    "description": "BCM GPIO pin number (0-27)",
                },
                "pull": {
                    "type": "string",
                    "enum": ["up", "down", "none"],
                    "description": "Internal pull resistor mode",
                    "default": "none",
                },
            },
            "required": ["pin"],
        },
    },
    {
        "name": "gpio-write",
        "description": "Set a Raspberry Pi GPIO pin to HIGH (1) or LOW (0). Uses BCM numbering.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "pin": {
                    "type": "integer",
                    "description": "BCM GPIO pin number (0-27)",
                },
                "value": {
                    "type": "integer",
                    "enum": [0, 1],
                    "description": "Pin value: 0=LOW, 1=HIGH",
                },
            },
            "required": ["pin", "value"],
        },
    },
    {
        "name": "gpio-watch",
        "description": "Wait for an edge event (rising, falling, or both) on a GPIO pin with a timeout.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "pin": {
                    "type": "integer",
                    "description": "BCM GPIO pin number (0-27)",
                },
                "edge": {
                    "type": "string",
                    "enum": ["rising", "falling", "both"],
                    "description": "Edge to wait for",
                    "default": "both",
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "Timeout in milliseconds (max 30000)",
                    "default": 5000,
                },
            },
            "required": ["pin"],
        },
    },
    {
        "name": "gpio-list",
        "description": "List all available GPIO pins on the Raspberry Pi with their BCM numbers.",
        "inputSchema": {
            "type": "object",
            "properties": {},
        },
    },
]

TOOL_HANDLERS = {
    "gpio-read": handle_gpio_read,
    "gpio-write": handle_gpio_write,
    "gpio-watch": handle_gpio_watch,
    "gpio-list": handle_gpio_list,
}
