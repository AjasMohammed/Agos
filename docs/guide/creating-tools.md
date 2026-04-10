# Creating AgentOS Tools

> Build a custom tool in under 10 minutes and make it available to any agent.

---

## What is a Tool?

A tool is a sandboxed capability an agent can invoke. Tools are:
- **Isolated** — they run in a WASM sandbox (Community tier) or in-process (Core tier)
- **Authorized** — every call requires a valid CapabilityToken
- **Audited** — every execution is logged to the append-only audit trail

---

## Quick Start: Hello World Tool (5 minutes)

### 1. Create a project with the SDK template

```bash
agentos init my-tool --template secure-agent
cd my-tool
```

### 2. Create the tool manifest

Create `tools/word-count.toml`:

```toml
[tool]
name = "word-count"
description = "Count words in a text string"
trust_tier = "core"        # core = in-process, community = WASM sandbox

[permissions]
required = []              # This tool needs no resource permissions
```

### 3. Implement the tool in Rust

Create `src/tools/word_count.rs`:

```rust
use agentos_sdk::prelude::*;

#[tool(
    name = "word-count",
    description = "Count words in a text string",
    permissions = []
)]
async fn word_count(input: WordCountInput) -> Result<ToolOutput> {
    let count = input.text.split_whitespace().count();
    Ok(ToolOutput::json(json!({ "word_count": count, "text": input.text })))
}

#[derive(Deserialize)]
struct WordCountInput {
    text: String,
}
```

### 4. Add it to `allowed_tools` in `agent.toml`

```toml
[capabilities]
allowed_tools = ["file-reader", "word-count"]
```

### 5. Test it

```bash
agentos kernel start
agentos mcp call --tool word-count --input '{"text": "hello world foo"}'
# Output: {"word_count": 3, "text": "hello world foo"}
```

---

## Tool Manifest Reference

Every tool needs a TOML manifest. Place it in `tools/` in your project.

```toml
[tool]
name = "my-tool"                     # Must be kebab-case, globally unique
description = "What this tool does"  # Shown in MCP tools/list

# Trust tier determines the sandbox:
#   core      — in-process, full kernel access, highest performance
#   verified  — seccomp-BPF syscall filter, reviewed by maintainers
#   community — WASM/Wasmtime isolation, suitable for third-party tools
#   blocked   — kernel hard-rejects, used to ban known-malicious tools
trust_tier = "community"

[permissions]
# Resources this tool needs. The kernel validates these against the
# agent's CapabilityToken before allowing execution.
required = [
    "fs.user_data",        # Read/write files in the agent's data dir
    "network.outbound",    # Outbound HTTP requests
    "memory.semantic",     # Query the semantic memory store
]

[signing]
# Required for community/verified tiers.
# Generate a keypair: agentos tool keygen --out ~/.agentos/my-key.json
# Sign the manifest:  agentos tool sign --key ~/.agentos/my-key.json
author_pubkey = "ed25519:<base64-public-key>"
signature     = "<base64-ed25519-signature>"
```

---

## Implementing with the Rust SDK

Add `agentos-sdk` to your `Cargo.toml`:

```toml
[dependencies]
agentos-sdk = { git = "https://github.com/your-org/agentos" }
```

### The `#[tool]` macro

```rust
use agentos_sdk::prelude::*;

#[tool(
    name = "http-fetch",
    description = "Fetch a URL and return the response body",
    permissions = ["network.outbound"]
)]
async fn http_fetch(input: HttpFetchInput) -> Result<ToolOutput> {
    // The cancellation token lets the kernel abort long-running calls
    let ctx = tool_context!();

    let resp = reqwest::get(&input.url).await?.text().await?;
    Ok(ToolOutput::text(resp))
}

#[derive(Deserialize)]
struct HttpFetchInput {
    url: String,
}
```

### Accessing the vault (secrets)

```rust
#[tool(name = "api-call", permissions = ["network.outbound"])]
async fn api_call(input: ApiCallInput) -> Result<ToolOutput> {
    let ctx = tool_context!();

    // vault is a ProxyVault — secrets are never exposed as plaintext
    let api_key = ctx.vault
        .as_ref()
        .ok_or_else(|| AgentOSError::ToolExecutionFailed {
            tool_name: "api-call".into(),
            reason: "vault not available".into(),
        })?
        .get_secret("MY_API_KEY")
        .await?;

    // api_key is a ZeroizingString — wiped from memory when dropped
    let resp = reqwest::Client::new()
        .get(&input.url)
        .header("Authorization", format!("Bearer {}", *api_key))
        .send()
        .await?;

    Ok(ToolOutput::json(serde_json::json!({"status": resp.status().as_u16()})))
}
```

---

## Implementing Without the SDK

Implement `AgentTool` directly for full control:

```rust
use agentos_tools::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

pub struct WordCount;

#[async_trait]
impl AgentTool for WordCount {
    fn name(&self) -> &str { "word-count" }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![] // No permissions needed
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let text = payload
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AgentOSError::SchemaValidation("missing 'text' field".into()))?;

        let count = text.split_whitespace().count();
        Ok(serde_json::json!({ "word_count": count }))
    }
}
```

---

## Security Best Practices

### Never trust the input

```rust
// ✗ Bad — path traversal possible
let path = format!("/data/{}", input.filename);

// ✓ Good — use resolve_tool_path which decodes %2e%2e and rejects ..
use agentos_tools::traits::resolve_tool_path;
let path = resolve_tool_path(&input.filename, &ctx.data_dir, &ctx.workspace_paths)?;
```

### Respect the cancellation token

```rust
// Check token for long-running operations
for chunk in data.chunks(1000) {
    if ctx.cancellation_token.is_cancelled() {
        return Err(AgentOSError::ToolExecutionFailed {
            tool_name: "my-tool".into(),
            reason: "cancelled".into(),
        });
    }
    process_chunk(chunk).await?;
}
```

### Use the right trust tier

| Tier | When to use |
|------|------------|
| `core` | Your own audited tool, bundled with AgentOS |
| `verified` | Reviewed third-party tool, known author |
| `community` | Untrusted external tool (default for new tools) |

---

## Publishing Your Tool

```bash
# Generate a signing keypair (one-time setup)
agentos tool keygen --out ~/.agentos/my-key.json

# Sign your manifest
agentos tool sign --manifest tools/my-tool.toml --key ~/.agentos/my-key.json

# Verify the signature
agentos tool verify tools/my-tool.toml

# Publish to local index
agentos tool publish tools/my-tool.toml

# Search the index
agentos tool search "word count"
```

---

## Related

- [Security Model](../whitepapers/agentos-security-model.md) — how trust tiers and capability tokens work
- [Getting Started](getting-started.md) — first agent in 5 minutes
- [Integration Guide](integration-guide.md) — expose tools via MCP to external frameworks
