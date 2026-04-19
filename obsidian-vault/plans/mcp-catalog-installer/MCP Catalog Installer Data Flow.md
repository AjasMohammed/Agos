---
title: MCP Catalog Installer Data Flow
tags:
  - mcp
  - flow
  - diagram
date: 2026-04-18
status: planned
effort: 0.25d
priority: high
---

# MCP Catalog Installer — Data Flow

> Request flow and error branches for `agentos mcp install <name>`.

---

## Happy path

```mermaid
sequenceDiagram
    participant U as User
    participant CLI as agentos (CLI)
    participant K as Kernel
    participant CAT as CatalogRegistry
    participant RT as RuntimeResolver
    participant PKG as PackagePrefetch
    participant AUTH as AuthHelper
    participant MCP as McpSupervisor

    U->>CLI: agentos mcp install gmail
    CLI->>K: KernelCommand::McpInstall("gmail")
    K->>CAT: lookup("gmail")
    CAT-->>K: CatalogEntry { runtime=node, package=..., auth=oauth }
    K->>RT: resolve_node(min="18")
    RT-->>K: /home/ajas/.nvm/.../node
    K->>PKG: prefetch(package, timeout=5min)
    PKG-->>K: cached
    K->>AUTH: check_credentials("google", "~/.gmail-mcp/credentials.json")
    alt missing
        K->>CLI: PromptForAuth
        CLI->>U: Run OAuth helper now? [Y/n]
        U->>CLI: Y
        CLI->>K: UserConfirmed
        K->>AUTH: run_helper("npx @gongrzhe/... auth")
        AUTH-->>K: credentials_written
    end
    K->>MCP: attach (reuses cmd_mcp_attach)
    MCP-->>K: handshake_ok, tools_discovered
    K->>CAT: persist_install_record
    K-->>CLI: McpInstalled { name, tools_count }
    CLI-->>U: ✅ Gmail MCP ready. 20 tools available.
```

---

## Error branches

### 1. Unknown server name

```
User: agentos mcp install foo
Kernel:
  CatalogRegistry::lookup("foo") -> None
Response: Error { message: "No catalog entry 'foo'. Try: agentos mcp catalog search <keyword>" }
```

### 2. Community-tier entry without `--unsafe-allow-community`

```
Interactive mode:
  "Package 'xyz' is community-tier (not vetted). Install anyway? [y/N]"
  If N -> abort with audit event McpInstallRejected { reason: trust_tier }
Non-interactive (--yes without --unsafe-allow-community):
  Abort with explicit error recommending the flag.
```

### 3. Runtime not found or version too low

```
RuntimeResolver::resolve_node(min="18"):
  - nvm: v12.22.9 (too old)
  - volta: not installed
  - asdf: not installed
  - system: not found
Response: Error {
  message: "Need node >= 18 for 'gmail'. Install with: agentos runtime install node@20,
            or pass --runtime-binary /path/to/node"
}
```

### 4. Package prefetch fails

```
PackagePrefetch::npx("-y", "@gongrzhe/..."):
  process exits non-zero, stderr: "404 Not Found"
Response: Error {
  message: "Failed to fetch '@gongrzhe/server-gmail-autoauth-mcp': 404 Not Found.
            Check catalog entry version or npm registry access."
}
```

### 5. OAuth helper abandoned (Ctrl+C mid-flow)

```
AuthHelper::run_helper():
  Detects subprocess exit code != 0
  Verifies credentials_path still missing
Response: Error {
  message: "OAuth flow was not completed. Run 'agentos mcp install gmail' again."
}
Audit: McpAuthAborted { name: "gmail" }
Clean up: no partial state left behind.
```

### 6. Handshake succeeds but tools/list is empty

```
McpSupervisor::list_tools -> Ok([])
Treat as warning, not error. Install succeeds, CLI prints:
  "⚠️  Attached but no tools discovered. The server may require
   additional configuration — check its docs or run:
   agentos mcp call <tool_name> <input>"
```

---

## Uninstall flow

```mermaid
sequenceDiagram
    participant U as User
    participant CLI as agentos
    participant K as Kernel
    participant MCP as McpSupervisor
    participant CAT as CatalogRegistry

    U->>CLI: agentos mcp uninstall gmail
    CLI->>K: KernelCommand::McpUninstall("gmail")
    K->>MCP: detach("gmail")
    MCP-->>K: ok
    K->>CAT: remove_install_record("gmail")
    Note over K: Optional (--purge): delete OAuth credentials, package cache
    K-->>CLI: McpUninstalled
```

---

## Catalog update flow

```mermaid
sequenceDiagram
    participant U as User
    participant CLI as agentos
    participant K as Kernel
    participant CAT as CatalogRegistry

    U->>CLI: agentos mcp update gmail
    CLI->>K: McpCatalogUpdate("gmail")
    K->>CAT: fetch_latest("gmail")
    CAT-->>K: new_entry (version bump, package updated)
    K-->>CLI: diff (version, package, env vars, risk classes)
    CLI->>U: "Apply update? [Y/n]"
    U->>CLI: Y
    CLI->>K: McpCatalogApplyUpdate("gmail")
    K->>MCP: detach
    K->>PKG: prefetch_new_version
    K->>MCP: attach with new args
    K-->>CLI: ok
```

Only runs on user command — no silent auto-update.

---

## Related

- [[MCP Catalog Installer Plan]]
- [[MCP Catalog Installer Research]]
- [[01-runtime-resolver]]
- [[04-install-command]]
