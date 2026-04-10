"""MCP stdio server for Raspberry Pi hardware access.

Reads JSON-RPC requests from stdin, dispatches to tool handlers,
writes JSON-RPC responses to stdout.  Logging goes to stderr so it
never corrupts the stdio JSON-RPC stream.
"""

from __future__ import annotations

import atexit
import concurrent.futures
import json
import logging
import signal
import sys
from typing import Any

from . import gpio, i2c, spi, uart, pwm

logger = logging.getLogger("pi-hardware-mcp")

# ---------------------------------------------------------------------------
# Tool registry
# ---------------------------------------------------------------------------

TOOLS: dict[str, dict[str, Any]] = {}
HANDLERS: dict[str, Any] = {}


def _register(module: Any) -> None:
    """Import all tools defined by a peripheral module."""
    for defn in module.TOOL_DEFINITIONS:
        TOOLS[defn["name"]] = defn
        HANDLERS[defn["name"]] = module.TOOL_HANDLERS[defn["name"]]


_register(gpio)
_register(i2c)
_register(spi)
_register(uart)
_register(pwm)

# ---------------------------------------------------------------------------
# JSON-RPC helpers
# ---------------------------------------------------------------------------

def _ok(id: Any, result: Any) -> dict:
    return {"jsonrpc": "2.0", "id": id, "result": result}


def _error(id: Any, code: int, message: str, data: Any = None) -> dict:
    err: dict[str, Any] = {"code": code, "message": message}
    if data is not None:
        err["data"] = data
    return {"jsonrpc": "2.0", "id": id, "error": err}


# ---------------------------------------------------------------------------
# MCP method dispatch
# ---------------------------------------------------------------------------

def handle_initialize(id: Any, _params: dict) -> dict:
    return _ok(id, {
        "protocolVersion": "2024-11-05",
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "pi-hardware", "version": "0.1.0"},
    })


def handle_tools_list(id: Any, _params: dict) -> dict:
    tools = []
    for defn in TOOLS.values():
        tools.append({
            "name": defn["name"],
            "description": defn["description"],
            "inputSchema": defn["inputSchema"],
        })
    return _ok(id, {"tools": tools})


# Thread pool for tool execution — prevents blocking calls (gpio-watch,
# uart-recv) from stalling the JSON-RPC main loop and causing health
# check timeouts.
_executor = concurrent.futures.ThreadPoolExecutor(max_workers=2)


def handle_tools_call(id: Any, params: dict) -> dict:
    name = params.get("name", "")
    arguments = params.get("arguments", {})

    handler = HANDLERS.get(name)
    if handler is None:
        return _error(id, -32601, f"Unknown tool: {name}")

    try:
        # Run tool in thread pool so blocking calls don't freeze the
        # main stdin reader.  Timeout is 35s (slightly above the max
        # tool timeout of 30s for gpio-watch / uart-recv).
        future = _executor.submit(handler, arguments)
        result = future.result(timeout=35)
        text = json.dumps(result, default=str)
        return _ok(id, {
            "content": [{"type": "text", "text": text}],
        })
    except concurrent.futures.TimeoutError:
        logger.error("Tool %s timed out", name)
        return _ok(id, {
            "content": [{"type": "text", "text": json.dumps({"error": f"Tool '{name}' timed out after 35s"})}],
            "isError": True,
        })
    except Exception as exc:
        logger.exception("Tool %s failed", name)
        return _ok(id, {
            "content": [{"type": "text", "text": json.dumps({"error": str(exc)})}],
            "isError": True,
        })


METHODS = {
    "initialize": handle_initialize,
    "notifications/initialized": None,  # notification, no response
    "tools/list": handle_tools_list,
    "tools/call": handle_tools_call,
}


# ---------------------------------------------------------------------------
# Main loop
# ---------------------------------------------------------------------------

def _cleanup() -> None:
    """Release all GPIO and PWM resources on exit."""
    for pin, obj in list(gpio._active_outputs.items()):
        try:
            if hasattr(obj, "close"):
                obj.close()
        except Exception:
            pass
    gpio._active_outputs.clear()

    for pin, obj in list(pwm._active_pwm.items()):
        try:
            if hasattr(obj, "close"):
                obj.close()
            elif hasattr(obj, "stop"):
                obj.stop()
        except Exception:
            pass
    pwm._active_pwm.clear()

    _executor.shutdown(wait=False)
    logger.info("pi-hardware MCP server cleaned up")


def run() -> None:
    """Read JSON-RPC from stdin, write responses to stdout."""
    logging.basicConfig(
        stream=sys.stderr,
        level=logging.INFO,
        format="%(asctime)s [%(name)s] %(levelname)s %(message)s",
    )

    # Register cleanup for graceful shutdown (SIGTERM from kernel supervisor).
    atexit.register(_cleanup)
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))

    logger.info("pi-hardware MCP server starting (tools=%d)", len(TOOLS))

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue

        try:
            request = json.loads(line)
        except json.JSONDecodeError as exc:
            resp = _error(None, -32700, f"Parse error: {exc}")
            sys.stdout.write(json.dumps(resp) + "\n")
            sys.stdout.flush()
            continue

        method = request.get("method", "")
        req_id = request.get("id")
        params = request.get("params", {})

        handler = METHODS.get(method)
        if handler is None:
            # Notifications (no id) are silently ignored if unhandled.
            if req_id is not None and method not in METHODS:
                resp = _error(req_id, -32601, f"Method not found: {method}")
                sys.stdout.write(json.dumps(resp) + "\n")
                sys.stdout.flush()
            continue

        resp = handler(req_id, params)
        sys.stdout.write(json.dumps(resp) + "\n")
        sys.stdout.flush()


def main() -> None:
    run()


if __name__ == "__main__":
    main()
