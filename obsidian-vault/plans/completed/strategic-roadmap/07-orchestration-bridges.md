---
title: "Phase 7: Orchestration Bridges"
tags:
  - strategy
  - integration
  - orchestration
  - phase-7
date: 2026-04-08
status: planned
effort: 1w
priority: medium
---

# Phase 7: Orchestration Bridges

> Build thin protocol adapters that allow high-level orchestrators (LangGraph, CrewAI, PydanticAI, Google ADK) to route agent tasks through AgentOS for secure execution. Prove the "frameworks build agents, AgentOS runs them safely" thesis.

---

## Why This Phase

Research finding: The strategic thesis is "don't compete with frameworks — be their runtime." MCP and A2A (Phases 2-3) provide the protocol foundation. This phase builds the **concrete adapters** that demonstrate the integration and produces reference architectures that orchestrator teams can adopt.

The key insight from research: bridges should be **thin protocol adapters**, not deep integrations. If the MCP and A2A implementations are solid, the bridges are almost trivial — they translate framework-specific requests into MCP tool calls or A2A task delegations.

**Why this is last:** It depends on stable MCP (Phase 2), A2A (Phase 3), and has the lowest independent value — it's a demonstration of the platform, not a platform feature itself.

---

## Current → Target State

**Current:** AgentOS agents communicate internally via `KernelCommand` dispatch and externally via REST API (50 endpoints). No framework-specific adapters. No reference architectures.

**Target:** Working examples of LangGraph, CrewAI, and PydanticAI routing tasks through AgentOS via MCP. A2A-based agent collaboration with Google ADK. Reference architecture documents.

---

## Detailed Subtasks

### 1. LangGraph → AgentOS Bridge

LangGraph agents can use MCP tools. The bridge is an MCP server configuration:

```python
# examples/bridges/langgraph/agent.py
from langgraph.prebuilt import create_react_agent
from langchain_mcp_adapters.client import MultiServerMCPClient

# Connect to AgentOS MCP server
async with MultiServerMCPClient(
    {
        "agentos": {
            "url": "http://localhost:3002/mcp",  # Streamable HTTP
            "transport": "streamable_http",
            "headers": {"Authorization": "Bearer <CAPABILITY_TOKEN>"}
        }
    }
) as mcp_client:
    tools = mcp_client.get_tools()
    # LangGraph agent now has access to all AgentOS tools
    # with CapabilityToken enforcement on every call
    agent = create_react_agent(model, tools)
    result = await agent.ainvoke({"messages": [("user", "Research and save findings")]})
```

**Files:**
- `examples/bridges/langgraph/agent.py` — working example
- `examples/bridges/langgraph/requirements.txt` — dependencies
- `examples/bridges/langgraph/README.md` — setup instructions

### 2. CrewAI → AgentOS Bridge

CrewAI supports MCP tool integration:

```python
# examples/bridges/crewai/crew.py
from crewai import Agent, Task, Crew

# CrewAI agent using AgentOS tools via MCP
researcher = Agent(
    role="Security Researcher",
    goal="Analyze threat landscapes",
    tools_config={
        "mcp_servers": [{
            "name": "agentos",
            "url": "http://localhost:3002/mcp",
            "token": "<CAPABILITY_TOKEN>"
        }]
    }
)

task = Task(
    description="Research latest CVEs and save findings",
    agent=researcher
)

crew = Crew(agents=[researcher], tasks=[task])
result = crew.kickoff()
```

**Files:**
- `examples/bridges/crewai/crew.py`
- `examples/bridges/crewai/requirements.txt`
- `examples/bridges/crewai/README.md`

### 3. PydanticAI → AgentOS Bridge

PydanticAI supports both MCP and A2A:

```python
# examples/bridges/pydantic_ai/agent.py
from pydantic_ai import Agent
from pydantic_ai.mcp import MCPServerHTTP

agentos_server = MCPServerHTTP(
    url="http://localhost:3002/mcp",
    headers={"Authorization": "Bearer <CAPABILITY_TOKEN>"}
)

agent = Agent(
    "openai:gpt-4o",
    mcp_servers=[agentos_server],
    system_prompt="You have secure file and shell access via AgentOS."
)

result = await agent.run("List files in /tmp/workspace and summarize them")
```

**Files:**
- `examples/bridges/pydantic_ai/agent.py`
- `examples/bridges/pydantic_ai/requirements.txt`
- `examples/bridges/pydantic_ai/README.md`

### 4. Google ADK → AgentOS A2A Bridge

Google ADK supports A2A for agent interop:

```python
# examples/bridges/google_adk/agent.py
from google.adk import Agent
from google.adk.a2a import A2AClient

# Discover AgentOS agent via A2A
agentos_client = A2AClient("http://localhost:3001/.well-known/agent.json")
card = await agentos_client.discover()

# Delegate a secure task to AgentOS
result = await agentos_client.delegate(
    capability="file-read",
    input={"path": "/data/reports/latest.json"}
)
```

**Files:**
- `examples/bridges/google_adk/agent.py`
- `examples/bridges/google_adk/requirements.txt`
- `examples/bridges/google_adk/README.md`

### 5. Reference Architecture Document

**File:** `docs/guide/integration-guide.md`

```
                    ┌──────────────────────────────┐
                    │    External Orchestrator       │
                    │  (LangGraph / CrewAI / ADK)   │
                    └──────────┬───────────────────┘
                               │
                    ┌──────────▼───────────────────┐
                    │     Protocol Layer             │
                    │  MCP (tool calls) + A2A (tasks)│
                    └──────────┬───────────────────┘
                               │
                    ┌──────────▼───────────────────┐
                    │     AgentOS Kernel             │
                    │  CapToken → Sandbox → Execute  │
                    │  Audit → Cost Track → Return   │
                    └──────────────────────────────┘
```

Sections:
1. Architecture overview — protocol layering diagram
2. MCP integration pattern — tool routing with token auth
3. A2A integration pattern — task delegation with discovery
4. Security guarantees — what the orchestrator gets from AgentOS
5. Performance characteristics — latency overhead of MCP routing
6. Deployment topologies — local, sidecar, remote

### 6. Integration Test Suite

```python
# examples/bridges/test_integration.py
# Requires: AgentOS running with MCP server on port 3002

import pytest
import httpx

async def test_mcp_tools_list():
    """External client can list AgentOS tools via MCP."""
    ...

async def test_mcp_tool_call_with_valid_token():
    """External client can call tool with valid CapabilityToken."""
    ...

async def test_mcp_tool_call_without_token_rejected():
    """External client without token is rejected."""
    ...

async def test_a2a_agent_card_discoverable():
    """A2A agent card is served at well-known URL."""
    ...

async def test_a2a_task_delegation():
    """External agent can delegate task via A2A."""
    ...
```

---

## Files Changed

| File | Change |
|------|--------|
| `examples/bridges/langgraph/` (new dir) | LangGraph bridge example |
| `examples/bridges/crewai/` (new dir) | CrewAI bridge example |
| `examples/bridges/pydantic_ai/` (new dir) | PydanticAI bridge example |
| `examples/bridges/google_adk/` (new dir) | Google ADK A2A bridge example |
| `examples/bridges/test_integration.py` (new) | Integration tests |
| `docs/guide/integration-guide.md` (new) | Reference architecture |

---

## Dependencies

- **Requires:** Phase 2 (MCP server operational), Phase 3 (A2A server operational)
- **Blocks:** Nothing — this is the capstone demonstration

---

## Test Plan

1. Start AgentOS with MCP+A2A servers → LangGraph example connects and calls tool → success
2. CrewAI example connects and runs crew → AgentOS tools used with correct token auth
3. PydanticAI example runs agent → MCP tools work
4. Google ADK example discovers agent card → delegates task → receives result
5. Token rejection: all bridges fail gracefully when token is invalid
6. Audit log: all bridge interactions produce audit entries

---

## Verification

```bash
# Start AgentOS with MCP+A2A
agentos kernel start &
agentos mcp serve --transport http --port 3002 &

# Test LangGraph bridge
cd examples/bridges/langgraph
pip install -r requirements.txt
python agent.py

# Test integration suite
cd examples/bridges
pytest test_integration.py -v

# Verify audit trail
agentos audit recent --limit 20
```
