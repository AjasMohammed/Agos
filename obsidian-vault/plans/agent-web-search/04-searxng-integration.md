---
title: "Phase 4: SearXNG Companion Service"
tags:
  - tools
  - search
  - searxng
  - self-hosted
  - phase-4
date: 2026-03-25
status: planned
effort: 2d
priority: medium
---

# Phase 4: SearXNG Companion Service

> Replace fragile HTML scraping with a self-hosted SearXNG instance as a companion service. Adds a stable JSON API, multi-engine aggregation, and internal SSRF allowlisting for the SearXNG endpoint.

---

## Why This Phase

Phase 2's HTML scraper works but is fragile — any DDG layout change breaks CSS selectors silently. SearXNG solves this:

- **Multi-engine**: aggregates Google, Bing, DDG, Brave, and more
- **Stable JSON API**: `GET /search?q=...&format=json` — layout changes in upstream engines are handled by SearXNG's maintained parsers
- **Self-hosted**: no API key, no account, no external service dependency
- **Privacy-respecting**: no tracking, no user profiles
- **Maintained**: active open-source project with frequent updates

This phase adds SearXNG as an optional companion service. When configured, `web-search` uses SearXNG instead of direct DDG scraping. When not configured, it falls back to Phase 2's scraper. This makes SearXNG opt-in, not required.

---

## Current → Target State

**Current (after Phase 3):** `web-search` scrapes DDG HTML directly. Fragile, single-engine.

**Target:** `web-search` checks config for a SearXNG endpoint. If configured, it sends `GET {endpoint}/search?q={query}&format=json` and parses the stable JSON response. If not configured, falls back to DDG HTML scraper.

---

## SearXNG Setup (User Responsibility)

Users run SearXNG as a Docker container or systemd service. Recommended:

```bash
# Docker Compose (simplest)
docker run -d \
  --name searxng \
  -p 127.0.0.1:8080:8080 \
  -v /etc/searxng:/etc/searxng \
  -e SEARXNG_SECRET=<random-string> \
  searxng/searxng:latest
```

Then configure in AgentOS: `search.searxng_url = "http://localhost:8080"`.

SearXNG resource footprint: ~80-150MB RAM, <1% CPU idle, Python/Flask. On a system already running the AgentOS kernel (~50MB Rust binary), total is ~200-250MB — acceptable.

---

## SearXNG JSON API Response Shape

```
GET http://localhost:8080/search?q={query}&format=json&language=en&safesearch=1
```

Response:
```json
{
  "query": "rust async runtime",
  "number_of_results": 42000,
  "results": [
    {
      "url": "https://tokio.rs",
      "title": "Tokio — An asynchronous Rust runtime",
      "content": "Tokio is an asynchronous runtime for the Rust programming language...",
      "engine": "google",
      "score": 1.0,
      "category": "general"
    }
  ],
  "answers": [],
  "corrections": [],
  "infoboxes": [...]
}
```

---

## Detailed Subtasks

### 1. Add `[search]` section to `config/default.toml`

```toml
[search]
# Optional: URL of a self-hosted SearXNG instance.
# When set, web-search uses SearXNG instead of direct DDG scraping.
# Leave empty to use the built-in DDG HTML scraper (Phase 2 fallback).
# Example: "http://localhost:8080"
searxng_url = ""

# Maximum results to return from SearXNG (1-25)
max_results = 10

# SearXNG safe search level: 0=off, 1=moderate, 2=strict
safesearch = 1
```

### 2. Add `SearchConfig` to kernel config struct

In `crates/agentos-kernel/src/config.rs`:

```rust
#[derive(Debug, Clone, Deserialize, Default)]
pub struct SearchConfig {
    #[serde(default)]
    pub searxng_url: String,
    #[serde(default = "default_max_results")]
    pub max_results: usize,
    #[serde(default = "default_safesearch")]
    pub safesearch: u8,
}

fn default_max_results() -> usize { 10 }
fn default_safesearch() -> u8 { 1 }
```

Add to `KernelConfig`:
```rust
#[serde(default)]
pub search: SearchConfig,
```

### 3. Thread `SearchConfig` to `ToolExecutionContext`

`ToolExecutionContext` needs to carry the SearXNG URL so `WebSearch` can read it. Options:

**Option A (simple):** Add `searxng_url: Option<String>` to `ToolExecutionContext`.
**Option B (cleaner):** Pass it via a shared `KernelConfig` Arc already available in context.

Use Option A for Phase 4 — minimal change, no new abstraction:

In `agentos-types/src/context.rs`:
```rust
pub searxng_url: Option<String>,   // None = use fallback scraper
```

In `task_executor.rs` when building `ToolExecutionContext`, populate from kernel config.

### 4. Add SSRF allowlist for SearXNG endpoint

The existing SSRF protection blocks private IPs and `localhost`. SearXNG typically runs on `localhost:8080`. We need a targeted allowlist — not a blanket SSRF bypass.

In `PermissionSet`, the `check()` method already supports allowlisting (confirm in `crates/agentos-capability/src/lib.rs`). Add a `network.search` permission type:

```rust
// Permission: "network.search" — allows outbound to the configured SearXNG endpoint only
// Granted as part of default agent manifest when searxng_url is configured
```

In `web_search.rs`, when SearXNG mode is active, check `network.search` instead of `network.outbound`:

```rust
if searxng_url.is_some() {
    // Use network.search permission (scoped to SearXNG endpoint)
    if !context.permissions.check("network.search", PermissionOp::Execute) {
        return Err(AgentOSError::PermissionDenied { ... });
    }
} else {
    // Use network.outbound (for DDG — public internet)
    if !context.permissions.check("network.outbound", PermissionOp::Execute) {
        return Err(AgentOSError::PermissionDenied { ... });
    }
}
```

Alternatively (simpler for Phase 4): add the SearXNG URL to the `PermissionSet` allowlist so that `network.outbound` permits it explicitly. This avoids a new permission type.

In `crates/agentos-kernel/src/core_manifests.rs`, when building default agent permissions, if `searxng_url` is configured, add it as an allowed outbound target:

```rust
if let Some(url) = &config.search.searxng_url {
    permission_set.grant_with_allowlist("network.outbound", vec![url.clone()]);
}
```

This is the simpler path — no new permission type, just allowlist scoping.

### 5. Implement SearXNG fetch in `web_search.rs`

Add `fetch_searxng_results()`:

```rust
async fn fetch_searxng_results(
    &self,
    query: &str,
    searxng_url: &str,
    max_results: usize,
    safesearch: u8,
    cancellation_token: &tokio_util::sync::CancellationToken,
) -> Result<Vec<serde_json::Value>, AgentOSError> {
    let url = format!(
        "{}/search?q={}&format=json&language=en&safesearch={}",
        searxng_url.trim_end_matches('/'),
        urlencoding::encode(query),
        safesearch,
    );

    let response = tokio::select! {
        result = self.client.get(&url).send() => {
            result.map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "web-search".into(),
                reason: format!("SearXNG request failed: {}", e),
            })?
        }
        _ = cancellation_token.cancelled() => {
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: "web-search".into(),
                reason: "Tool execution cancelled".into(),
            });
        }
    };

    let body: serde_json::Value = response.json().await.map_err(|e| AgentOSError::ToolExecutionFailed {
        tool_name: "web-search".into(),
        reason: format!("Failed to parse SearXNG response: {}", e),
    })?;

    let results = body
        .get("results")
        .and_then(|v| v.as_array())
        .unwrap_or(&vec![])
        .iter()
        .enumerate()
        .take(max_results)
        .filter_map(|(i, r)| {
            let url = r.get("url")?.as_str()?;
            let title = r.get("title")?.as_str().unwrap_or("");
            let snippet = r.get("content").or_else(|| r.get("snippet"))
                .and_then(|v| v.as_str()).unwrap_or("");
            let engine = r.get("engine").and_then(|v| v.as_str()).unwrap_or("unknown");
            Some(serde_json::json!({
                "rank": i + 1,
                "title": title,
                "url": url,
                "snippet": snippet,
                "source_engine": engine,
            }))
        })
        .collect();

    Ok(results)
}
```

Update `execute()` dispatch:
```rust
let web_results = if let Some(ref searxng_url) = context.searxng_url {
    self.fetch_searxng_results(query, searxng_url, max_results, safesearch, &context.cancellation_token).await?
} else {
    self.fetch_web_results(query, max_results, &context.cancellation_token).await?
};
```

Add `source` field to output JSON indicating which backend was used:
```json
{ "source": "searxng" }   // or "duckduckgo_html"
```

### 6. Update `tools/core/web-search.toml` description

```toml
description = "Search the web and return ranked results. Uses SearXNG (multi-engine, stable) when configured, or DuckDuckGo HTML scraper as fallback. Returns ranked URLs + snippets. Always web-fetch the relevant URLs for accurate content — snippets are a discovery signal only."
```

### 7. Add health check for SearXNG on kernel boot

In `crates/agentos-kernel/src/health_monitor.rs`, add a SearXNG connectivity check on boot if `searxng_url` is configured:

```rust
if !config.search.searxng_url.is_empty() {
    let url = format!("{}/healthz", config.search.searxng_url);
    match reqwest::get(&url).await {
        Ok(r) if r.status().is_success() => {
            tracing::info!("SearXNG health check passed: {}", config.search.searxng_url);
        }
        _ => {
            tracing::warn!("SearXNG health check failed — web-search will fall back to DDG HTML scraper");
        }
    }
}
```

---

## Files Changed

| File | Change |
|---|---|
| `config/default.toml` | Add `[search]` section with `searxng_url`, `max_results`, `safesearch` |
| `crates/agentos-kernel/src/config.rs` | Add `SearchConfig` struct, add `search: SearchConfig` to `KernelConfig` |
| `crates/agentos-types/src/context.rs` | Add `searxng_url: Option<String>` to `ToolExecutionContext` |
| `crates/agentos-kernel/src/task_executor.rs` | Populate `searxng_url` in context from config |
| `crates/agentos-kernel/src/core_manifests.rs` | Grant SearXNG allowlist to `network.outbound` when configured |
| `crates/agentos-tools/src/web_search.rs` | Add `fetch_searxng_results()`, update dispatch logic, add `source` field |
| `tools/core/web-search.toml` | Update description |
| `crates/agentos-kernel/src/health_monitor.rs` | Add SearXNG boot health check |

---

## Dependencies

- **Phase 3 must be complete** (search→fetch pipeline)
- User must have SearXNG running (documented setup, not automated by AgentOS)
- No new Rust dependencies — `reqwest` already handles JSON

---

## Test Plan

```rust
#[tokio::test]
async fn web_search_falls_back_to_ddg_when_no_searxng_url() {
    // context.searxng_url = None → uses DDG HTML scraper
    // Verify source field = "duckduckgo_html"
}

#[tokio::test]
async fn searxng_url_missing_scheme_handled() {
    // Misconfigured searxng_url (no http://) → informative error
}

// Live test (requires running SearXNG on localhost:8080):
#[tokio::test]
#[ignore]
async fn web_search_uses_searxng_when_configured() {
    let tool = WebSearch::new().unwrap();
    let mut ctx = ctx_with_network();
    ctx.searxng_url = Some("http://localhost:8080".to_string());
    let result = tool.execute(
        serde_json::json!({"query": "rust programming", "mode": "web"}),
        ctx,
    ).await.unwrap();
    assert_eq!(result["source"], "searxng");
    assert!(!result["results"].as_array().unwrap().is_empty());
}
```

---

## Verification

```bash
# Build
cargo build --workspace

# Tests (unit only)
cargo test --workspace

# Clippy + fmt
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check

# Manual: start SearXNG and verify
docker run -d -p 127.0.0.1:8080:8080 searxng/searxng:latest
curl "http://localhost:8080/search?q=rust&format=json" | jq '.results[0]'

# Then configure AgentOS and test:
# In config/default.toml: searxng_url = "http://localhost:8080"
# Start kernel and have an agent call web-search
```

---

## Related

- [[Agent Web Search Plan]] — master plan
- [[Agent Web Search Research]] — SearXNG chosen as production target
- [[03-search-fetch-pipeline]] — Phase 3 (prerequisite)
