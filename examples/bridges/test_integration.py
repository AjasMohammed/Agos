"""
AgentOS Integration Tests — MCP + A2A Protocol

Verifies that external frameworks can connect to AgentOS and that
security enforcement works across both protocols.

Requirements:
  AgentOS must be running:
    agentos kernel start
    agentos mcp serve --transport http --port 3002 --token testtoken &

Usage:
  AGENTOS_TOKEN=testtoken pytest test_integration.py -v
"""

import asyncio
import os

import httpx
import pytest

AGENTOS_MCP_URL = os.getenv("AGENTOS_MCP_URL", "http://localhost:3002")
AGENTOS_A2A_URL = os.getenv("AGENTOS_A2A_URL", "http://localhost:3001")
AGENTOS_TOKEN = os.getenv("AGENTOS_TOKEN", "testtoken")


# ── MCP Protocol Tests ─────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_mcp_health_endpoint():
    """AgentOS MCP server responds to health checks."""
    async with httpx.AsyncClient() as client:
        resp = await client.get(f"{AGENTOS_MCP_URL}/mcp/health")
    assert resp.status_code == 200


@pytest.mark.asyncio
async def test_mcp_tools_list_returns_tools():
    """External client can discover AgentOS tools via MCP."""
    async with httpx.AsyncClient() as client:
        resp = await client.post(
            f"{AGENTOS_MCP_URL}/mcp",
            json={"jsonrpc": "2.0", "id": 1, "method": "tools/list"},
            headers={
                "Content-Type": "application/json",
                "Authorization": f"Bearer {AGENTOS_TOKEN}",
            },
        )
    assert resp.status_code == 200
    data = resp.json()
    assert "result" in data
    tools = data["result"]["tools"]
    assert isinstance(tools, list)
    assert len(tools) > 0
    tool_names = [t["name"] for t in tools]
    assert "file-reader" in tool_names  # core tool always present


@pytest.mark.asyncio
async def test_mcp_resources_list():
    """MCP resources endpoint returns agentos:// resources."""
    async with httpx.AsyncClient() as client:
        resp = await client.post(
            f"{AGENTOS_MCP_URL}/mcp",
            json={"jsonrpc": "2.0", "id": 2, "method": "resources/list"},
            headers={
                "Content-Type": "application/json",
                "Authorization": f"Bearer {AGENTOS_TOKEN}",
            },
        )
    assert resp.status_code == 200
    data = resp.json()
    assert "result" in data
    resources = data["result"]["resources"]
    uris = [r["uri"] for r in resources]
    assert "agentos://tools" in uris


@pytest.mark.asyncio
async def test_mcp_prompts_list():
    """MCP prompts endpoint returns available prompt templates."""
    async with httpx.AsyncClient() as client:
        resp = await client.post(
            f"{AGENTOS_MCP_URL}/mcp",
            json={"jsonrpc": "2.0", "id": 3, "method": "prompts/list"},
            headers={
                "Content-Type": "application/json",
                "Authorization": f"Bearer {AGENTOS_TOKEN}",
            },
        )
    assert resp.status_code == 200
    data = resp.json()
    assert "result" in data
    prompts = data["result"]["prompts"]
    assert isinstance(prompts, list)


@pytest.mark.asyncio
async def test_mcp_request_without_token_rejected():
    """MCP requests without a token are rejected when auth is configured."""
    async with httpx.AsyncClient() as client:
        resp = await client.post(
            f"{AGENTOS_MCP_URL}/mcp",
            json={"jsonrpc": "2.0", "id": 4, "method": "tools/list"},
            headers={"Content-Type": "application/json"},
            # Note: no Authorization header
        )
    # Should be 401 if auth is configured with a token
    # If running with --token, this must reject unauthenticated requests
    if AGENTOS_TOKEN:
        assert resp.status_code == 401


@pytest.mark.asyncio
async def test_mcp_initialize_advertises_all_capabilities():
    """MCP initialize response advertises tools, resources, prompts, sampling."""
    async with httpx.AsyncClient() as client:
        resp = await client.post(
            f"{AGENTOS_MCP_URL}/mcp",
            json={"jsonrpc": "2.0", "id": 5, "method": "initialize"},
            headers={
                "Content-Type": "application/json",
                "Authorization": f"Bearer {AGENTOS_TOKEN}",
            },
        )
    assert resp.status_code == 200
    caps = resp.json()["result"]["capabilities"]
    assert "tools" in caps
    assert "resources" in caps
    assert "prompts" in caps
    assert "sampling" in caps


# ── A2A Protocol Tests ─────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_a2a_agent_card_discoverable():
    """Agent Card is served at the well-known URL."""
    async with httpx.AsyncClient() as client:
        resp = await client.get(f"{AGENTOS_A2A_URL}/.well-known/agent.json")
    assert resp.status_code == 200
    card = resp.json()
    assert card["name"]
    assert card["provider"] == "agentos"
    assert "protocolVersion" in card


@pytest.mark.asyncio
async def test_a2a_task_submission_accepted():
    """A2A task submission returns 202 Accepted with a task ID."""
    async with httpx.AsyncClient() as client:
        capabilities_resp = await client.get(
            f"{AGENTOS_A2A_URL}/.well-known/agent.json"
        )
        card = capabilities_resp.json()
        caps = [c["name"] for c in card.get("capabilities", [])]

    if not caps:
        pytest.skip("No capabilities advertised — is AgentOS running?")

    async with httpx.AsyncClient() as client:
        resp = await client.post(
            f"{AGENTOS_A2A_URL}/a2a/tasks",
            json={
                "sender": "http://test-agent.local",
                "capability": caps[0],
                "input": {},
            },
        )
    assert resp.status_code == 202
    data = resp.json()
    assert "id" in data


@pytest.mark.asyncio
async def test_a2a_task_status_polled():
    """Submitted A2A task can be polled for status."""
    async with httpx.AsyncClient() as client:
        card = (
            await client.get(f"{AGENTOS_A2A_URL}/.well-known/agent.json")
        ).json()
        caps = [c["name"] for c in card.get("capabilities", [])]

    if not caps:
        pytest.skip("No capabilities advertised")

    async with httpx.AsyncClient() as client:
        submit = await client.post(
            f"{AGENTOS_A2A_URL}/a2a/tasks",
            json={
                "sender": "http://test-agent.local",
                "capability": caps[0],
                "input": {},
            },
        )
        task_id = submit.json()["id"]

        # Poll once — should return a valid task status
        poll = await client.get(f"{AGENTOS_A2A_URL}/a2a/tasks/{task_id}")
    assert poll.status_code == 200
    task = poll.json()
    assert "status" in task
    assert "state" in task["status"]


@pytest.mark.asyncio
async def test_a2a_unknown_task_returns_404():
    """Polling a non-existent task ID returns 404."""
    async with httpx.AsyncClient() as client:
        resp = await client.get(
            f"{AGENTOS_A2A_URL}/a2a/tasks/00000000-0000-0000-0000-000000000000"
        )
    assert resp.status_code == 404
