---
title: "Phase 6: Episodic Auto-Write Integration"
tags:
  - memory
  - scratchpad
  - episodic
  - v3
  - plan
date: 2026-03-23
status: complete
effort: 1d
priority: medium
---

# Phase 6: Episodic Auto-Write Integration

> Automatically generate scratchpad notes when tasks complete, linking them to relevant concepts and creating an organic knowledge trail.

---

## Why This Phase

Agents currently lose their working context when a task ends. Episodic memory records *what happened* (events), but not *what was learned* (insights). Auto-generating a scratchpad note on task completion captures the agent's understanding — what worked, what failed, what patterns emerged — and links it to existing knowledge.

This addresses **Remaining Gap #4** from the project memory: "Phase 5.1 — episodic memory auto-write on task completion."

Over time, the scratchpad becomes a growing knowledge base that each agent builds organically through their work, not just through explicit write calls.

---

## Current → Target State

**Current:** Tasks complete → episodic entries logged → working context discarded. No knowledge synthesis.

**Target:**
- On task completion (success or failure), kernel generates a summary scratchpad note
- Note title: `Task: {task_description_summary}` (auto-generated, deduped)
- Note content: structured markdown with outcome, key observations, tool calls summary, errors
- Auto-detected wikilinks: scan for references to existing scratchpad page titles in the summary
- Configurable via `[scratchpad] auto_write_on_completion = true`
- Kernel hook in task completion path (not tool-triggered — kernel-internal)

---

## Detailed Subtasks

### 1. Add auto-write config

In `config/default.toml`:

```toml
[scratchpad]
# ... existing config ...
auto_write_on_completion = true
auto_write_min_steps = 3      # Don't write notes for trivial tasks
auto_write_max_summary = 2048  # Max bytes for auto-generated note
```

### 2. Implement task summary generator

New file or method in `agentos-kernel`:

```rust
/// Generate a scratchpad note summarizing a completed task.
pub fn generate_task_summary(
    task: &AgentTask,
    episodes: &[EpisodicEntry],
    existing_pages: &[String],  // Titles of existing scratch pages for auto-linking
) -> TaskSummary {
    let mut content = String::new();

    // Header
    content.push_str(&format!("# Task: {}\n\n", task.description));
    content.push_str(&format!("**Status:** {}\n", task.status));
    content.push_str(&format!("**Agent:** {}\n", task.agent_id));
    content.push_str(&format!("**Completed:** {}\n\n", Utc::now().to_rfc3339()));

    // Key observations from episodes
    content.push_str("## What Happened\n\n");
    for ep in episodes.iter().filter(|e| matches!(e.episode_type, EpisodeType::ToolResult | EpisodeType::LLMResponse)) {
        if let Some(summary) = &ep.summary {
            content.push_str(&format!("- {}\n", summary));
        }
    }

    // Errors encountered
    let errors: Vec<_> = episodes.iter()
        .filter(|e| e.content.contains("error") || e.content.contains("failed"))
        .collect();
    if !errors.is_empty() {
        content.push_str("\n## Errors Encountered\n\n");
        for err in errors {
            content.push_str(&format!("- {}\n", err.summary.as_deref().unwrap_or(&err.content)));
        }
    }

    // Auto-link: scan for references to existing page titles
    for page_title in existing_pages {
        if content.contains(page_title) {
            // Replace plain text mention with wikilink
            content = content.replace(page_title, &format!("[[{}]]", page_title));
        }
    }

    TaskSummary { title: truncate_title(&task.description), content, tags: vec!["auto".to_string(), "task-summary".to_string()] }
}
```

### 3. Hook into task completion path

In the kernel's task completion handler (likely in `task_executor.rs` or `run_loop.rs`):

```rust
// After task completes
if config.scratchpad.auto_write_on_completion {
    let episodes = episodic_memory.list_by_task(&task.id).await?;

    // Skip trivial tasks
    if episodes.len() >= config.scratchpad.auto_write_min_steps {
        let existing_pages = scratchpad_store.list_pages(&task.agent_id).await?
            .iter().map(|p| p.title.clone()).collect::<Vec<_>>();

        let summary = generate_task_summary(&task, &episodes, &existing_pages);

        scratchpad_store.write_page(
            &task.agent_id,
            &summary.title,
            &summary.content,
            &summary.tags,
        ).await?;

        tracing::info!(
            agent_id = %task.agent_id,
            title = %summary.title,
            "Auto-generated scratchpad note for completed task"
        );
    }
}
```

### 4. Title deduplication

If a page with the same title already exists (agent ran a similar task before), append a counter:
- `Task: Fix login bug` → exists → `Task: Fix login bug (2)`

Or update the existing page with a new section:
```markdown
---
## Run 2 (2026-03-24)
...
```

Decision: **append counter** for simplicity. Each task run is a distinct page.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/task_summary.rs` | **New** — `generate_task_summary()` function |
| `crates/agentos-kernel/src/task_executor.rs` | Hook auto-write into task completion path |
| `crates/agentos-kernel/src/kernel.rs` | Add scratchpad config fields |
| `config/default.toml` | Add `auto_write_on_completion`, `auto_write_min_steps`, `auto_write_max_summary` |

---

## Dependencies

- **Requires:** Phase 1 (storage), Phase 3 (kernel has scratchpad_store)
- **Blocks:** Nothing — this is a leaf phase

---

## Test Plan

| Test | Assertion |
|------|-----------|
| `test_auto_write_on_completion` | Task completes → scratchpad page created with "auto" tag |
| `test_no_auto_write_trivial_task` | Task with <3 episodes → no page created |
| `test_auto_write_disabled` | Config `auto_write_on_completion = false` → no page created |
| `test_auto_link_detection` | Existing page "Login System" → mention in summary becomes `[[Login System]]` |
| `test_title_dedup` | Second task with same description → page title has `(2)` suffix |
| `test_auto_write_content` | Generated note contains task status, agent ID, completion time |
| `test_auto_write_errors` | Task with errors → "Errors Encountered" section present |

---

## Verification

```bash
cargo test -p agentos-kernel -- task_summary
cargo test -p agentos-kernel -- auto_write
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

---

## Related

- [[01-core-storage-engine]]
- [[03-scratchpad-tools]]
- [[Agent Scratchpad Plan]]
