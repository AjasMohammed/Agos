# MCP Integrations

AgentOS speaks the **Model Context Protocol (MCP)** in both directions: it can **import**
tools from external MCP servers, and **export** its own tools as an MCP server for clients
like Claude Desktop and Cursor. The implementation lives in `crates/agentos-mcp`.

## Import: connect external MCP servers

Declare servers in the `[mcp]` config block; the kernel spawns each as a child process and
connects over stdio JSON-RPC at boot:

```toml
[[mcp.servers]]
name = "filesystem"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
```

Imported tools register with **`TrustTier::Community`** and `RiskClass::ReadonlyExternal`,
and are subject to the **full AgentOS capability-token and `PermissionSet` enforcement** —
an MCP tool cannot do anything an agent lacks permission for. They appear alongside native
tools in `agent-manual` and `agentos tool list`.

## Export: serve AgentOS tools over MCP

Expose all registered AgentOS tools as an MCP server:

```bash
# stdio (default) — point a local MCP client's config at this command
agentos mcp serve

# HTTP transport with bearer-token auth
agentos mcp serve --transport http --port 3002 --token <secret>
```

Other MCP CLI subcommands run without a kernel connection:

```bash
agentos mcp tools                                   # list available MCP tools
agentos mcp call --tool file-reader --input '{"path":"notes.txt"}'
```

## A2A

The crate also implements the **Agent-to-Agent (A2A)** protocol (`agentos a2a …`) for
discovering and delegating to external agents.

## Planned: one-command catalog installer

A catalog-based installer — `agentos mcp install <id>` — is planned (production-release
Phase 04). It will add a TOML catalog + registry, a runtime resolver (nvm/volta/asdf/bundled),
OAuth helper automation, and seed entries for common servers (filesystem, GitHub, Slack,
Postgres, SQLite, Brave, Puppeteer, and a vetted Google Workspace server). Until then, connect
MCP servers through the `[mcp]` config block above.
