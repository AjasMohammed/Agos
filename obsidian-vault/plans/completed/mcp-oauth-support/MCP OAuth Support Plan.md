---
title: MCP OAuth2 Support
tags:
  - mcp
  - security
  - oauth
  - v3
date: 2026-04-09
status: in-progress
effort: 1d
priority: high
---

# MCP OAuth2 Support

> Add OAuth2 Bearer token authentication to the HTTP MCP transport so agents can connect to OAuth-protected MCP servers (e.g., Zomato).

---

## Why this matters

HTTP-based MCP servers like `https://mcp-server.zomato.com/mcp` require OAuth2 — not static Bearer tokens. The current `StreamableHttpTransport` only accepts a static `auth_token` string with no refresh lifecycle. Without this, agents cannot reliably connect to any real-world OAuth-protected MCP server.

## Current state

| Component | State |
|-----------|-------|
| `StreamableHttpTransport` | Static Bearer token only; 401 is treated as fatal |
| `agentos-vault` `OAuthStore` | Full OAuth credential CRUD + refresh implemented |
| `agentos-vault` `TokenRefreshLoop` | Background token refresh exists but not wired to MCP |
| `McpAttach` bus command | No OAuth parameters |
| CLI `mcp attach` | `--token` only; no `--oauth-connector` |

## Target architecture

```
CLI: agentos mcp oauth-store zomato [--access-token ...] [--refresh-token ...] ...
       ↓ McpOAuthStore bus command
Kernel: SecretsVault.oauth_store().store(...)

CLI: agentos mcp attach zomato --url https://... --oauth-connector zomato
       ↓ McpAttach { oauth_connector_id: Some("zomato") }
Kernel: cmd_mcp_attach builds VaultOAuthProvider(connector_id, vault)
       ↓
StreamableHttpTransport::new_with_oauth(url, VaultOAuthProvider)
       ↓
On each request: provider.get_token() → add Bearer header
On 401: provider.force_refresh() → retry once
```

## Phase overview

| Phase | Name | Detail Doc | Status |
|-------|------|-----------|--------|
| 1 | OAuthTokenProvider trait + HTTP transport changes | [[01-oauth-trait-and-transport]] | complete |
| 2 | VaultOAuthProvider + bus + kernel wiring | [[02-vault-provider-and-kernel]] | complete |
| 3 | CLI commands + attachment store migration | [[03-cli-and-store-migration]] | complete |

## Key design decisions

1. **Trait in `agentos-mcp`, impl in `agentos-kernel`** — avoids adding `agentos-vault` as a dependency of `agentos-mcp`; the kernel already depends on both.
2. **401 retry once** — on a 401, force-refresh the token and retry exactly once. If the retry also returns 401, propagate as a fatal `McpTransportError::Auth` error.
3. **New `McpTransportError::Auth` variant** — distinguishes auth failures from connection failures; does NOT trigger reconnect (reconnect doesn't help with expired tokens).
4. **`McpOAuthStore` bus command** — storing credentials goes through the kernel bus because the vault master key is only available to the running kernel.
5. **`oauth_connector_id` in `McpAttach`** — the connector ID references a pre-stored OAuth credential in the vault. Mutually exclusive with `auth_token`.
6. **Attachment store migration** — adds `oauth_connector_id` column; existing rows have `NULL` (no breaking change).

## Risks

| Risk | Mitigation |
|------|-----------|
| Zomato OAuth redirect URI not whitelisted for AgentOS | Use manually-obtained tokens via `mcp oauth-store`; contact Zomato for redirect URI |
| Token refresh race (multiple concurrent requests) | VaultOAuthProvider serializes refresh via tokio Mutex; requests queue behind the refresh |
