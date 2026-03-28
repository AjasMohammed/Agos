"""
Tests for BusClient — wire protocol framing.

Uses socket pairs (Python 3.10 compatible) to test the 4-byte
length-prefix protocol without a real Unix domain socket.
"""
from __future__ import annotations

import asyncio
import json
import socket
import struct

import pytest

from agentos.client import BusClient, _LENGTH_FORMAT, _LENGTH_SIZE
from agentos.exceptions import KernelCommandError, KernelConnectionError


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _frame(payload: dict) -> bytes:
    """Build a length-prefixed JSON frame (server → client)."""
    data = json.dumps(payload).encode()
    return struct.pack(_LENGTH_FORMAT, len(data)) + data


async def _make_stream_pair():
    """
    Create two connected asyncio stream pairs via socket.socketpair().

    Returns (client_reader, client_writer, server_reader, server_writer).
    Python 3.10 compatible — asyncio.create_pipe() is 3.12+.
    """
    sock_a, sock_b = socket.socketpair()
    sock_a.setblocking(False)
    sock_b.setblocking(False)
    client_reader, client_writer = await asyncio.open_connection(sock=sock_a)
    server_reader, server_writer = await asyncio.open_connection(sock=sock_b)
    return client_reader, client_writer, server_reader, server_writer


async def _make_client() -> tuple[BusClient, asyncio.StreamReader, asyncio.StreamWriter]:
    """Return a BusClient wired up to an in-memory socket pair."""
    client_r, client_w, server_r, server_w = await _make_stream_pair()
    client = BusClient.__new__(BusClient)
    client._socket_path = None  # type: ignore[assignment]
    client._reader = client_r
    client._writer = client_w
    return client, server_r, server_w


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------

class TestFraming:
    async def test_write_and_read_roundtrip(self):
        """BusClient encodes messages with a correct 4-byte length prefix."""
        client, server_r, server_w = await _make_client()

        payload = {"Command": "ListAgents"}
        await client._write_framed(json.dumps(payload).encode())

        # Read back the frame from the server side
        len_buf = await server_r.readexactly(_LENGTH_SIZE)
        (length,) = struct.unpack(_LENGTH_FORMAT, len_buf)
        body = await server_r.readexactly(length)
        assert json.loads(body) == payload

    async def test_send_command_parses_success(self):
        """send_command() returns the inner KernelResponse dict on success."""
        client, _server_r, server_w = await _make_client()

        response_envelope = {
            "CommandResponse": {"Success": {"data": {"agent_id": "abc-123"}}}
        }
        server_w.write(_frame(response_envelope))
        await server_w.drain()

        result = await client.send_command(
            "ConnectAgent", name="test", provider="Anthropic",
            model="claude-sonnet-4-6", base_url=None,
            roles=[], test_mode=False, extra_permissions=[],
        )
        assert result == {"Success": {"data": {"agent_id": "abc-123"}}}

    async def test_send_command_raises_on_error(self):
        """send_command() raises KernelCommandError on Error response."""
        client, _server_r, server_w = await _make_client()

        error_envelope = {
            "CommandResponse": {"Error": {"message": "Agent not found"}}
        }
        server_w.write(_frame(error_envelope))
        await server_w.drain()

        with pytest.raises(KernelCommandError, match="Agent not found"):
            await client.send_command("RunTask", agent_name="ghost", prompt="hi",
                                      autonomous=False)

    async def test_status_update_skipped(self):
        """BusClient transparently skips StatusUpdate pushes."""
        client, _server_r, server_w = await _make_client()

        status_push = {"StatusUpdate": {"task_id": "x", "state": "Running", "message": "..."}}
        success = {"CommandResponse": {"Success": {"data": {"result": "done"}}}}

        server_w.write(_frame(status_push))
        server_w.write(_frame(success))
        await server_w.drain()

        result = await client.send_command("RunTask", agent_name="a", prompt="p",
                                           autonomous=False)
        assert "Success" in result

    async def test_connect_raises_when_socket_missing(self, tmp_path):
        """connect() raises KernelConnectionError when the socket file is absent."""
        client = BusClient(tmp_path / "nonexistent.sock")
        with pytest.raises(KernelConnectionError):
            await client.connect()
