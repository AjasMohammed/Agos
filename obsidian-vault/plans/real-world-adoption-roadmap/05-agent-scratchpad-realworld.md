---
title: Agent Scratchpad — Real-World Context
tags:
  - scratchpad
  - memory
  - agent
  - plan
  - v3
date: 2026-03-25
status: planned
effort: 7d
priority: medium
---

# Phase 5 — Agent Scratchpad (Real-World Context)

> Implement an Obsidian-inspired wikilink knowledge graph as agent working memory, allowing agents to maintain complex reasoning across long tasks without burning context window tokens on every iteration. This directly addresses the "semantic drift" failure mode identified in production agent systems.

---

## Why This Phase

Ecosystem research identifies **semantic drift** as a critical production failure mode:

> "In complex, multi-stage reasoning tasks, agents often suffer from a loss of context or logical consistency as the task progresses, known as semantic drift."

The three-tier memory system (episodic, semantic, procedural) is excellent for **retrieval across tasks**. But it has no mechanism for **within-task structured working memory** — a persistent scratchpad where an agent can:

- Write intermediate findings as named pages
- Link related findings with wikilinks
- Read back specific pages by name without a semantic search
- Build a graph of related concepts that persists across iterations without stuffing it all in the context window

This makes AgentOS uniquely differentiated: most frameworks have no scratchpad concept at all. Agents either hallucinate continuity or stuff the entire task history into context (burning tokens).

---

## Current → Target State

| Area | Current | Target |
|------|---------|--------|
| Within-task working memory | Context window only | Persistent scratchpad pages in SQLite |
| Cross-iteration notes | None | Named pages with `[[wikilinks]]` |
| Agent navigation | Semantic search (FTS5+vectors) | Direct page read by name + wikilink traversal |
| Task-scoped knowledge | None | Scratchpad auto-scoped to task; persists on completion |
| Knowledge graph | None | Backlink graph: which pages link to this one |
| Tools | None | 5 scratchpad tools (read, write, list, link, search) |
| CLI | None | `agentctl scratchpad list/read/delete` |

---

## Architecture

```
Agent Task (multiple iterations)
     │
     │  tool call: scratchpad-write "Research Notes" "Found X, links to [[Y]]"
     │  tool call: scratchpad-read "Research Notes"
     │  tool call: scratchpad-search "database connection"
     │  tool call: scratchpad-link "Research Notes" → "Implementation Plan"
     ▼
┌──────────────────────────────────────────────┐
│  Scratchpad Engine (agentos-scratch crate)   │
│                                              │
│  PageStore (SQLite)                          │
│  ├── pages(id, task_id, agent_id, title,     │
│  │         content, created_at, updated_at)  │
│  └── links(from_page_id, to_page_title)      │
│                                              │
│  WikilinkParser                              │
│  ├── Extract [[PageTitle]] references        │
│  └── Resolve to page IDs                    │
│                                              │
│  BacklinkIndex                               │
│  └── title → List<page_id that links to it> │
└──────────────────────────────────────────────┘
     │
     │  On task completion: snapshot scratchpad
     │  to episodic memory (summary + key pages)
     ▼
  Episodic Memory (existing)
```

---

## Detailed Subtasks

### Subtask 5.1 — Core scratchpad storage (agentos-scratch crate)

The `agentos-scratch` crate is listed in `Cargo.toml` but unimplemented. Implement it now.

**File:** `crates/agentos-scratch/src/lib.rs`

```rust
pub mod page_store;
pub mod wikilink;
pub mod backlink;
pub mod engine;

pub use engine::ScratchpadEngine;
pub use page_store::{Page, PageID};
```

**File:** `crates/agentos-scratch/src/page_store.rs`

```rust
use rusqlite::{Connection, params};
use crate::types::{TaskID, AgentID};

pub struct PageStore {
    conn: Arc<Mutex<Connection>>,
}

impl PageStore {
    pub fn new(db_path: &Path) -> Result<Self> {
        let conn = Connection::open(db_path)?;
        conn.execute_batch("
            CREATE TABLE IF NOT EXISTS pages (
                id          TEXT PRIMARY KEY,
                task_id     TEXT,
                agent_id    TEXT NOT NULL,
                title       TEXT NOT NULL,
                content     TEXT NOT NULL,
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL,
                UNIQUE(agent_id, task_id, title)
            );
            CREATE TABLE IF NOT EXISTS links (
                from_id     TEXT NOT NULL,
                to_title    TEXT NOT NULL,
                PRIMARY KEY (from_id, to_title)
            );
            CREATE VIRTUAL TABLE IF NOT EXISTS pages_fts USING fts5(
                title, content,
                content=pages, content_rowid=rowid
            );
        ")?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    pub fn write_page(
        &self,
        agent_id: &AgentID,
        task_id: Option<&TaskID>,
        title: &str,
        content: &str,
    ) -> Result<PageID> { ... }

    pub fn read_page(
        &self,
        agent_id: &AgentID,
        title: &str,
        task_id: Option<&TaskID>,
    ) -> Result<Option<Page>> { ... }

    pub fn list_pages(
        &self,
        agent_id: &AgentID,
        task_id: Option<&TaskID>,
    ) -> Result<Vec<PageSummary>> { ... }

    pub fn search_pages(
        &self,
        agent_id: &AgentID,
        query: &str,
    ) -> Result<Vec<Page>> { ... }

    pub fn backlinks(&self, title: &str, agent_id: &AgentID) -> Result<Vec<String>> { ... }

    pub fn delete_page(&self, agent_id: &AgentID, title: &str) -> Result<()> { ... }
}
```

---

### Subtask 5.2 — Wikilink parser

**File:** `crates/agentos-scratch/src/wikilink.rs`

```rust
use regex::Regex;

pub struct WikilinkParser {
    pattern: Regex,  // \[\[([^\]]+)\]\]
}

impl WikilinkParser {
    pub fn new() -> Self {
        Self { pattern: Regex::new(r"\[\[([^\]]+)\]\]").unwrap() }
    }

    /// Extract all [[PageTitle]] references from content
    pub fn extract_links(&self, content: &str) -> Vec<String> {
        self.pattern
            .captures_iter(content)
            .map(|cap| cap[1].to_string())
            .collect()
    }

    /// Render wikilinks as plain text (for context injection)
    pub fn render_links(&self, content: &str, resolver: impl Fn(&str) -> Option<String>) -> String {
        self.pattern.replace_all(content, |caps: &regex::Captures| {
            let title = &caps[1];
            match resolver(title) {
                Some(page_content) => format!("[{}]: {}", title, &page_content[..200.min(page_content.len())]),
                None => format!("[{}]: (page not found)", title),
            }
        }).to_string()
    }
}
```

---

### Subtask 5.3 — ScratchpadEngine: main API

**File:** `crates/agentos-scratch/src/engine.rs`

```rust
pub struct ScratchpadEngine {
    store: Arc<PageStore>,
    parser: WikilinkParser,
}

impl ScratchpadEngine {
    pub fn new(data_dir: &Path) -> Result<Self> {
        Ok(Self {
            store: Arc::new(PageStore::new(&data_dir.join("scratchpad.db"))?),
            parser: WikilinkParser::new(),
        })
    }

    pub async fn write(
        &self,
        agent_id: &AgentID,
        task_id: Option<&TaskID>,
        title: &str,
        content: &str,
        append: bool,
    ) -> Result<PageID> {
        let final_content = if append {
            let existing = self.store.read_page(agent_id, title, task_id)?
                .map(|p| p.content)
                .unwrap_or_default();
            format!("{}\n\n{}", existing, content)
        } else {
            content.to_string()
        };

        // Extract and store wikilinks
        let links = self.parser.extract_links(&final_content);
        let page_id = self.store.write_page(agent_id, task_id, title, &final_content)?;
        self.store.update_links(&page_id, &links)?;
        Ok(page_id)
    }

    pub async fn read(
        &self,
        agent_id: &AgentID,
        title: &str,
        task_id: Option<&TaskID>,
        resolve_links: bool,
    ) -> Result<Option<String>> {
        match self.store.read_page(agent_id, title, task_id)? {
            None => Ok(None),
            Some(page) => {
                if resolve_links {
                    // Inline first 200 chars of each linked page
                    let content = self.parser.render_links(&page.content, |t| {
                        self.store.read_page(agent_id, t, task_id).ok()?.map(|p| p.content)
                    });
                    Ok(Some(content))
                } else {
                    Ok(Some(page.content))
                }
            }
        }
    }

    /// Snapshot task scratchpad into episodic memory on task completion
    pub async fn snapshot_to_episodic(
        &self,
        agent_id: &AgentID,
        task_id: &TaskID,
        episodic_store: &EpisodicStore,
    ) -> Result<()> {
        let pages = self.store.list_pages(agent_id, Some(task_id))?;
        if pages.is_empty() { return Ok(()); }

        let summary = format!(
            "Task scratchpad: {} pages. Titles: {}",
            pages.len(),
            pages.iter().map(|p| p.title.as_str()).collect::<Vec<_>>().join(", ")
        );
        episodic_store.write_entry(agent_id, task_id, &summary).await?;
        Ok(())
    }
}
```

---

### Subtask 5.4 — 5 scratchpad tools

**File:** `crates/agentos-tools/src/scratchpad.rs` (new)

Register 5 tools with the kernel:

| Tool | Description | Permission | Key Parameters |
|------|-------------|-----------|----------------|
| `scratchpad-write` | Write or append to a named page | `scratchpad:w` | `title`, `content`, `append: bool` |
| `scratchpad-read` | Read a page by title (resolves wikilinks) | `scratchpad:r` | `title`, `resolve_links: bool` |
| `scratchpad-list` | List all pages in current task scope | `scratchpad:r` | `include_content: bool` |
| `scratchpad-search` | Full-text search across pages | `scratchpad:r` | `query` |
| `scratchpad-backlinks` | List pages that link to a given title | `scratchpad:r` | `title` |

**File:** `tools/core/scratchpad-write.toml` (new)
**File:** `tools/core/scratchpad-read.toml` (new)
**File:** `tools/core/scratchpad-list.toml` (new)
**File:** `tools/core/scratchpad-search.toml` (new)
**File:** `tools/core/scratchpad-backlinks.toml` (new)

---

### Subtask 5.5 — Kernel integration

**File:** `crates/agentos-kernel/src/context.rs`

Add `scratchpad: Arc<ScratchpadEngine>` to `KernelContext`. Initialize in kernel boot from config `data_dir`.

**File:** `crates/agentos-kernel/src/task_completion.rs`

On task completion, call `scratchpad.snapshot_to_episodic(agent_id, task_id, episodic)`. This already happens for memory writes; add scratchpad alongside.

**File:** `crates/agentos-kernel/src/core_manifests.rs`

Register the 5 scratchpad tools as core tools (loaded on boot, no runtime signature check).

---

### Subtask 5.6 — CLI commands

**File:** `crates/agentos-cli/src/commands/` (new file: `scratchpad.rs`)

```bash
agentctl scratchpad list [--agent AGENT] [--task TASK_ID]
agentctl scratchpad read <TITLE> [--agent AGENT]
agentctl scratchpad delete <TITLE> [--agent AGENT]
agentctl scratchpad graph [--agent AGENT]   # print wikilink graph as ASCII
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-scratch/src/lib.rs` | New — module exports |
| `crates/agentos-scratch/src/page_store.rs` | New — SQLite page storage |
| `crates/agentos-scratch/src/wikilink.rs` | New — wikilink parser |
| `crates/agentos-scratch/src/engine.rs` | New — ScratchpadEngine API |
| `crates/agentos-tools/src/scratchpad.rs` | New — 5 scratchpad tool implementations |
| `tools/core/scratchpad-*.toml` | New — 5 tool manifests |
| `crates/agentos-kernel/src/context.rs` | Modified — add ScratchpadEngine |
| `crates/agentos-kernel/src/task_completion.rs` | Modified — snapshot on task end |
| `crates/agentos-kernel/src/core_manifests.rs` | Modified — register scratchpad tools |
| `crates/agentos-cli/src/commands/scratchpad.rs` | New — CLI commands |

---

## Dependencies

- No other phases required
- Requires existing episodic memory store (already complete)
- Builds on `agentos-scratch` crate skeleton already in workspace

---

## Test Plan

1. **Write and read roundtrip** — write page "Research", read it back, assert content matches
2. **Wikilink extraction** — write "[[Topic A]] and [[Topic B]]", call extract_links, assert 2 links found
3. **Wikilink resolution** — write Topic A page, read Research page with `resolve_links=true`, assert Topic A content inlined
4. **Backlink index** — write Topic A linking to [[Topic B]], call backlinks("Topic B"), assert ["Topic A"] returned
5. **FTS search** — write 3 pages, search for a keyword only in page 2, assert single result
6. **Snapshot to episodic** — complete a task with 3 scratchpad pages, verify episodic store has an entry mentioning the page titles
7. **Task scoping** — write pages under task T1 and T2, list with task_id=T1, assert only T1 pages returned

---

## Verification

```bash
cargo build -p agentos-scratch -p agentos-tools
cargo test -p agentos-scratch

# Integration test
agentctl task run --agent myagent "Research the Rust borrow checker rules. Write your findings to a scratchpad. Then write a summary page that links back to your research."
agentctl scratchpad list --agent myagent
agentctl scratchpad read "Summary"
```

---

## Related

- [[Real World Adoption Roadmap Plan]]
- [[Agent Scratchpad Plan]] — original design document in agent-scratchpad/ directory
