---
title: "Phase 2: Wikilink Parser & Backlink Index"
tags:
  - memory
  - scratchpad
  - v3
  - plan
date: 2026-03-23
status: complete
effort: 1.5d
priority: high
---

# Phase 2: Wikilink Parser & Backlink Index

> Parse `[[wikilinks]]` from markdown content on write, maintain an adjacency table for instant backlink queries.

---

## Why This Phase

Wikilinks are what turn a flat collection of pages into a knowledge graph. Without them, the scratchpad is just a better MemoryBlockStore. The backlink index enables:
- **Associative discovery** — "what else links to this concept?"
- **Graph traversal** — Phase 4 depends on this for context injection
- **Orphan detection** — find pages with no inbound links (potentially stale)

Eager indexing (parse on write) means backlink queries are O(1) lookups. The alternative — scanning all pages on every backlink query — doesn't scale.

---

## Current → Target State

**Current (after Phase 1):** `ScratchpadStore` with pages table and FTS5. No link awareness.

**Target:**
- `link_index` table in SQLite — adjacency list of (source_page, target_title)
- `parse_wikilinks(content) -> Vec<String>` function
- `write_page()` updated to parse links and update `link_index` atomically
- `get_backlinks(title)` — pages that link TO this page
- `get_outlinks(title)` — pages this page links TO
- `get_orphans()` — pages with no inbound links
- Support for aliased links: `[[Page Title|display text]]`
- Support for cross-agent references: `[[Page Title]]` (same agent) vs `[[@agent_id/Page Title]]` (cross-agent, Phase 5)

---

## Detailed Subtasks

### 1. Add `links.rs` module

New file: `crates/agentos-scratch/src/links.rs`

```rust
use regex::Regex;
use std::sync::LazyLock;

static WIKILINK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[\[([^\]\|]+)(?:\|[^\]]+)?\]\]").unwrap()
});

/// Parsed wikilink with target title and optional display text
#[derive(Debug, Clone, PartialEq)]
pub struct WikiLink {
    pub target: String,       // The page title being linked to
    pub display: Option<String>, // Optional alias text after |
    pub is_cross_agent: bool, // Starts with @
    pub agent_id: Option<String>, // If cross-agent, the target agent
}

/// Parse all [[wikilinks]] from markdown content
pub fn parse_wikilinks(content: &str) -> Vec<WikiLink> {
    WIKILINK_RE.captures_iter(content)
        .map(|cap| {
            let raw = cap[1].trim().to_string();
            let display = cap.get(2).map(|m| m.as_str().trim().to_string());

            if let Some(stripped) = raw.strip_prefix('@') {
                if let Some((agent_id, title)) = stripped.split_once('/') {
                    WikiLink {
                        target: title.to_string(),
                        display,
                        is_cross_agent: true,
                        agent_id: Some(agent_id.to_string()),
                    }
                } else {
                    WikiLink { target: raw, display, is_cross_agent: false, agent_id: None }
                }
            } else {
                WikiLink { target: raw, display, is_cross_agent: false, agent_id: None }
            }
        })
        .collect()
}
```

### 2. Add `link_index` table to schema

In `store.rs`, extend `init_schema()`:

```sql
CREATE TABLE IF NOT EXISTS link_index (
    source_id TEXT NOT NULL,
    target_title TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    link_text TEXT NOT NULL,
    is_cross_agent INTEGER NOT NULL DEFAULT 0,
    target_agent_id TEXT,
    UNIQUE(source_id, target_title, agent_id)
);

CREATE INDEX IF NOT EXISTS idx_links_target ON link_index(agent_id, target_title);
CREATE INDEX IF NOT EXISTS idx_links_source ON link_index(agent_id, source_id);
```

### 3. Update `write_page()` to parse and index links

After inserting/updating the page, within the same transaction:

```rust
// 1. Delete existing outbound links for this page
tx.execute("DELETE FROM link_index WHERE source_id = ?", [&page_id])?;

// 2. Parse wikilinks from new content
let links = parse_wikilinks(&content);

// 3. Insert new outbound links
let mut stmt = tx.prepare(
    "INSERT OR IGNORE INTO link_index (source_id, target_title, agent_id, link_text, is_cross_agent, target_agent_id)
     VALUES (?, ?, ?, ?, ?, ?)"
)?;
for link in &links {
    let effective_agent = link.agent_id.as_deref().unwrap_or(agent_id);
    stmt.execute(params![
        page_id,
        link.target,
        agent_id,  // source agent
        format!("[[{}]]", link.target),
        link.is_cross_agent as i32,
        link.agent_id,
    ])?;
}
```

### 4. Add backlink/outlink query methods

```rust
impl ScratchpadStore {
    /// Get pages that link TO the given page title
    pub async fn get_backlinks(&self, agent_id: &str, title: &str) -> Result<Vec<PageSummary>, ScratchError>;

    /// Get page titles that the given page links TO
    pub async fn get_outlinks(&self, agent_id: &str, title: &str) -> Result<Vec<String>, ScratchError>;

    /// Get pages with no inbound links (potential orphans)
    pub async fn get_orphans(&self, agent_id: &str) -> Result<Vec<PageSummary>, ScratchError>;

    /// Get all links for a page (both directions)
    pub async fn get_all_links(&self, agent_id: &str, title: &str) -> Result<LinkInfo, ScratchError>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkInfo {
    pub backlinks: Vec<PageSummary>,   // Pages linking TO this page
    pub outlinks: Vec<String>,         // Titles this page links TO
    pub unresolved: Vec<String>,       // Outlinks to non-existent pages
}
```

### 5. Add `regex` dependency

Add `regex = "1"` to `crates/agentos-scratch/Cargo.toml`.

### 6. Update `delete_page()` to clean up links

When a page is deleted, remove its outbound links:
```rust
tx.execute("DELETE FROM link_index WHERE source_id = ?", [&page_id])?;
```

Note: inbound links from other pages are NOT deleted — they become "unresolved" links (pointing to a non-existent page). This matches Obsidian behavior where deleting a page leaves dangling links.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-scratch/src/links.rs` | **New** — wikilink parser + `WikiLink` type |
| `crates/agentos-scratch/src/store.rs` | Add `link_index` schema, update `write_page()` and `delete_page()`, add backlink methods |
| `crates/agentos-scratch/src/lib.rs` | Export `links` module |
| `crates/agentos-scratch/src/types.rs` | Add `LinkInfo` struct |
| `crates/agentos-scratch/Cargo.toml` | Add `regex = "1"` dependency |

---

## Dependencies

- **Requires:** Phase 1 (core storage)
- **Blocks:** Phase 3 (tools need link queries), Phase 4 (graph traversal needs adjacency table)

---

## Test Plan

| Test | Assertion |
|------|-----------|
| `test_parse_simple_wikilink` | `[[Foo]]` → `WikiLink { target: "Foo", display: None }` |
| `test_parse_aliased_wikilink` | `[[Foo\|bar]]` → `WikiLink { target: "Foo", display: Some("bar") }` |
| `test_parse_cross_agent` | `[[@agent123/Foo]]` → `is_cross_agent: true, agent_id: Some("agent123")` |
| `test_parse_multiple` | Content with 3 wikilinks returns 3 `WikiLink` items |
| `test_parse_no_links` | Plain text returns empty vec |
| `test_backlinks_populated` | Page A links to Page B → `get_backlinks("B")` includes A |
| `test_outlinks_populated` | Page A has `[[B]]` and `[[C]]` → `get_outlinks("A")` returns ["B", "C"] |
| `test_links_updated_on_rewrite` | Rewrite page A without `[[B]]` → B's backlinks no longer include A |
| `test_delete_cleans_outlinks` | Delete page A → `link_index` has no rows where source=A |
| `test_delete_preserves_inbound` | Delete page B → pages linking to B still have their outlinks (unresolved) |
| `test_orphan_detection` | Page with no inbound links appears in `get_orphans()` |
| `test_unresolved_links` | `[[NonExistent]]` shows up in `LinkInfo.unresolved` |

---

## Verification

```bash
cargo test -p agentos-scratch
cargo clippy -p agentos-scratch -- -D warnings
cargo fmt -p agentos-scratch -- --check
cargo build --workspace
```

---

## Related

- [[01-core-storage-engine]]
- [[03-scratchpad-tools]]
- [[Agent Scratchpad Data Flow]]
