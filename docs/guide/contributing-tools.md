# Contributing Tools to AgentOS

> The path from "I have a tool idea" to "it's published and usable by any agent."

---

## Contribution Ladder

```
Your tool idea
     │
     ▼
Community tier     ← You sign it with your own key, anyone can install
     │
     ▼ (after review)
Verified tier      ← AgentOS team reviews source + signs the manifest
     │
     ▼ (after integration)
Core tier          ← Bundled in the AgentOS binary, always available
```

---

## Quick Contribution Flow

```bash
# 1. Scaffold a new tool project
agentos init my-tool-project --template secure-agent
cd my-tool-project

# 2. Implement your tool (see creating-tools.md)
# Edit tools/my-tool.toml and implement the Rust logic

# 3. Generate a keypair (one-time setup)
agentos tool keygen --out ~/.agentos/author.key
# Keep your private key safe — never commit it

# 4. Sign your manifest
agentos tool sign --manifest tools/my-tool.toml --key ~/.agentos/author.key

# 5. Verify the signature is valid
agentos tool verify tools/my-tool.toml

# 6. Test it locally
agentos kernel start
agentos mcp call --tool my-tool --input '{"example": "input"}'
agentos audit logs --last 5   # Check no unexpected access attempts

# 7. Publish to the local index
agentos tool publish tools/my-tool.toml
```

---

## Getting Your Tool Reviewed (Verified tier)

Open a pull request with:
1. The tool source code and manifest
2. A description of what the tool does and why it's useful
3. A description of what permissions it needs and why
4. Tests covering normal use and edge cases
5. Documentation in the tool's description field

**Review checklist (what we check):**

- [ ] Manifest permissions match actual resource access in the code
- [ ] Path traversal protection — uses `resolve_tool_path()` for all file paths
- [ ] No `.unwrap()` in production paths
- [ ] Secrets accessed only via `ctx.vault` (ProxyVault), never stored as strings
- [ ] Long operations check `ctx.cancellation_token`
- [ ] `trust_tier` is `community` or `verified` (never `core` in PRs)
- [ ] Tests cover permission denial, invalid input, and cancellation

---

## Trust Tier Details

### Community tier
- You sign the manifest with your own Ed25519 key
- AgentOS users can install it with `agentos tool install`
- Runs in a **WASM/Wasmtime sandbox** — cannot escape the sandbox
- Kernel enforces filesystem/network limits even inside WASM
- No code review required

### Verified tier
- AgentOS maintainers review and co-sign the manifest
- Runs under **Seccomp-BPF syscall filtering** (faster than WASM)
- Co-signature added to manifest by the review team
- Requires a pull request and passing tests

### Core tier
- Bundled in the AgentOS binary via `rust-embed`
- Runs **in-process** with full kernel access
- Only for tools maintained by the AgentOS team
- Requires merge into `main` branch

---

## Manifest Signing Deep Dive

AgentOS uses Ed25519 signatures to verify tool authenticity.

### What gets signed

The signature covers a canonical JSON payload:
```json
{
  "name": "my-tool",
  "description": "...",
  "trust_tier": "community",
  "permissions": { "required": ["..."] }
}
```

Fields are sorted alphabetically before signing. This means if any field changes, the signature becomes invalid.

### Verification at registration time

When `agentos kernel start` loads a tool:
1. Reads the manifest TOML
2. Extracts `author_pubkey` and `signature`
3. Constructs the canonical JSON payload
4. Verifies the Ed25519 signature
5. If invalid → `ToolSignatureInvalid` error, tool not loaded
6. If `trust_tier = "blocked"` → `ToolBlocked` error regardless of signature

---

## Tool Manifest Best Practices

```toml
[tool]
name = "fetch-weather"
# ✓ Good description: explains what it does AND what it returns
description = "Fetch current weather for a city using Open-Meteo API. Returns temperature, humidity, and wind speed."
trust_tier = "community"

[permissions]
# ✓ Good: minimal permissions — only what's actually needed
required = ["network.outbound"]
# ✗ Bad: overly broad
# required = ["fs.user_data", "network.outbound", "shell.execute"]
```

**Naming rules:**
- Use kebab-case: `my-tool`, not `myTool` or `my_tool`
- Be specific: `fetch-weather` not `weather` or `util`
- Don't namespace with your username: `fetch-weather` not `john-fetch-weather`

---

## Filing a Bug or Security Issue

- **Bugs:** Open a GitHub issue with the `tool:` prefix in the title
- **Security vulnerabilities:** See [SECURITY.md](../../SECURITY.md) — do NOT open a public issue for security bugs

---

## Related

- [Creating Tools](creating-tools.md) — step-by-step tool development guide
- [Security Model](../whitepapers/agentos-security-model.md) — trust tiers and sandbox architecture
- [Integration Guide](integration-guide.md) — expose tools via MCP
