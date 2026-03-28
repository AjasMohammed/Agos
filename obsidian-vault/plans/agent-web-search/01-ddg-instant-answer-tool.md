---
title: "Phase 1: DDG Instant Answer Tool"
tags:
  - tools
  - search
  - network
  - phase-1
date: 2026-03-25
status: planned
effort: 0.5d
priority: high
---

# Phase 1: DDG Instant Answer Tool

> Implement `web-search` as a new tool in `agentos-tools` that calls the official DuckDuckGo Instant Answer API — zero new dependencies, no API key, stable structured JSON output.

---

## Why This Phase

The DuckDuckGo Instant Answer API (`api.duckduckgo.com`) is the only zero-dependency, no-API-key, **official** (non-scraping) search endpoint available. It is stable, returns structured JSON, and works with the existing `reqwest` client already in `agentos-tools`.

This phase does not return a full ranked list of web results — it returns instant answers (Wikipedia summaries, definitions, computed answers, related topics). This covers the most common factual agent queries immediately, without any fragility risk. Full web search results come in Phase 2.

---

## Current → Target State

**Current:** No `web-search` tool exists. Agents cannot discover URLs.

**Target:** A `web-search` tool that calls `https://api.duckduckgo.com/?q={query}&format=json&no_html=1&skip_disambig=1` and returns structured JSON with the instant answer, abstract, and related topics (each with title + URL + snippet).

---

## DDG Instant Answer API Response Shape

The API returns a large JSON object. The fields agents care about:

```json
{
  "Type": "A",              // A=article, D=disambiguation, C=category, N=name, E=exclusive, ""=no result
  "Heading": "Rust (programming language)",
  "Abstract": "Rust is a multi-paradigm...",
  "AbstractURL": "https://en.wikipedia.org/wiki/Rust_(programming_language)",
  "AbstractSource": "Wikipedia",
  "Answer": "",             // Direct computed answer (e.g. "42" for "what is 6*7")
  "AnswerType": "",
  "RelatedTopics": [
    {
      "Text": "Tokio – Async runtime for Rust",
      "FirstURL": "https://tokio.rs",
      "Icon": { "URL": "..." }
    }
  ]
}
```

---

## Detailed Subtasks

### 1. Create `crates/agentos-tools/src/web_search.rs`

Implement `WebSearch` struct implementing `AgentTool`:

```rust
use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::Value;

pub struct WebSearch {
    client: Client,
}

impl WebSearch {
    pub fn new() -> Result<Self, AgentOSError> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .user_agent("AgentOS/1.0")
            .build()
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "web-search".into(),
                reason: format!("Failed to build HTTP client: {}", e),
            })?;
        Ok(Self { client })
    }
}

#[async_trait]
impl AgentTool for WebSearch {
    fn name(&self) -> &str { "web-search" }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("network.outbound".to_string(), PermissionOp::Execute)]
    }

    async fn execute(&self, payload: Value, context: ToolExecutionContext) -> Result<Value, AgentOSError> {
        if !context.permissions.check("network.outbound", PermissionOp::Execute) {
            return Err(AgentOSError::PermissionDenied {
                resource: "network.outbound".to_string(),
                operation: "Execute".to_string(),
            });
        }

        let query = payload.get("query").and_then(|v| v.as_str()).ok_or_else(|| {
            AgentOSError::SchemaValidation("web-search requires 'query' field".into())
        })?;

        if query.trim().is_empty() {
            return Err(AgentOSError::SchemaValidation("query must not be empty".into()));
        }

        let url = format!(
            "https://api.duckduckgo.com/?q={}&format=json&no_html=1&skip_disambig=1",
            urlencoding::encode(query)
        );

        let response = tokio::select! {
            result = self.client.get(&url).send() => {
                result.map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "web-search".into(),
                    reason: format!("HTTP request failed: {}", e),
                })?
            }
            _ = context.cancellation_token.cancelled() => {
                return Err(AgentOSError::ToolExecutionFailed {
                    tool_name: "web-search".into(),
                    reason: "Tool execution cancelled".into(),
                });
            }
        };

        let body: Value = response.json().await.map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: "web-search".into(),
            reason: format!("Failed to parse response: {}", e),
        })?;

        // Extract the fields agents care about
        let result_type = body.get("Type").and_then(|v| v.as_str()).unwrap_or("");
        let heading = body.get("Heading").and_then(|v| v.as_str()).unwrap_or("");
        let abstract_text = body.get("Abstract").and_then(|v| v.as_str()).unwrap_or("");
        let abstract_url = body.get("AbstractURL").and_then(|v| v.as_str()).unwrap_or("");
        let abstract_source = body.get("AbstractSource").and_then(|v| v.as_str()).unwrap_or("");
        let answer = body.get("Answer").and_then(|v| v.as_str()).unwrap_or("");

        let related_topics: Vec<Value> = body
            .get("RelatedTopics")
            .and_then(|v| v.as_array())
            .unwrap_or(&vec![])
            .iter()
            .filter_map(|t| {
                let text = t.get("Text")?.as_str()?;
                let url = t.get("FirstURL")?.as_str()?;
                if text.is_empty() || url.is_empty() { return None; }
                Some(serde_json::json!({
                    "title": text.split(" – ").next().unwrap_or(text),
                    "url": url,
                    "snippet": text,
                }))
            })
            .take(10)
            .collect();

        let has_result = !abstract_text.is_empty() || !answer.is_empty() || !related_topics.is_empty();

        Ok(serde_json::json!({
            "query": query,
            "has_result": has_result,
            "result_type": result_type,
            "answer": answer,
            "heading": heading,
            "abstract": abstract_text,
            "abstract_url": abstract_url,
            "abstract_source": abstract_source,
            "related_topics": related_topics,
            "note": "This is an instant answer, not a full web search. For ranked web results, see Phase 2 (HTML scraper)."
        }))
    }
}
```

**Dependency to add in `crates/agentos-tools/Cargo.toml`:**
```toml
urlencoding = "2"
```
Note: `reqwest` with `json` feature is already present. `urlencoding` is a tiny crate (no transitive deps).

### 2. Create `tools/core/web-search.toml`

```toml
[manifest]
name        = "web-search"
version     = "1.0.0"
description = "Search the web using DuckDuckGo Instant Answers. Returns structured JSON with direct answers, Wikipedia summaries, and related topic URLs. Use web-fetch on the returned URLs for full page content."
author      = "agentos-core"
trust_tier  = "core"

[capabilities_required]
permissions = ["network.outbound:x"]

[capabilities_provided]
outputs = ["search.results"]

[intent_schema]
input  = "WebSearchIntent"
output = "WebSearchResult"

[input_schema]
type     = "object"
required = ["query"]

[input_schema.properties.query]
type        = "string"
description = "The search query string"
maxLength   = 500

[sandbox]
network       = true
fs_write      = false
gpu           = false
max_memory_mb = 32
max_cpu_ms    = 15000
syscalls      = []
```

### 3. Register in `crates/agentos-tools/src/factory.rs`

Find the `build_core_tools` function (or equivalent registration point) and add:

```rust
// web-search — DDG Instant Answer API
tools.push(Arc::new(WebSearch::new()?));
```

Import at top of `factory.rs`:
```rust
use crate::web_search::WebSearch;
```

### 4. Export from `crates/agentos-tools/src/lib.rs`

```rust
pub mod web_search;
pub use web_search::WebSearch;
```

### 5. Grant permission in default agent manifest

In `crates/agentos-kernel/src/core_manifests.rs`, find the default agent capability grants and add `network.outbound:x` if not already present (it should be — `web-fetch` already has it).

---

## Files Changed

| File | Change |
|---|---|
| `crates/agentos-tools/src/web_search.rs` | **New** — `WebSearch` tool implementation |
| `crates/agentos-tools/src/lib.rs` | Add `pub mod web_search; pub use web_search::WebSearch;` |
| `crates/agentos-tools/src/factory.rs` | Register `WebSearch` in tool factory |
| `crates/agentos-tools/Cargo.toml` | Add `urlencoding = "2"` |
| `tools/core/web-search.toml` | **New** — tool manifest |

---

## Dependencies

- **Phase 0 (none)** — no prior phases needed
- `reqwest` with `json` feature — already in `agentos-tools/Cargo.toml`
- `urlencoding` crate — tiny, no transitive deps

---

## Test Plan

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::*;

    fn ctx_no_perms() -> ToolExecutionContext { /* ... */ }
    fn ctx_with_network() -> ToolExecutionContext { /* ... */ }

    #[tokio::test]
    async fn web_search_requires_query_field() {
        let tool = WebSearch::new().unwrap();
        let result = tool.execute(serde_json::json!({}), ctx_with_network()).await;
        assert!(matches!(result, Err(AgentOSError::SchemaValidation(_))));
    }

    #[tokio::test]
    async fn web_search_rejects_empty_query() {
        let tool = WebSearch::new().unwrap();
        let result = tool.execute(serde_json::json!({"query": ""}), ctx_with_network()).await;
        assert!(matches!(result, Err(AgentOSError::SchemaValidation(_))));
    }

    #[tokio::test]
    async fn web_search_requires_network_permission() {
        let tool = WebSearch::new().unwrap();
        let result = tool.execute(serde_json::json!({"query": "rust"}), ctx_no_perms()).await;
        assert!(matches!(result, Err(AgentOSError::PermissionDenied { .. })));
    }

    // Live test (skipped in CI — requires network)
    #[tokio::test]
    #[ignore]
    async fn web_search_returns_structured_result() {
        let tool = WebSearch::new().unwrap();
        let result = tool.execute(
            serde_json::json!({"query": "Rust programming language"}),
            ctx_with_network(),
        ).await.unwrap();
        assert_eq!(result["query"], "Rust programming language");
        assert!(result["has_result"].as_bool().unwrap());
        assert!(result.get("abstract").is_some());
    }
}
```

---

## Verification

```bash
# Build passes
cargo build -p agentos-tools

# Tests pass (unit tests only, no network)
cargo test -p agentos-tools web_search

# Live test (requires network, skipped in CI)
cargo test -p agentos-tools web_search -- --ignored

# Clippy clean
cargo clippy -p agentos-tools -- -D warnings

# Format check
cargo fmt -p agentos-tools -- --check
```

---

## Related

- [[Agent Web Search Plan]] — master plan
- [[Agent Web Search Research]] — research backing this decision
- [[02-html-search-scraper]] — Phase 2 (full results)
