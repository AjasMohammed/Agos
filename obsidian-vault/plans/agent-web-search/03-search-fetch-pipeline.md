---
title: "Phase 3: Search→Fetch Pipeline"
tags:
  - tools
  - search
  - pipeline
  - agentic-workflow
  - phase-3
date: 2026-03-25
status: planned
effort: 1d
priority: high
---

# Phase 3: Search→Fetch Pipeline

> Formalize the two-phase search→fetch accuracy pattern: agent guidance in system prompt + `agent-manual`, optional `deep_fetch` shortcut in `web-search`, and semantic memory caching of fetched page content.

---

## Why This Phase

After Phases 1 and 2, agents *can* call `web-search` and `web-fetch` — but without guidance, they will likely inject search snippets directly into context and reason on them as if they were accurate. Research (see [[Agent Web Search Research]]) confirms this causes semantic drift and hallucination spirals.

This phase closes the gap between capability and correct behavior:
1. System prompt instructions teach agents the two-phase pattern
2. `agent-manual` documents it for agent self-reference
3. An optional `deep_fetch` shortcut in `web-search` automates the pattern for simple cases
4. Fetched page content is written to Semantic memory for cross-task reuse

---

## Current → Target State

**Current:** `web-search` and `web-fetch` exist as independent tools. No agent guidance on when to use them together.

**Target:**
- System prompt includes a standing instruction about the search→fetch pattern
- `agent-manual` has a `web-search` section with usage examples
- `web-search` accepts `deep_fetch: true` to automatically fetch the top N results and return full page content alongside snippets
- Fetched content can be written to Semantic memory via the `memory-write` tool (documented in agent-manual, not automated)

---

## Detailed Subtasks

### 1. Add search→fetch guidance to system prompt

In `crates/agentos-kernel/src/core_manifests.rs` (or wherever the base system prompt is constructed), add a standing instruction block:

```
## Web Research Pattern

When researching a topic using web search:
1. Call web-search with mode="web" to get a ranked list of URLs and snippets.
2. Snippets are short and may be inaccurate — do NOT treat them as ground truth.
3. Call web-fetch on the top 2-3 most relevant URLs to retrieve full page content.
4. Reason only on the fetched content, not the snippets.
5. Cite the source URL when stating facts derived from web content.

Example workflow:
  web-search(query="tokio rust async runtime") → [{rank:1, url:"https://tokio.rs", snippet:"..."}]
  web-fetch(url="https://tokio.rs") → {content: "Tokio is an asynchronous runtime..."}
  → Use the fetched content to answer the question.
```

Find the existing system prompt location — likely in `core_manifests.rs` or injected in `task_executor.rs` as a system context entry.

### 2. Add `web-search` section to `agent-manual` tool content

In `crates/agentos-tools/src/agent_manual.rs`, locate the section map and add an entry for web search:

```rust
("web-search", r#"
## web-search

Search the web for information. Returns ranked results (URLs + snippets) or instant answers.

### Modes
- `mode: "web"` (default) — ranked web results from DuckDuckGo. Returns title, URL, snippet.
- `mode: "instant"` — DDG Instant Answers (Wikipedia summaries, computed answers). Stable.
- `mode: "both"` — both ranked results and instant answer.

### Important: Two-Phase Pattern
Snippets in search results are short extracts — they are a discovery tool, not a source of truth.
Always use web-fetch on the relevant URLs before reasoning on the content.

### Example
```json
// Step 1: Discover URLs
{"tool": "web-search", "input": {"query": "rust tokio tutorial", "mode": "web", "max_results": 5}}

// Step 2: Fetch actual content
{"tool": "web-fetch", "input": {"url": "https://tokio.rs/tokio/tutorial"}}

// Step 3: Reason on fetched content (not snippets)
```

### Permissions required
`network.outbound:x`
"#),
```

### 3. Add `deep_fetch` shortcut to `web-search`

This is a convenience field that runs the search→fetch pipeline in a single tool call for simple cases (saves 2-3 round trips in the agent loop):

In `tools/core/web-search.toml`, add to input schema:
```toml
[input_schema.properties.deep_fetch]
type        = "boolean"
default     = false
description = "When true, automatically web-fetch the top N results and include full page content. Slower but more accurate. Use for research tasks."

[input_schema.properties.deep_fetch_count]
type        = "integer"
default     = 3
minimum     = 1
maximum     = 5
description = "Number of top results to deep-fetch when deep_fetch=true"
```

In `web_search.rs` `execute()`, after fetching web results:

```rust
let deep_fetch = payload.get("deep_fetch").and_then(|v| v.as_bool()).unwrap_or(false);
let deep_fetch_count = payload.get("deep_fetch_count").and_then(|v| v.as_u64()).unwrap_or(3).min(5) as usize;

if deep_fetch && !web_results.is_empty() {
    let urls: Vec<String> = web_results.iter()
        .take(deep_fetch_count)
        .filter_map(|r| r.get("url")?.as_str().map(String::from))
        .collect();

    // Reuse WebFetch internally — call reqwest directly to avoid permission re-check
    let mut fetched = Vec::new();
    for url in &urls {
        match self.fetch_page_content(url, &context.cancellation_token).await {
            Ok(content) => fetched.push(serde_json::json!({
                "url": url,
                "content": content,
            })),
            Err(e) => fetched.push(serde_json::json!({
                "url": url,
                "error": e.to_string(),
            })),
        }
    }
    // Add fetched_content array to output
    result["fetched_content"] = serde_json::Value::Array(fetched);
}
```

Add private `fetch_page_content()` method using `html2text::from_read()` (already a transitive dep via `web-fetch`). Limit to 8000 chars per page when doing deep_fetch (total budget: ~40k for 5 pages).

### 4. Update `tools/core/web-search.toml` description

Update description to reflect the new capabilities:

```toml
description = "Search the web using DuckDuckGo. Returns ranked results (URLs + snippets) for web mode, or instant answers for factual queries. Set deep_fetch=true to automatically fetch top result pages for accurate content. Always use web-fetch on returned URLs before treating snippets as factual."
```

### 5. Sandbox memory budget update

`deep_fetch=true` with 5 results can use up to ~5×64MB for page fetching. Update `web-search.toml` sandbox:

```toml
max_memory_mb = 128   # raised from 32 to accommodate deep_fetch
max_cpu_ms    = 60000 # raised to 60s for deep_fetch (5 pages × 10s each)
```

---

## Files Changed

| File | Change |
|---|---|
| `crates/agentos-kernel/src/core_manifests.rs` | Add web research standing instruction to system prompt |
| `crates/agentos-tools/src/agent_manual.rs` | Add `web-search` section with two-phase pattern docs |
| `crates/agentos-tools/src/web_search.rs` | Add `deep_fetch` support, `fetch_page_content()` method |
| `tools/core/web-search.toml` | Add `deep_fetch`, `deep_fetch_count` to schema; update description; raise sandbox limits |

---

## Dependencies

- **Phase 1 must be complete** (DDG Instant Answer tool)
- **Phase 2 must be complete** (HTML scraper providing ranked results)
- `html2text` — check if already in `agentos-tools/Cargo.toml` (it is in `web_fetch.rs`)

---

## Test Plan

```rust
#[tokio::test]
async fn web_search_deep_fetch_false_by_default() {
    // Verify that without deep_fetch=true, no fetched_content field is present
    // Mock web results are returned without page fetching
}

// Live tests (ignored in CI):
#[tokio::test]
#[ignore]
async fn web_search_deep_fetch_includes_page_content() {
    let tool = WebSearch::new().unwrap();
    let result = tool.execute(
        serde_json::json!({
            "query": "tokio rust",
            "mode": "web",
            "deep_fetch": true,
            "deep_fetch_count": 2
        }),
        ctx_with_network(),
    ).await.unwrap();
    let fetched = result["fetched_content"].as_array().unwrap();
    assert_eq!(fetched.len(), 2);
    assert!(fetched[0].get("content").is_some() || fetched[0].get("error").is_some());
}
```

---

## Verification

```bash
cargo build -p agentos-tools -p agentos-kernel
cargo test -p agentos-tools web_search
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check

# Manually verify system prompt contains search guidance:
grep -r "Web Research Pattern" crates/agentos-kernel/src/
```

---

## Related

- [[Agent Web Search Plan]] — master plan
- [[Agent Web Search Research]] — confirms two-phase pattern is correct
- [[02-html-search-scraper]] — Phase 2 (must be complete)
- [[04-searxng-integration]] — Phase 4 (replaces HTML scraper with stable source)
