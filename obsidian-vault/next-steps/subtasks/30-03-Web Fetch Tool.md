---
title: 30-03 Web Fetch Tool
tags:
  - tools
  - network
  - next-steps
  - subtask
date: 2026-03-18
status: planned
effort: 4h
priority: high
---

# 30-03 — Web Fetch Tool

> Add `web-fetch`: fetches a URL with reqwest and converts HTML to readable Markdown/text so agents can read web pages without raw HTML noise.

---

## Why This Phase

`http-client` makes raw HTTP requests but returns the full HTTP body including HTML tags, scripts, and boilerplate. An agent trying to read a documentation page or article receives ~50KB of HTML when it needs ~5KB of content. This forces agents to pipe through `shell-exec` with `html2text` or `lynx`, bypassing the capability model.

`web-fetch` wraps reqwest (already a dependency) and adds the `html2text` crate to strip HTML.

---

## Current → Target State

| Capability | Current | Target |
|-----------|---------|--------|
| Fetch URL + get text | `shell-exec curl \| html2text` | `web-fetch` first-class tool |
| HTML stripping | none within capability model | `html2text` crate, pure-Rust |
| Output format | raw HTML | plain text or Markdown |

---

## What to Do

### Step 1 — Add `html2text` to `crates/agentos-tools/Cargo.toml`

Read `crates/agentos-tools/Cargo.toml` first to see existing deps. Then add:
```toml
html2text = "0.12"
```

`html2text` 0.12 is pure-Rust (no C native deps), MIT licensed, ~300KB. Verify the latest version at crates.io if needed.

### Step 2 — Create `crates/agentos-tools/src/web_fetch.rs`

```rust
use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

pub struct WebFetch;

impl WebFetch {
    pub fn new() -> Self { Self }
}

impl Default for WebFetch {
    fn default() -> Self { Self::new() }
}

#[async_trait]
impl AgentTool for WebFetch {
    fn name(&self) -> &str { "web-fetch" }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("network.outbound".to_string(), PermissionOp::Execute)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        if !context.permissions.check("network.outbound", PermissionOp::Execute) {
            return Err(AgentOSError::PermissionDenied {
                resource: "network.outbound".to_string(),
                operation: "Execute".to_string(),
            });
        }

        let url = payload
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AgentOSError::SchemaValidation("web-fetch requires 'url' field".into()))?;

        // SSRF guard: block private/loopback addresses
        // Parse the URL and reject non-http(s) schemes
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(AgentOSError::SchemaValidation(
                "web-fetch only supports http:// and https:// URLs".into(),
            ));
        }

        let extract_text = payload
            .get("extract_text")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let max_chars = payload
            .get("max_chars")
            .and_then(|v| v.as_u64())
            .unwrap_or(32_000)
            .min(100_000) as usize;

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::limited(5))
            .user_agent("AgentOS/1.0")
            .build()
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "web-fetch".into(),
                reason: format!("Failed to build HTTP client: {}", e),
            })?;

        let response = client.get(url).send().await.map_err(|e| {
            AgentOSError::ToolExecutionFailed {
                tool_name: "web-fetch".into(),
                reason: format!("HTTP request failed: {}", e),
            }
        })?;

        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let body = response.text().await.map_err(|e| {
            AgentOSError::ToolExecutionFailed {
                tool_name: "web-fetch".into(),
                reason: format!("Failed to read response body: {}", e),
            }
        })?;

        let (content, was_extracted) = if extract_text && content_type.contains("html") {
            let text = html2text::from_read(body.as_bytes(), 80);
            // Truncate to max_chars
            let truncated = if text.len() > max_chars {
                format!("{}... [truncated at {} chars]", &text[..max_chars], max_chars)
            } else {
                text
            };
            (truncated, true)
        } else {
            let truncated = if body.len() > max_chars {
                format!("{}... [truncated at {} chars]", &body[..max_chars], max_chars)
            } else {
                body
            };
            (truncated, false)
        };

        Ok(serde_json::json!({
            "url": url,
            "status_code": status,
            "content_type": content_type,
            "text_extracted": was_extracted,
            "content": content,
            "char_count": content.len(),
        }))
    }
}
```

**SSRF note:** The current implementation only checks the URL scheme. A stronger guard would resolve the hostname and reject RFC-1918 ranges. This can be added in a follow-up; document it as a known gap.

### Step 3 — Register in `lib.rs`

```rust
pub mod web_fetch;
pub use web_fetch::WebFetch;
```

### Step 4 — Register in `runner.rs`

```rust
use crate::web_fetch::WebFetch;
// In registration block:
runner.register(Box::new(WebFetch::new()));
```

Read `runner.rs` to find the correct registration method name.

### Step 5 — Create `tools/core/web-fetch.toml`

```toml
[manifest]
name        = "web-fetch"
version     = "1.0.0"
description = "Fetch a URL and return its content as readable text (HTML is auto-converted to plain text). Use 'extract_text: false' to get raw body."
author      = "agentos-core"
trust_tier  = "core"

[capabilities_required]
permissions = ["network.outbound:x"]

[capabilities_provided]
outputs = ["content.text"]

[intent_schema]
input  = "WebFetchIntent"
output = "WebFetchResult"

[sandbox]
network       = true
fs_write      = false
gpu           = false
max_memory_mb = 64
max_cpu_ms    = 35000
syscalls      = []
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-tools/Cargo.toml` | Add `html2text = "0.12"` |
| `crates/agentos-tools/src/web_fetch.rs` | Create |
| `crates/agentos-tools/src/lib.rs` | Add `pub mod web_fetch;` and re-export |
| `crates/agentos-tools/src/runner.rs` | Register `WebFetch` |
| `tools/core/web-fetch.toml` | Create |

---

## Prerequisites

None — reqwest already a dependency. Only new crate dep is `html2text`.

## Verification

```bash
cargo build -p agentos-tools
cargo test -p agentos-tools -- web_fetch
```

Unit test (mock HTTP not needed — test SSRF rejection and scheme validation):
```rust
#[tokio::test]
async fn web_fetch_rejects_non_http_scheme() {
    let tool = WebFetch::new();
    let result = tool.execute(
        serde_json::json!({"url": "ftp://example.com"}),
        ctx_with_network_perms(),
    ).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn web_fetch_requires_network_permission() {
    let tool = WebFetch::new();
    let result = tool.execute(
        serde_json::json!({"url": "https://example.com"}),
        ctx_without_network_perms(),
    ).await;
    assert!(matches!(result, Err(AgentOSError::PermissionDenied { .. })));
}
```
