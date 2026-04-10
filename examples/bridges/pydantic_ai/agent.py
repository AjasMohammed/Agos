"""
PydanticAI → AgentOS Bridge (MCP + A2A)

Demonstrates two integration modes:
  1. MCP mode: PydanticAI agent uses AgentOS tools via MCP
  2. A2A mode: PydanticAI agent delegates tasks to AgentOS via A2A

Prerequisites:
  pip install -r requirements.txt
  agentos mcp serve --transport http --port 3002 --token your-token &

Usage:
  AGENTOS_TOKEN=your-token python agent.py --mode mcp
  AGENTOS_TOKEN=your-token python agent.py --mode a2a
"""

import argparse
import asyncio
import os

import httpx
from pydantic_ai import Agent
from pydantic_ai.mcp import MCPServerHTTP


AGENTOS_URL = os.getenv("AGENTOS_URL", "http://localhost:3002")
AGENTOS_TOKEN = os.getenv("AGENTOS_TOKEN", "")


async def run_mcp_mode():
    """Use AgentOS tools directly via MCP."""
    headers = {}
    if AGENTOS_TOKEN:
        headers["Authorization"] = f"Bearer {AGENTOS_TOKEN}"

    agentos_server = MCPServerHTTP(
        url=f"{AGENTOS_URL}/mcp",
        headers=headers,
    )

    agent = Agent(
        "anthropic:claude-3-5-sonnet-20241022",
        mcp_servers=[agentos_server],
        system_prompt=(
            "You have secure file and memory access via AgentOS. "
            "All your tool calls are validated by AgentOS's CapabilityToken system."
        ),
    )

    async with agent.run_mcp_servers():
        result = await agent.run(
            "List the available tools, then read any README file you find."
        )
    print(result.data)


async def run_a2a_mode():
    """Delegate a task to AgentOS via A2A protocol."""
    # Fetch the AgentOS agent card
    async with httpx.AsyncClient() as client:
        resp = await client.get(f"{AGENTOS_URL}/.well-known/agent.json")
        resp.raise_for_status()
        card = resp.json()

    print(f"Discovered agent: {card['name']}")
    print(f"Capabilities: {[c['name'] for c in card.get('capabilities', [])]}")

    # Submit a task delegation
    headers = {"Content-Type": "application/json"}
    if AGENTOS_TOKEN:
        headers["Authorization"] = f"Bearer {AGENTOS_TOKEN}"

    async with httpx.AsyncClient() as client:
        resp = await client.post(
            f"{AGENTOS_URL}/a2a/tasks",
            json={
                "sender": "http://pydantic-ai-agent.local",
                "capability": "file-reader",
                "input": {"path": "README.md"},
            },
            headers=headers,
        )
        resp.raise_for_status()
        task = resp.json()
        task_id = task["id"]
        print(f"Task submitted: {task_id}")

        # Poll for completion
        for _ in range(30):
            await asyncio.sleep(1)
            resp = await client.get(f"{AGENTOS_URL}/a2a/tasks/{task_id}")
            task = resp.json()
            state = task.get("status", {}).get("state", "unknown")
            print(f"Status: {state}")
            if state == "completed":
                print("Output:", task["status"].get("output"))
                return
            elif state == "failed":
                print("Error:", task["status"].get("error"))
                return

    print("Timed out waiting for task")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--mode", choices=["mcp", "a2a"], default="mcp")
    args = parser.parse_args()

    if args.mode == "mcp":
        asyncio.run(run_mcp_mode())
    else:
        asyncio.run(run_a2a_mode())
