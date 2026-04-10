# LangGraph → AgentOS Bridge

Use AgentOS tools from a LangGraph ReAct agent via MCP.

## Setup

```bash
# 1. Start AgentOS MCP server with HTTP transport
agentos mcp serve --transport http --port 3002 --token mysecret &

# 2. Install Python dependencies
pip install -r requirements.txt

# 3. Run the bridge example
ANTHROPIC_API_KEY=sk-... AGENTOS_TOKEN=mysecret python agent.py
```

## What happens

1. The LangGraph agent connects to AgentOS via MCP HTTP transport
2. It discovers all AgentOS tools via `tools/list`
3. On each tool call, AgentOS validates the Bearer token → CapabilityToken
4. All tool executions are logged to AgentOS's audit trail

## Security guarantees

Even though LangGraph orchestrates the agent, AgentOS enforces:
- **CapabilityToken validation** on every tool call
- **Path traversal blocking** in file tools
- **Audit logging** of every execution
- **SSRF protection** on network tools

A malicious prompt cannot trick the agent into exceeding its defined boundaries
because the kernel rejects unauthorized calls regardless of what the LLM says.

## Custom token

To restrict what tools this agent can call:

```bash
# Mint a restricted capability token for this agent
agentos agent mint-token --agent langgraph-agent \
  --tools "file-reader,memory-search" \
  --expires 24h \
  --out langgraph.token

# Use it when starting the MCP server
agentos mcp serve --transport http --port 3002 --token "$(cat langgraph.token)"
```
