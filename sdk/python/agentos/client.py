"""
BusClient — async Unix socket client for the AgentOS kernel.

Wire protocol (matches agentos-bus transport.rs):
  [4 bytes big-endian u32 length][JSON payload]

Every message is wrapped in a BusMessage envelope:
  Send:    {"Command": {"VariantName": {fields...}}}
  Receive: {"CommandResponse": {"VariantName": {fields...}}}
           or {"StatusUpdate": {...}} (pushed by kernel, ignored unless polled)
"""
from __future__ import annotations

import asyncio
import json
import struct
from pathlib import Path
from typing import Any

from .exceptions import KernelCommandError, KernelConnectionError

# 4-byte big-endian unsigned int — matches `u32::from_be_bytes` in Rust
_LENGTH_FORMAT = ">I"
_LENGTH_SIZE = struct.calcsize(_LENGTH_FORMAT)
# Matches MAX_MESSAGE_SIZE in transport.rs
_MAX_MESSAGE_SIZE = 16 * 1024 * 1024


class BusClient:
    """
    Async client for the AgentOS kernel Unix socket bus.

    Uses the same length-prefixed JSON protocol as the Rust CLI client.
    Messages are wrapped in BusMessage envelopes that match the serde
    serialization produced by agentos-bus.
    """

    DEFAULT_SOCKET: Path = Path("/tmp/agentos.sock")

    def __init__(self, socket_path: Path = DEFAULT_SOCKET) -> None:
        self._socket_path = socket_path
        self._reader: asyncio.StreamReader | None = None
        self._writer: asyncio.StreamWriter | None = None

    # ------------------------------------------------------------------
    # Connection lifecycle
    # ------------------------------------------------------------------

    async def connect(self) -> None:
        """Open connection to the kernel socket."""
        try:
            self._reader, self._writer = await asyncio.open_unix_connection(
                str(self._socket_path)
            )
        except (FileNotFoundError, ConnectionRefusedError, OSError) as exc:
            raise KernelConnectionError(
                f"Cannot connect to kernel at {self._socket_path}: {exc}. "
                "Is the kernel running? (agentctl kernel start)"
            ) from exc

    async def close(self) -> None:
        """Close the connection gracefully."""
        if self._writer is not None:
            try:
                self._writer.close()
                await self._writer.wait_closed()
            except Exception:  # noqa: BLE001
                pass
            finally:
                self._writer = None
                self._reader = None

    async def __aenter__(self) -> "BusClient":
        await self.connect()
        return self

    async def __aexit__(self, *_: Any) -> None:
        await self.close()

    # ------------------------------------------------------------------
    # Low-level framing
    # ------------------------------------------------------------------

    def _require_connected(self) -> None:
        if self._reader is None or self._writer is None:
            raise KernelConnectionError("BusClient is not connected. Call connect() first.")

    async def _write_framed(self, payload: bytes) -> None:
        """Write a length-prefixed frame to the socket."""
        self._require_connected()
        if len(payload) > _MAX_MESSAGE_SIZE:
            raise KernelCommandError(
                f"Message too large: {len(payload)} bytes (max {_MAX_MESSAGE_SIZE})"
            )
        length_prefix = struct.pack(_LENGTH_FORMAT, len(payload))
        self._writer.write(length_prefix + payload)  # type: ignore[union-attr]
        await self._writer.drain()  # type: ignore[union-attr]

    async def _read_framed(self) -> bytes:
        """Read a length-prefixed frame from the socket."""
        self._require_connected()
        len_buf = await self._reader.readexactly(_LENGTH_SIZE)  # type: ignore[union-attr]
        (length,) = struct.unpack(_LENGTH_FORMAT, len_buf)
        if length == 0:
            raise KernelCommandError("Empty message received (length 0)")
        if length > _MAX_MESSAGE_SIZE:
            raise KernelCommandError(
                f"Message too large: {length} bytes (max {_MAX_MESSAGE_SIZE})"
            )
        return await self._reader.readexactly(length)  # type: ignore[union-attr]

    # ------------------------------------------------------------------
    # Message send/receive
    # ------------------------------------------------------------------

    # Maximum number of status pushes to skip before giving up.
    _MAX_STATUS_SKIPS = 200

    async def send_raw(self, envelope: dict[str, Any]) -> dict[str, Any]:
        """
        Send a BusMessage envelope and return the response envelope.

        Skips StatusUpdate/NotificationPush messages transparently, up to
        _MAX_STATUS_SKIPS times, then raises KernelCommandError.
        """
        payload = json.dumps(envelope).encode()
        await self._write_framed(payload)

        for _ in range(self._MAX_STATUS_SKIPS + 1):
            raw = await self._read_framed()
            response = json.loads(raw)
            # Skip async kernel broadcasts; wait for the actual CommandResponse
            if "StatusUpdate" in response or "NotificationPush" in response:
                continue
            return response

        raise KernelCommandError(
            f"Received more than {self._MAX_STATUS_SKIPS} status updates "
            "without a CommandResponse — possible protocol error"
        )

    async def send_command(self, command_variant: str, **fields: Any) -> dict[str, Any]:
        """
        Send a KernelCommand and return the unwrapped KernelResponse dict.

        Raises KernelCommandError if the kernel returns an Error response.

        Args:
            command_variant: The KernelCommand variant name (e.g. "ConnectAgent").
            **fields: Fields for the command variant.

        Returns:
            The inner dict from the KernelResponse variant, or {} for unit variants.
        """
        # Build: {"Command": {"ConnectAgent": {fields...}}} or {"Command": "ListAgents"}
        if fields:
            command_body: Any = {command_variant: fields}
        else:
            command_body = command_variant

        envelope = {"Command": command_body}
        response_envelope = await self.send_raw(envelope)

        # Unwrap: {"CommandResponse": inner}
        if "CommandResponse" not in response_envelope:
            raise KernelCommandError(
                f"Unexpected response envelope: {response_envelope}"
            )
        response = response_envelope["CommandResponse"]

        # Handle Error variant
        if isinstance(response, dict) and "Error" in response:
            raise KernelCommandError(response["Error"].get("message", str(response)))

        return response
