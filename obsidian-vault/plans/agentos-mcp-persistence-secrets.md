---
title: MCP Runtime Persistence & Vault Secret Injection
tags:
  - mcp
  - kernel
  - vault
  - plan
date: 2026-04-09
status: in-progress
effort: 3h
priority: high
---

# MCP Runtime Persistence & Vault Secret Injection

> Persists runtime `mcp attach` entries across kernel restarts, and resolves `vault:KEY` references in MCP server env vars so secrets never appear in plaintext config or CLI args.

---

## Problem

1. `agentos mcp attach github -- npx ...` is ephemeral — lost on restart.
2. MCP servers that need tokens (e.g. `GITHUB_TOKEN`) currently require them as shell env vars or plaintext in config — not integrated with the vault.

## Design

### Persistence: McpAttachmentStore

SQLite table at `{data_dir}/mcp_attachments.db`:

```sql
CREATE TABLE IF NOT EXISTS mcp_attachments (
    name       TEXT PRIMARY KEY,
    command    TEXT,
    args_json  TEXT NOT NULL,
    url        TEXT,
    env_json   TEXT NOT NULL,   -- may contain "vault:KEY" references
    timeout_secs INTEGER,
    created_at TEXT NOT NULL
);
```

- `cmd_mcp_attach` → upserts row
- `cmd_mcp_detach` → deletes row
- Kernel boot → loads all rows, spawns them after config-based servers

### Secret injection: `vault:KEY` syntax

In env vars (both config and runtime), values of the form `vault:KEY` are resolved from the vault at spawn time:

```toml
[[mcp.servers]]
name    = "github"
command = "npx"
args    = ["-y", "@modelcontextprotocol/server-github"]
env     = { GITHUB_PERSONAL_ACCESS_TOKEN = "vault:github_token" }
```

```bash
agentos mcp attach github \
  --env GITHUB_TOKEN=vault:github_token \
  -- npx -y @modelcontextprotocol/server-github
```

Resolved values are passed directly to the subprocess — never logged or stored plaintext.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/mcp_attachment_store.rs` | New: SQLite persistence |
| `crates/agentos-kernel/src/kernel.rs` | Add store field; load persisted attachments at boot; resolve vault secrets for config-based servers |
| `crates/agentos-kernel/src/commands/mcp.rs` | Save/delete from store; resolve vault secrets at attach time |
| `crates/agentos-bus/src/message.rs` | Add `env` field to `McpAttach` command |
| `crates/agentos-cli/src/commands/mcp.rs` | Add `--env KEY=VALUE` flag to `Attach` |
| `crates/agentos-cli/src/main.rs` | Pass env to `McpAttach` command |

## Related
[[MCP Runtime Attach/Detach]]
