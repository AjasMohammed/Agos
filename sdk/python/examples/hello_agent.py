"""
AgentOS Python SDK — Hello World

Prerequisite: agentctl kernel start (in another terminal)

Install:
    pip install -e sdk/python

Run:
    python sdk/python/examples/hello_agent.py
"""
import asyncio
import agentos


async def main() -> None:
    async with await agentos.Agent.connect("my-first-agent") as agent:
        print(f"Connected as {agent.name} (id={agent.agent_id})")
        result = await agent.run("What is 2 + 2? Explain step by step.")
        print(result.output)


if __name__ == "__main__":
    asyncio.run(main())
