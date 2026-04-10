"""PWM tool implementations for Raspberry Pi.

Uses gpiozero when available, falls back to RPi.GPIO, or mock mode.
"""

from __future__ import annotations

import logging
import threading
from typing import Any

logger = logging.getLogger("pi-hardware-mcp.pwm")

_backend: str = "mock"

try:
    from gpiozero import PWMOutputDevice  # type: ignore[import-untyped]
    _backend = "gpiozero"
    logger.info("PWM backend: gpiozero")
except ImportError:
    try:
        import RPi.GPIO  # type: ignore[import-untyped]
        _backend = "rpigpio"
        logger.info("PWM backend: RPi.GPIO")
    except (ImportError, RuntimeError):
        logger.info("PWM backend: mock (no Pi hardware detected)")

# Track active PWM outputs for stop/cleanup.  Protected by lock for
# thread safety when the server dispatches tool calls to a thread pool.
_active_pwm: dict[int, Any] = {}
_active_pwm_lock = threading.Lock()

# ---------------------------------------------------------------------------
# Core functions
# ---------------------------------------------------------------------------

def _set_pwm(pin: int, frequency: float, duty_cycle: float) -> dict:
    """Set PWM output on a pin."""
    if _backend == "gpiozero":
        with _active_pwm_lock:
            # Close existing PWM on this pin if any.
            if pin in _active_pwm:
                _active_pwm[pin].close()

            device = PWMOutputDevice(pin, frequency=frequency)
            device.value = duty_cycle
            _active_pwm[pin] = device
        return {
            "pin": pin,
            "frequency": frequency,
            "duty_cycle": duty_cycle,
            "active": True,
            "backend": "gpiozero",
        }

    if _backend == "rpigpio":
        import RPi.GPIO as GPIO  # type: ignore[import-untyped]
        with _active_pwm_lock:
            # Stop existing PWM on this pin.
            if pin in _active_pwm:
                _active_pwm[pin].stop()

            GPIO.setup(pin, GPIO.OUT)
            p = GPIO.PWM(pin, frequency)
            p.start(duty_cycle * 100)  # RPi.GPIO uses 0-100 scale
            _active_pwm[pin] = p
        return {
            "pin": pin,
            "frequency": frequency,
            "duty_cycle": duty_cycle,
            "active": True,
            "backend": "rpigpio",
        }

    # Mock.
    with _active_pwm_lock:
        _active_pwm[pin] = {"frequency": frequency, "duty_cycle": duty_cycle}
    return {
        "pin": pin,
        "frequency": frequency,
        "duty_cycle": duty_cycle,
        "active": True,
        "backend": "mock",
        "simulated": True,
    }


def _stop_pwm(pin: int) -> dict:
    """Stop PWM output on a pin."""
    with _active_pwm_lock:
        if pin in _active_pwm:
            obj = _active_pwm.pop(pin)
            if _backend == "gpiozero":
                obj.close()
            elif _backend == "rpigpio":
                obj.stop()

    return {"pin": pin, "active": False, "backend": _backend}


def _list_pwm() -> dict:
    """List currently active PWM outputs."""
    active = []
    with _active_pwm_lock:
        for pin, obj in _active_pwm.items():
            if _backend == "gpiozero":
                active.append({
                    "pin": pin,
                    "frequency": obj.frequency if hasattr(obj, "frequency") else None,
                    "duty_cycle": obj.value if hasattr(obj, "value") else None,
                })
            elif _backend == "mock" and isinstance(obj, dict):
                active.append({"pin": pin, **obj})
            else:
                active.append({"pin": pin})
    return {"active_outputs": active, "count": len(active), "backend": _backend}


# ---------------------------------------------------------------------------
# MCP tool wrappers
# ---------------------------------------------------------------------------

# Hardware PWM pins on Pi: GPIO 12, 13, 18, 19.
# Software PWM: any GPIO pin (via gpiozero).
HARDWARE_PWM_PINS = {12, 13, 18, 19}


def handle_pwm_set(args: dict) -> dict:
    pin = int(args["pin"])
    frequency = float(args.get("frequency", 1000))
    duty_cycle = float(args["duty_cycle"])
    if pin < 0 or pin > 27:
        raise ValueError(f"Invalid BCM pin: {pin} (must be 0-27)")
    if frequency < 1 or frequency > 100000:
        raise ValueError(f"Invalid frequency: {frequency}Hz (must be 1-100000)")
    if duty_cycle < 0.0 or duty_cycle > 1.0:
        raise ValueError(f"Invalid duty cycle: {duty_cycle} (must be 0.0-1.0)")
    return _set_pwm(pin, frequency, duty_cycle)


def handle_pwm_stop(args: dict) -> dict:
    pin = int(args["pin"])
    if pin < 0 or pin > 27:
        raise ValueError(f"Invalid BCM pin: {pin}")
    return _stop_pwm(pin)


def handle_pwm_list(args: dict) -> dict:
    return _list_pwm()


# ---------------------------------------------------------------------------
# Tool definitions
# ---------------------------------------------------------------------------

TOOL_DEFINITIONS = [
    {
        "name": "pwm-set",
        "description": "Set PWM output on a GPIO pin with specified frequency and duty cycle (0.0-1.0).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "pin": {
                    "type": "integer",
                    "description": "BCM GPIO pin number (hardware PWM: 12, 13, 18, 19; software PWM: any)",
                },
                "frequency": {
                    "type": "number",
                    "description": "PWM frequency in Hz (1-100000)",
                    "default": 1000,
                },
                "duty_cycle": {
                    "type": "number",
                    "description": "Duty cycle from 0.0 (off) to 1.0 (fully on)",
                },
            },
            "required": ["pin", "duty_cycle"],
        },
    },
    {
        "name": "pwm-stop",
        "description": "Stop PWM output on a GPIO pin and release the resource.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "pin": {
                    "type": "integer",
                    "description": "BCM GPIO pin number to stop",
                },
            },
            "required": ["pin"],
        },
    },
    {
        "name": "pwm-list",
        "description": "List all currently active PWM outputs with their frequency and duty cycle.",
        "inputSchema": {
            "type": "object",
            "properties": {},
        },
    },
]

TOOL_HANDLERS = {
    "pwm-set": handle_pwm_set,
    "pwm-stop": handle_pwm_stop,
    "pwm-list": handle_pwm_list,
}
