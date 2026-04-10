"""
LangGraph → AgentOS Bridge Example

This example shows a LangGraph ReAct agent using AgentOS tools via MCP.
AgentOS enforces CapabilityToken authorization on every tool call and logs
all tool invocations to its tamper-evident audit trail.

Architecture:
  LangGraph agent → MCP client → AgentOS MCP server → CapToken check → Tool

Prerequisites:
  pip install -r requirements.txt
  agentos mcp serve --transport http --port 3002 --token your-token &

Usage:
  AGENTOS_TOKEN=your-token python agent.py
"""

import asyncio
import os

from langchain_anthropic import ChatAnthropic
from langchain_mcp_adapters.client import MultiServerMCPClient
from langgraph.prebuilt import create_react_agent


AGENTOS_URL = os.getenv("AGENTOS_URL", "http://localhost:3002/mcp")
AGENTOS_TOKEN = os.getenv("AGENTOS_TOKEN", "")


async def main():
    # Connect to AgentOS MCP server.
    # CapabilityToken is passed as a Bearer token — validated on every call.
    async with MultiServerMCPClient(
        {
            "agentos": {
                "url": AGENTOS_URL,
                "transport": "streamable_http",
                "headers": {
                    "Authorization": f"Bearer {AGENTOS_TOKEN}"
                } if AGENTOS_TOKEN else {},
            }
        }
    ) as mcp_client:
        # Discover all tools exposed by AgentOS
        tools = mcp_client.get_tools()
        print(f"AgentOS tools available: {[t.name for t in tools]}")

        model = ChatAnthropic(model="claude-3-5-sonnet-20241022")
        agent = create_react_agent(model, tools)

        # Run the agent with a goal
        result = await agent.ainvoke(
            {
                "messages": [
                    (
                        "user",
                        "List the files in the workspace and summarize what you find.",
                    )
                ]
            }
        )

        # Print the final response
        for msg in result["messages"]:
            if hasattr(msg, "content") and msg.content:
                print(f"\n[{msg.__class__.__name__}] {msg.content}")


if __name__ == "__main__":
    asyncio.run(main())
