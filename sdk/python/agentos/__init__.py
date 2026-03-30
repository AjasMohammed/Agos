"""
AgentOS Python SDK

Connect agents, run tasks, define tools, and receive notifications —
without writing a single line of Rust.

Quick start:
    import asyncio
    import agentos

    async def main():
        async with await agentos.Agent.connect("my-agent") as agent:
            result = await agent.run("What is 2 + 2?")
            print(result.output)

    asyncio.run(main())
"""

from .agent import Agent
from .client import BusClient
from .exceptions import (
    AgentOSError,
    KernelCommandError,
    KernelConnectionError,
    NotificationError,
    TaskError,
    TaskTimeoutError,
    ToolError,
)
from .notification import Inbox, Message
from .task import TaskResult
from .testing import MockKernel
from .tool import tool
from .types import LLMProvider, TaskState

__all__ = [
    # Core classes
    "Agent",
    "BusClient",
    "TaskResult",
    "Inbox",
    "Message",
    # Decorators
    "tool",
    # Enums / types
    "LLMProvider",
    "TaskState",
    # Exceptions
    "AgentOSError",
    "KernelCommandError",
    "KernelConnectionError",
    "TaskError",
    "TaskTimeoutError",
    "ToolError",
    "NotificationError",
    # Testing
    "MockKernel",
]

__version__ = "0.1.0"
