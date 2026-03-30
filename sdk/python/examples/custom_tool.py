"""
AgentOS Python SDK — Custom Tool Example

Demonstrates how to define a Python tool with @agentos.tool
and register it with a running kernel agent.

Run:
    python sdk/python/examples/custom_tool.py
"""
import asyncio
import agentos


@agentos.tool(
    name="word-count",
    description="Count the number of words in a text string",
    permissions=[],
)
async def word_count(text: str) -> dict:
    """Return word count and a preview of the text."""
    words = text.split()
    return {"count": len(words), "text_preview": text[:50]}


async def main() -> None:
    async with await agentos.Agent.connect("word-count-agent") as agent:
        # Register the custom tool with the kernel
        await agent.register_tool(word_count)
        print("Tool registered.")

        result = await agent.run(
            "Count the words in: 'Hello world this is AgentOS'"
        )
        print(result.output)


if __name__ == "__main__":
    asyncio.run(main())
