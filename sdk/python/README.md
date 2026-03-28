# AgentOS Python SDK

Connect agents, run tasks, define tools, and receive notifications — without writing a single line of Rust.

## Quick start

```python
import asyncio
import agentos

async def main():
    async with await agentos.Agent.connect("my-agent") as agent:
        result = await agent.run("What is 2 + 2?")
        print(result.output)

asyncio.run(main())
```

## Installation

```bash
pip install agentos-sdk
```

Requires a running AgentOS kernel: `agentctl kernel start`
