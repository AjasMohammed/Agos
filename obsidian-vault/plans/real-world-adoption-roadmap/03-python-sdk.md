---
title: Python SDK
tags:
  - python
  - sdk
  - adoption
  - plan
  - v3
date: 2026-03-25
status: complete
effort: 8d
priority: high
---

# Phase 3 — Python SDK

> Build a Python SDK that speaks the AgentOS bus protocol, allowing Python developers to connect agents, run tasks, define tools, and receive notifications — without writing a single line of Rust. This unlocks 90% of the agent developer market.

---

## Why This Phase

The ecosystem research is unambiguous: Python dominates the AI agent developer ecosystem. AutoGen, CrewAI, LangGraph, PydanticAI, and OpenAI Agents SDK are all Python-first. An AgentOS that requires Rust development has an adoption ceiling.

The key insight: **the SDK does not need to reimplement the kernel.** The kernel runs as a local process. The Python SDK is a thin client that speaks the existing Unix socket bus protocol. All security, memory, cost tracking, and tool execution remain in the kernel. The SDK is purely a developer experience layer.

---

## Current → Target State

| Area | Current | Target |
|------|---------|--------|
| Language support | Rust only | Rust + Python |
| Agent development | Implement `AgentTool` trait in Rust | `@agentos.tool` decorator in Python |
| Task submission | `agentctl task run` CLI | `agent.run("do X")` in Python |
| Result handling | Parse CLI JSON output | `result = await agent.run(...)` |
| Tool development | Write TOML manifest + Rust impl | Python function + decorator auto-generates manifest |
| Notifications | CLI inbox polling | `async for msg in agent.inbox(): ...` |
| Test/mock | MockLLMCore in Rust | `agentos.MockKernel()` for unit testing |

---

## Architecture

```
Python Script
     │
     │  import agentos
     │
     ▼
┌─────────────────────────────────────────────────────┐
│  agentos Python SDK (pip install agentos-sdk)       │
│                                                     │
│  BusClient         ← async Unix socket client       │
│  Agent             ← connect, run tasks, get perms  │
│  Tool              ← @agentos.tool decorator        │
│  Notification      ← inbox, ask-user, notify        │
│  MockKernel        ← test doubles                   │
└─────────────────────┬───────────────────────────────┘
                      │ Unix socket (same protocol as agentctl)
                      ▼
          ┌─────────────────────┐
          │  AgentOS Kernel     │
          │  (Rust binary)      │
          └─────────────────────┘
```

The Python SDK serializes `KernelCommand` structs to JSON (matching the existing `serde_json` serialization used by the CLI) and sends them over the Unix socket. Responses are deserialized from JSON.

---

## Detailed Subtasks

### Subtask 3.1 — Python package scaffold

**Directory:** `sdk/python/` (new directory at repo root)

```
sdk/python/
├── pyproject.toml          # PEP 517 build config (hatchling)
├── README.md
├── agentos/
│   ├── __init__.py         # Public API re-exports
│   ├── client.py           # BusClient (Unix socket JSON-RPC)
│   ├── agent.py            # Agent class (connect, run, list tasks)
│   ├── task.py             # Task, TaskResult, TaskStatus
│   ├── tool.py             # @tool decorator, ToolManifest builder
│   ├── notification.py     # Inbox, Message, AskUserReply
│   ├── types.py            # Python dataclasses matching Rust types
│   ├── exceptions.py       # AgentOSError hierarchy
│   └── testing/
│       ├── __init__.py
│       └── mock_kernel.py  # MockKernel for unit tests
└── tests/
    ├── test_client.py
    ├── test_agent.py
    ├── test_tool.py
    └── test_notifications.py
```

**pyproject.toml:**
```toml
[build-system]
requires = ["hatchling"]
build-backend = "hatchling.build"

[project]
name = "agentos-sdk"
version = "0.1.0"
requires-python = ">=3.10"
dependencies = [
    "anyio>=4.0",       # async I/O across asyncio/trio
    "pydantic>=2.0",    # type validation (matches serde conventions)
    "rich>=13.0",       # terminal output for CLI helpers
]

[project.optional-dependencies]
dev = ["pytest", "pytest-asyncio", "httpx"]
```

---

### Subtask 3.2 — BusClient: async Unix socket client

**File:** `sdk/python/agentos/client.py`

```python
import asyncio
import json
from pathlib import Path
from typing import Any

class BusClient:
    """Async client for the AgentOS kernel Unix socket bus."""

    DEFAULT_SOCKET = Path("/tmp/agentos.sock")

    def __init__(self, socket_path: Path = DEFAULT_SOCKET):
        self._socket_path = socket_path
        self._reader: asyncio.StreamReader | None = None
        self._writer: asyncio.StreamWriter | None = None

    async def connect(self) -> None:
        self._reader, self._writer = await asyncio.open_unix_connection(
            str(self._socket_path)
        )

    async def close(self) -> None:
        if self._writer:
            self._writer.close()
            await self._writer.wait_closed()

    async def __aenter__(self):
        await self.connect()
        return self

    async def __aexit__(self, *args):
        await self.close()

    async def send(self, command: dict) -> dict:
        """Send a KernelCommand and receive a KernelResponse."""
        payload = json.dumps(command) + "\n"
        self._writer.write(payload.encode())
        await self._writer.drain()
        line = await self._reader.readline()
        return json.loads(line)

    async def send_command(self, command_type: str, **kwargs) -> Any:
        cmd = {"type": command_type, **kwargs}
        resp = await self.send(cmd)
        if resp.get("type") == "Error":
            raise AgentOSError(resp.get("message", "Unknown error"))
        return resp
```

---

### Subtask 3.3 — Agent class

**File:** `sdk/python/agentos/agent.py`

```python
from dataclasses import dataclass
from typing import AsyncIterator
from .client import BusClient
from .task import Task, TaskResult
from .notification import Inbox

@dataclass
class AgentProfile:
    name: str
    model: str = "claude-sonnet-4-6"
    provider: str = "anthropic"
    max_budget_usd: float | None = None

class Agent:
    """Represents a connected AgentOS agent."""

    def __init__(self, name: str, client: BusClient):
        self._name = name
        self._client = client
        self._agent_id: str | None = None

    @classmethod
    async def connect(
        cls,
        name: str,
        *,
        model: str = "claude-sonnet-4-6",
        socket_path: Path | None = None,
    ) -> "Agent":
        """Connect to kernel and register/reuse an agent by name."""
        client = BusClient(socket_path or BusClient.DEFAULT_SOCKET)
        await client.connect()
        agent = cls(name, client)
        resp = await client.send_command(
            "AgentConnect",
            name=name,
            model=model,
            provider="anthropic",
        )
        agent._agent_id = resp["agent_id"]
        return agent

    async def run(
        self,
        prompt: str,
        *,
        tools: list[str] | None = None,
        timeout_secs: int = 300,
        stream: bool = False,
    ) -> TaskResult:
        """Submit a task and wait for completion."""
        resp = await self._client.send_command(
            "TaskRun",
            agent_id=self._agent_id,
            prompt=prompt,
            tool_names=tools or [],
        )
        task = Task(resp["task_id"], self._client)
        return await task.wait(timeout_secs=timeout_secs)

    async def run_background(self, prompt: str, **kwargs) -> Task:
        """Submit a task and return immediately without waiting."""
        resp = await self._client.send_command(
            "TaskRun",
            agent_id=self._agent_id,
            prompt=prompt,
        )
        return Task(resp["task_id"], self._client)

    @property
    def inbox(self) -> "Inbox":
        return Inbox(self._agent_id, self._client)

    async def grant_permission(self, resource: str, ops: str) -> None:
        await self._client.send_command(
            "PermGrant",
            agent_id=self._agent_id,
            resource=resource,
            operations=ops,
        )

    async def close(self) -> None:
        await self._client.close()

    async def __aenter__(self): return self
    async def __aexit__(self, *args): await self.close()
```

---

### Subtask 3.4 — @tool decorator

**File:** `sdk/python/agentos/tool.py`

The decorator auto-generates a TOML manifest and registers the tool with the kernel at import time:

```python
import inspect
import json
from functools import wraps
from typing import Callable, Any
import pydantic

def tool(
    name: str,
    description: str,
    permissions: list[str] = (),
    trust_tier: str = "community",
):
    """
    Decorator that registers a Python function as an AgentOS tool.

    Example:
        @agentos.tool(
            name="summarize-text",
            description="Summarize a block of text",
            permissions=["memory.read:r"],
        )
        async def summarize(text: str, max_words: int = 100) -> str:
            return text[:max_words * 5]  # placeholder
    """
    def decorator(fn: Callable) -> Callable:
        # Infer JSON Schema from type hints using pydantic
        sig = inspect.signature(fn)
        input_schema = _sig_to_json_schema(sig)

        manifest = {
            "name": name,
            "description": description,
            "version": "1.0.0",
            "trust_tier": trust_tier,
            "permissions": list(permissions),
            "input_schema": input_schema,
        }

        # Register with kernel via BusClient when an agent is active
        fn._agentos_manifest = manifest
        fn._agentos_tool = True

        @wraps(fn)
        async def wrapper(*args, **kwargs):
            return await fn(*args, **kwargs)

        return wrapper
    return decorator

def _sig_to_json_schema(sig: inspect.Signature) -> dict:
    """Convert a Python function signature to JSON Schema."""
    properties = {}
    required = []
    for name, param in sig.parameters.items():
        if name in ("self", "cls"):
            continue
        ann = param.annotation
        prop = _annotation_to_schema(ann)
        properties[name] = prop
        if param.default is inspect.Parameter.empty:
            required.append(name)
    return {
        "type": "object",
        "properties": properties,
        "required": required,
    }
```

---

### Subtask 3.5 — Notification inbox

**File:** `sdk/python/agentos/notification.py`

```python
from dataclasses import dataclass
from typing import AsyncIterator
import asyncio

@dataclass
class Message:
    id: str
    kind: str       # "info" | "ask" | "alert"
    body: str
    from_agent: str
    timestamp: str
    read: bool

class Inbox:
    def __init__(self, agent_id: str, client):
        self._agent_id = agent_id
        self._client = client

    async def messages(self, unread_only: bool = True) -> list[Message]:
        resp = await self._client.send_command(
            "NotificationList",
            agent_id=self._agent_id,
            unread_only=unread_only,
        )
        return [Message(**m) for m in resp["messages"]]

    async def respond(self, message_id: str, text: str) -> None:
        """Reply to an ask-user prompt, unblocking the waiting task."""
        await self._client.send_command(
            "NotificationRespond",
            message_id=message_id,
            response=text,
        )

    async def mark_read(self, message_id: str) -> None:
        await self._client.send_command(
            "NotificationMarkRead",
            message_id=message_id,
        )

    async def __aiter__(self) -> AsyncIterator[Message]:
        """Poll for new messages every 2 seconds."""
        seen = set()
        while True:
            msgs = await self.messages(unread_only=True)
            for msg in msgs:
                if msg.id not in seen:
                    seen.add(msg.id)
                    yield msg
            await asyncio.sleep(2)
```

---

### Subtask 3.6 — MockKernel for testing

**File:** `sdk/python/agentos/testing/mock_kernel.py`

```python
class MockKernel:
    """
    In-process mock kernel for unit testing Python tools and agents.
    Does not require a running Rust kernel.

    Usage:
        async with MockKernel() as kernel:
            kernel.add_tool_response("file-reader", {"content": "hello"})
            agent = await kernel.connect_agent("test-agent")
            result = await agent.run("read the file")
            assert result.success
    """

    def __init__(self):
        self._tool_responses: dict[str, Any] = {}
        self._tasks: list[dict] = []

    def add_tool_response(self, tool_name: str, response: Any) -> None:
        self._tool_responses[tool_name] = response

    def set_llm_response(self, response: str) -> None:
        self._llm_response = response

    async def connect_agent(self, name: str, **kwargs) -> Agent:
        # Returns an Agent backed by MockBusClient
        ...

    async def __aenter__(self): return self
    async def __aexit__(self, *args): pass
```

---

### Subtask 3.7 — Hello-world example and quickstart docs

**File:** `sdk/python/examples/hello_agent.py`

```python
"""
AgentOS Python SDK — Hello World

Prerequisite: agentctl web serve (or agentctl kernel start)

pip install agentos-sdk
"""
import asyncio
import agentos

async def main():
    async with await agentos.Agent.connect("my-first-agent") as agent:
        result = await agent.run("What is 2 + 2? Explain step by step.")
        print(result.output)

asyncio.run(main())
```

**File:** `sdk/python/examples/custom_tool.py`

```python
import agentos

@agentos.tool(
    name="word-count",
    description="Count the number of words in a text string",
    permissions=[],
)
async def word_count(text: str) -> dict:
    words = text.split()
    return {"count": len(words), "text_preview": text[:50]}

async def main():
    async with await agentos.Agent.connect("word-count-agent") as agent:
        # Register custom tool with kernel
        await agent.register_tool(word_count)
        result = await agent.run("Count the words in: 'Hello world this is AgentOS'")
        print(result.output)
```

---

### Subtask 3.8 — Bus protocol compatibility: JSON schema sync

The Python types must exactly match the Rust `serde_json` serialization. Add a build step that generates Python type stubs from Rust:

**File:** `sdk/python/scripts/gen_types.py`

This script reads the `KernelCommand` and `KernelResponse` enum variants from `crates/agentos-bus/src/message.rs` (simple regex parse) and emits a `agentos/types_generated.py` with matching dataclasses. Run as part of CI to catch drift.

```bash
# Run from repo root to regenerate Python types
python sdk/python/scripts/gen_types.py
```

---

## Files Changed

| File | Change |
|------|--------|
| `sdk/python/` | New directory — entire Python SDK |
| `sdk/python/agentos/client.py` | New — BusClient (Unix socket) |
| `sdk/python/agentos/agent.py` | New — Agent class |
| `sdk/python/agentos/task.py` | New — Task, TaskResult |
| `sdk/python/agentos/tool.py` | New — @tool decorator |
| `sdk/python/agentos/notification.py` | New — Inbox, Message |
| `sdk/python/agentos/testing/mock_kernel.py` | New — MockKernel |
| `sdk/python/examples/` | New — hello_agent.py, custom_tool.py |
| `sdk/python/pyproject.toml` | New — package config |
| `sdk/python/scripts/gen_types.py` | New — type sync from Rust |

No Rust files are changed. The SDK is purely additive.

---

## Dependencies

- No other phases required
- Requires kernel to be running (standard prerequisite)

---

## Test Plan

1. **connect and run** — `MockKernel`, connect agent, run "hello world" task, assert result
2. **@tool decorator** — decorate a function, check `fn._agentos_manifest` has correct JSON schema
3. **notification inbox poll** — mock kernel emits 3 messages, iterate `async for msg in agent.inbox()`, assert 3 received
4. **ask-user respond** — mock kernel emits an `ask` message, call `inbox.respond("yes")`, assert kernel receives `NotificationRespond` command
5. **type sync** — run `gen_types.py`, assert no diff in generated file (CI regression guard)
6. **hello_agent example** — with a real running kernel, execute `examples/hello_agent.py`, assert non-empty result output

---

## Verification

```bash
cd sdk/python
pip install -e ".[dev]"
pytest tests/ -v

# Quickstart smoke test (requires running kernel)
python examples/hello_agent.py
```

---

## Related

- [[Real World Adoption Roadmap Plan]]
- [[04-agent-marketplace]] — marketplace tool install will need Python manifest support
