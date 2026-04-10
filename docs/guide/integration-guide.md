# Integration Guide — Connecting External Frameworks to AgentOS

> Use AgentOS as the secure execution layer for LangGraph, CrewAI, PydanticAI, Google ADK, or any MCP/A2A-compatible framework.

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                    EXTERNAL FRAMEWORKS                           │
│  ┌───────────┐ ┌──────────┐ ┌───────────┐ ┌──────────────────┐ │
│  │ LangGraph │ │  CrewAI  │ │PydanticAI │ │   Google ADK     │ │
│  └─────┬─────┘ └────┬─────┘ └─────┬─────┘ └────────┬─────────┘ │
└────────┼────────────┼─────────────┼────────────────┼────────────┘
         │            │             │                │
         └─── MCP ────┴─────────────┘                │ A2A
              (tool calls)                           │ (task delegation)
                   │                                 │
┌──────────────────▼─────────────────────────────────▼────────────┐
│                     AgentOS KERNEL                               │
│                                                                  │
│   CapToken validation → Trust tier → Sandbox → Tool execute      │
│   Audit log ← Cost tracking ← Result                            │
└──────────────────────────────────────────────────────────────────┘
```

**Key principle:** Frameworks build and orchestrate agents. AgentOS runs them safely.

- **MCP** (port 3002) — for tool use: any framework calls AgentOS tools
- **A2A** (port 3001) — for agent delegation: frameworks hand whole tasks to AgentOS

---

## Quick Start: Expose Tools via MCP

```bash
# Start AgentOS with HTTP MCP server
agentos kernel start
agentos mcp serve --transport http --port 3002 --token mysecret
```

Then any MCP-compatible framework can use your tools:

```bash
# Test from curl
curl -X POST http://localhost:3002/mcp \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer mysecret" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
```

---

## LangGraph Integration

```python
from langchain_mcp_adapters.client import MultiServerMCPClient
from langgraph.prebuilt import create_react_agent

async with MultiServerMCPClient({
    "agentos": {
        "url": "http://localhost:3002/mcp",
        "transport": "streamable_http",
        "headers": {"Authorization": "Bearer mysecret"}
    }
}) as mcp_client:
    tools = mcp_client.get_tools()
    agent = create_react_agent(model, tools)
    result = await agent.ainvoke({"messages": [("user", "List workspace files")]})
```

**Full example:** [`examples/bridges/langgraph/agent.py`](../../examples/bridges/langgraph/agent.py)

---

## CrewAI Integration

```python
from crewai import Agent, Crew, Task
from crewai_tools import MCPServerAdapter

mcp_adapter = MCPServerAdapter({
    "url": "http://localhost:3002/mcp",
    "headers": {"Authorization": "Bearer mysecret"}
})

researcher = Agent(
    role="Researcher",
    tools=mcp_adapter.tools,
    ...
)
```

**Full example:** [`examples/bridges/crewai/crew.py`](../../examples/bridges/crewai/crew.py)

---

## PydanticAI Integration (MCP + A2A)

```python
from pydantic_ai import Agent
from pydantic_ai.mcp import MCPServerHTTP

agent = Agent(
    "anthropic:claude-3-5-sonnet-20241022",
    mcp_servers=[MCPServerHTTP(
        url="http://localhost:3002/mcp",
        headers={"Authorization": "Bearer mysecret"}
    )],
)
async with agent.run_mcp_servers():
    result = await agent.run("Research AI safety trends")
```

**Full example:** [`examples/bridges/pydantic_ai/agent.py`](../../examples/bridges/pydantic_ai/agent.py)

---

## Google ADK / A2A Integration

```python
import httpx

# Discover AgentOS's capabilities
card = (await client.get("http://localhost:3001/.well-known/agent.json")).json()

# Delegate a task
resp = await client.post("http://localhost:3001/a2a/tasks", json={
    "sender": "http://my-adk-agent.local",
    "capability": "file-reader",
    "input": {"path": "data.json"}
})
task_id = resp.json()["id"]

# Poll for result
task = (await client.get(f"http://localhost:3001/a2a/tasks/{task_id}")).json()
```

**Full example:** [`examples/bridges/google_adk/agent.py`](../../examples/bridges/google_adk/agent.py)

---

## Security Guarantees

Regardless of which framework orchestrates the agent, AgentOS enforces:

| Guarantee | Mechanism |
|-----------|-----------|
| Only allowed tools can be called | CapabilityToken `allowed_tools` whitelist |
| File access confined to workspace | `resolve_tool_path()` + canonicalize check |
| No SSRF to private IP ranges | `PermissionSet::is_denied()` |
| Every tool call audited | Append-only SQLite + SHA-256 chain |
| Malicious prompts detected | InjectionScanner (32+ patterns, NFKC) |
| Secret values never in tool output | ProxyVault at tool boundary |

**A malicious prompt that reaches the LLM cannot make the agent exceed its capability boundaries** — the kernel rejects unauthorized calls regardless of what the LLM instructs.

---

## Configuring Tool Access Per Framework

You can mint restricted tokens for each external framework:

```bash
# LangGraph agent: read-only file access
agentos agent mint-token \
  --tools "file-reader,memory-search" \
  --deny "fs:/etc,net:http" \
  --expires 24h \
  --out langgraph.token

# CrewAI agents: read + write
agentos agent mint-token \
  --tools "file-reader,file-writer,memory-search,memory-write" \
  --expires 24h \
  --out crewai.token
```

Start dedicated MCP servers for each framework with its own token:

```bash
agentos mcp serve --transport http --port 3002 --token "$(cat langgraph.token)" &
agentos mcp serve --transport http --port 3003 --token "$(cat crewai.token)" &
```

---

## Checking the Audit Trail

After running any bridge example, inspect what was called:

```bash
agentos audit logs --last 20
```

You'll see entries for every tool call, including the agent ID, tool name, timestamp, and result.

---

## Troubleshooting

| Problem | Solution |
|---------|----------|
| `401 Unauthorized` | Check your `--token` matches `AGENTOS_TOKEN` |
| `Tool not found` | Run `agentos mcp tools` to see what's available |
| `Path traversal denied` | Don't use `..` in file paths — use absolute paths in the data dir |
| `SSRF blocked` | AgentOS blocks private IPs; use public URLs for external APIs |
| `CapabilityDenied: tool not in allowed_tools` | Add the tool to `allowed_tools` in your agent manifest |

---

## Related

- [Creating Tools](creating-tools.md) — build custom tools
- [Security Model](../whitepapers/agentos-security-model.md) — understand what AgentOS enforces
- [Getting Started](getting-started.md) — first agent in 5 minutes
