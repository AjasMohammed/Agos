---
title: "Phase 2: HTML Search Scraper"
tags:
  - tools
  - search
  - network
  - phase-2
date: 2026-03-25
status: planned
effort: 1.5d
priority: high
---

# Phase 2: HTML Search Scraper

> Extend `web-search` to scrape DuckDuckGo's HTML lite endpoint (`html.duckduckgo.com/html/`) for a full ranked list of web results — title, URL, snippet, rank. Adds the `scraper` crate for CSS-selector-based HTML parsing.

---

## Why This Phase

The DDG Instant Answer API (Phase 1) only returns factual/Wikipedia-style answers — not a ranked list of web URLs. For research tasks, agents need actual web results: the top 10 URLs with titles and snippets so they can decide which pages to `web-fetch`.

DDG's HTML lite endpoint (`html.duckduckgo.com/html/`) is designed for low-bandwidth clients and is significantly more scraper-tolerant than their main site. It has been stable for years and does not require JavaScript. The `scraper` crate (pure Rust, CSS selectors) parses it in ~1-2ms.

This phase makes `web-search` return a ranked result list. Phase 1's instant answer output is preserved and returned alongside the ranked results when both are present.

---

## Current → Target State

**Current (after Phase 1):** `web-search` returns instant answers only — no ranked URL list.

**Target:** `web-search` accepts a `mode` field (`"instant"` | `"web"` | `"both"`, default `"web"`) and returns:
```json
{
  "query": "rust async runtime",
  "mode": "web",
  "results": [
    {
      "rank": 1,
      "title": "Tokio — async runtime for Rust",
      "url": "https://tokio.rs",
      "snippet": "Tokio is an asynchronous runtime for the Rust programming language...",
      "source_engine": "duckduckgo"
    }
  ],
  "result_count": 10,
  "instant_answer": { ... }   // present when mode="both" or when DDG returns one
}
```

---

## DDG HTML Lite Endpoint

```
GET https://html.duckduckgo.com/html/?q={encoded_query}
Headers:
  User-Agent: AgentOS/1.0
  Accept-Language: en-US,en;q=0.9
```

Result elements use these CSS selectors (stable as of 2025-2026):

| Field | CSS Selector |
|---|---|
| Result container | `.result` |
| Title | `.result__title a` or `.result__a` |
| URL | `.result__url` (text) or `.result__a[href]` |
| Snippet | `.result__snippet` |

The `.result__a[href]` attribute contains a DDG redirect URL — extract the actual URL from the `uddg` query parameter or use the `.result__url` text directly.

---

## Detailed Subtasks

### 1. Add `scraper` dependency to `crates/agentos-tools/Cargo.toml`

```toml
scraper = "0.20"
```

`scraper` is pure Rust, builds to ~200KB, zero network deps, and uses `cssparser` + `html5ever` under the hood.

### 2. Add `mode` field to `web-search` input schema

In `tools/core/web-search.toml`, add to `[input_schema]`:

```toml
[input_schema.properties.mode]
type        = "string"
enum        = ["instant", "web", "both"]
default     = "web"
description = "Search mode. 'instant' returns DDG Instant Answers only (stable, factual). 'web' returns ranked web results (scraped, may break on layout changes). 'both' returns both."

[input_schema.properties.max_results]
type        = "integer"
default     = 10
minimum     = 1
maximum     = 25
description = "Maximum number of ranked results to return (web mode only)"
```

### 3. Implement HTML scraping in `crates/agentos-tools/src/web_search.rs`

Add a private method `fetch_web_results`:

```rust
use scraper::{Html, Selector};

impl WebSearch {
    async fn fetch_web_results(
        &self,
        query: &str,
        max_results: usize,
        cancellation_token: &tokio_util::sync::CancellationToken,
    ) -> Result<Vec<serde_json::Value>, AgentOSError> {
        let url = format!(
            "https://html.duckduckgo.com/html/?q={}",
            urlencoding::encode(query)
        );

        let response = tokio::select! {
            result = self.client
                .get(&url)
                .header("Accept-Language", "en-US,en;q=0.9")
                .send() => {
                result.map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "web-search".into(),
                    reason: format!("HTML search request failed: {}", e),
                })?
            }
            _ = cancellation_token.cancelled() => {
                return Err(AgentOSError::ToolExecutionFailed {
                    tool_name: "web-search".into(),
                    reason: "Tool execution cancelled".into(),
                });
            }
        };

        if !response.status().is_success() {
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: "web-search".into(),
                reason: format!("DDG returned HTTP {}", response.status()),
            });
        }

        let html_text = response.text().await.map_err(|e| AgentOSError::ToolExecutionFailed {
            tool_name: "web-search".into(),
            reason: format!("Failed to read response body: {}", e),
        })?;

        let document = Html::parse_document(&html_text);

        // CSS selectors — validated against DDG HTML lite (2025-2026)
        let result_sel   = Selector::parse(".result").unwrap();
        let title_sel    = Selector::parse(".result__title a, .result__a").unwrap();
        let url_sel      = Selector::parse(".result__url").unwrap();
        let snippet_sel  = Selector::parse(".result__snippet").unwrap();

        let mut results = Vec::new();
        for (rank, result_el) in document.select(&result_sel).enumerate() {
            if rank >= max_results { break; }

            let title = result_el
                .select(&title_sel)
                .next()
                .map(|el| el.text().collect::<String>().trim().to_string())
                .unwrap_or_default();

            let raw_url = result_el
                .select(&url_sel)
                .next()
                .map(|el| el.text().collect::<String>().trim().to_string())
                .unwrap_or_default();

            let snippet = result_el
                .select(&snippet_sel)
                .next()
                .map(|el| el.text().collect::<String>().trim().to_string())
                .unwrap_or_default();

            if title.is_empty() || raw_url.is_empty() { continue; }

            // Normalize URL: DDG text URLs may omit scheme
            let url = if raw_url.starts_with("http") {
                raw_url
            } else {
                format!("https://{}", raw_url)
            };

            results.push(serde_json::json!({
                "rank": rank + 1,
                "title": title,
                "url": url,
                "snippet": snippet,
                "source_engine": "duckduckgo",
            }));
        }

        Ok(results)
    }
}
```

Update `execute()` to dispatch based on `mode`:

```rust
let mode = payload.get("mode").and_then(|v| v.as_str()).unwrap_or("web");
let max_results = payload.get("max_results").and_then(|v| v.as_u64()).unwrap_or(10).min(25) as usize;

let (instant, web_results) = match mode {
    "instant" => (Some(self.fetch_instant_answer(query, &context.cancellation_token).await?), vec![]),
    "web"     => (None, self.fetch_web_results(query, max_results, &context.cancellation_token).await?),
    "both"    => {
        let (i, w) = tokio::join!(
            self.fetch_instant_answer(query, &context.cancellation_token),
            self.fetch_web_results(query, max_results, &context.cancellation_token),
        );
        (Some(i?), w?)
    }
    _ => return Err(AgentOSError::SchemaValidation(format!("Unknown mode: {}", mode))),
};
```

Note: Refactor Phase 1's `execute()` body into a private `fetch_instant_answer()` method to enable the `tokio::join!` pattern above.

### 4. Rate limiting: respect a minimum delay

DDG's HTML endpoint may return a CAPTCHA page if requests come too fast. Add a configurable minimum inter-request delay via a `tokio::time::sleep` guard in the tool factory (not in `execute()` to avoid blocking the runtime). For now, a single `500ms` sleep before the HTML request is sufficient for agentic use:

```rust
// In fetch_web_results, before sending request:
tokio::time::sleep(std::time::Duration::from_millis(500)).await;
```

This is intentionally conservative. SearXNG (Phase 4) eliminates this limitation.

### 5. Handle CAPTCHA / blocked responses gracefully

If DDG returns a page with no `.result` elements, return an informative error rather than an empty list:

```rust
if results.is_empty() && !html_text.contains("result") {
    return Err(AgentOSError::ToolExecutionFailed {
        tool_name: "web-search".into(),
        reason: "No results found — DDG may have rate-limited this request. Wait before retrying.".into(),
    });
}
```

---

## Files Changed

| File | Change |
|---|---|
| `crates/agentos-tools/src/web_search.rs` | Add `fetch_web_results()`, refactor `execute()` with mode dispatch, add rate-limit sleep |
| `crates/agentos-tools/Cargo.toml` | Add `scraper = "0.20"` |
| `tools/core/web-search.toml` | Add `mode` and `max_results` to input schema |

---

## Dependencies

- **Phase 1 must be complete** — this phase extends Phase 1's `web_search.rs`
- `scraper = "0.20"` — new dependency (pure Rust, no C deps)

---

## Test Plan

```rust
#[tokio::test]
async fn web_search_rejects_unknown_mode() {
    let tool = WebSearch::new().unwrap();
    let result = tool.execute(
        serde_json::json!({"query": "rust", "mode": "invalid"}),
        ctx_with_network(),
    ).await;
    assert!(matches!(result, Err(AgentOSError::SchemaValidation(_))));
}

#[tokio::test]
async fn web_search_caps_max_results_at_25() {
    // Verify that max_results=100 is capped to 25 without error
    let tool = WebSearch::new().unwrap();
    // Mock test — verify capping logic without real network call
    // Use AGENTOS_TEST_ALLOW_LOCAL pattern or mock HTTP server
}

// Live tests (ignored in CI):
#[tokio::test]
#[ignore]
async fn web_search_web_mode_returns_ranked_results() {
    let tool = WebSearch::new().unwrap();
    let result = tool.execute(
        serde_json::json!({"query": "tokio rust async", "mode": "web"}),
        ctx_with_network(),
    ).await.unwrap();
    let results = result["results"].as_array().unwrap();
    assert!(!results.is_empty());
    assert_eq!(results[0]["rank"], 1);
    assert!(results[0]["url"].as_str().unwrap().starts_with("http"));
    assert!(!results[0]["title"].as_str().unwrap().is_empty());
}
```

---

## Verification

```bash
cargo build -p agentos-tools
cargo test -p agentos-tools web_search
cargo clippy -p agentos-tools -- -D warnings
cargo fmt -p agentos-tools -- --check

# Live web search test (requires network)
cargo test -p agentos-tools web_search -- --ignored
```

---

## Related

- [[Agent Web Search Plan]] — master plan
- [[01-ddg-instant-answer-tool]] — Phase 1 (must be complete first)
- [[03-search-fetch-pipeline]] — Phase 3 (uses this tool's results)
