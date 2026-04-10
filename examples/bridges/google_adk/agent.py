"""
Google ADK → AgentOS A2A Bridge

Demonstrates an ADK agent discovering and delegating tasks to AgentOS
via the A2A (Agent-to-Agent) protocol.

Architecture:
  ADK Agent → A2A discovery → AgentOS agent card → Task delegation → A2A response

Prerequisites:
  pip install -r requirements.txt
  agentos mcp serve --transport http --port 3001 &  (A2A runs on same server)

Usage:
  python agent.py
"""

import asyncio
import os

import httpx


AGENTOS_URL = os.getenv("AGENTOS_URL", "http://localhost:3001")
AGENTOS_TOKEN = os.getenv("AGENTOS_TOKEN", "")


async def discover_agent(base_url: str) -> dict:
    """Fetch the A2A Agent Card from a remote agent."""
    async with httpx.AsyncClient() as client:
        resp = await client.get(f"{base_url}/.well-known/agent.json")
        resp.raise_for_status()
        return resp.json()


async def delegate_task(
    base_url: str,
    capability: str,
    input_data: dict,
    token: str = "",
) -> dict:
    """Submit a task and poll until completion."""
    headers = {"Content-Type": "application/json"}
    if token:
        headers["Authorization"] = f"Bearer {token}"

    async with httpx.AsyncClient() as client:
        # Submit task
        resp = await client.post(
            f"{base_url}/a2a/tasks",
            json={
                "sender": "http://google-adk-agent.local",
                "capability": capability,
                "input": input_data,
            },
            headers=headers,
        )
        resp.raise_for_status()
        task_id = resp.json()["id"]
        print(f"Task {task_id} submitted for capability '{capability}'")

        # Poll for result
        for attempt in range(30):
            await asyncio.sleep(1)
            poll_resp = await client.get(f"{base_url}/a2a/tasks/{task_id}")
            task = poll_resp.json()
            state = task.get("status", {}).get("state", "unknown")

            if state == "completed":
                return task["status"].get("output", {})
            elif state == "failed":
                raise RuntimeError(f"Task failed: {task['status'].get('error')}")
            elif state == "cancelled":
                raise RuntimeError("Task was cancelled")

            if attempt % 5 == 0:
                print(f"  Still {state}... (attempt {attempt + 1}/30)")

    raise TimeoutError("Task did not complete within 30 seconds")


async def main():
    print("=== Google ADK → AgentOS A2A Bridge Demo ===\n")

    # Step 1: Discover the AgentOS agent
    print(f"1. Discovering AgentOS agent at {AGENTOS_URL}...")
    card = await discover_agent(AGENTOS_URL)
    print(f"   Agent: {card['name']} (provider: {card['provider']})")
    print(f"   Protocol: {card['protocol_version']}")
    caps = [c["name"] for c in card.get("capabilities", [])]
    print(f"   Capabilities: {caps[:5]}{'...' if len(caps) > 5 else ''}\n")

    if not caps:
        print("No capabilities advertised. Is AgentOS running?")
        return

    # Step 2: Delegate a task using the first available capability
    first_cap = caps[0]
    print(f"2. Delegating task: capability='{first_cap}'...")

    try:
        result = await delegate_task(
            AGENTOS_URL,
            capability=first_cap,
            input_data={"path": "README.md"} if "file" in first_cap else {},
            token=AGENTOS_TOKEN,
        )
        print(f"   Result: {result}\n")
    except Exception as e:
        print(f"   Task failed: {e}\n")

    print("Demo complete.")
    print("All interactions were logged to the AgentOS audit trail.")
    print(f"Run 'agentos audit logs --last 10' to see them.")


if __name__ == "__main__":
    asyncio.run(main())
