---
title: 30-07 Agent Manual New Sections
tags:
  - tools
  - agent-manual
  - next-steps
  - subtask
date: 2026-03-18
status: planned
effort: 4h
priority: medium
---

# 30-07 — Agent Manual New Sections

> Extend `agent-manual` with three new sections: `agents` (peer discovery), `tasks` (task lifecycle reference), and `procedural` (how to use procedure-create/procedure-search). Update `index` to list them.

---

## Why This Phase

After phases 30-01 through 30-06, 16 new tools are available but `agent-manual` only documents 20. An agent querying `section=tools` will see the new tools in the list, but has no dedicated reference for the coordination tier (`agents`, `tasks`) or procedural memory workflow.

---

## Prerequisites

- [[30-01-Missing Tool Manifests]] — `procedure-create` and `procedure-search` must have manifests before they're worth documenting
- [[30-05-Agent Discovery Tool]] — `agent-list` must exist before the `agents` section is useful

---

## Current → Target State

| Section | Current | Target |
|---------|---------|--------|
| `index` | Lists 9 sections | Lists 12 sections |
| `agents` | does not exist | Peer discovery reference |
| `tasks` | does not exist | Task lifecycle reference |
| `procedural` | does not exist | Procedural memory workflow |

---

## What to Do

Read `crates/agentos-tools/src/agent_manual.rs` in full before editing. Key structures to understand:
- `ManualSection` enum — add three new variants
- `ManualSection::from_str()` — add three new match arms
- `ManualSection::all_names()` — add three new names
- `AgentManualTool::execute()` — dispatch to new content generators
- Content generator functions (one per section)

### Step 1 — Add new `ManualSection` variants

In the `ManualSection` enum, add:
```rust
pub enum ManualSection {
    // existing variants ...
    Agents,
    Tasks,
    Procedural,
}
```

In `from_str()`:
```rust
"agents" => Some(Self::Agents),
"tasks" => Some(Self::Tasks),
"procedural" => Some(Self::Procedural),
```

In `all_names()`:
```rust
&[
    "index", "tools", "tool-detail", "permissions", "memory",
    "events", "commands", "errors", "feedback",
    "agents", "tasks", "procedural",  // NEW
]
```

### Step 2 — Add dispatch in `execute()`

Find the `match section { ... }` block in `execute()` and add:
```rust
ManualSection::Agents => Self::section_agents(),
ManualSection::Tasks => Self::section_tasks(),
ManualSection::Procedural => Self::section_procedural(),
```

### Step 3 — Implement `section_agents()`

```rust
fn section_agents() -> Result<serde_json::Value, AgentOSError> {
    Ok(serde_json::json!({
        "section": "agents",
        "title": "Agent Discovery & Coordination",
        "summary": "How to find available agents and coordinate with them.",
        "subsections": [
            {
                "title": "Discover Peers",
                "content": "Use 'agent-list' to see all registered agents with their status and capabilities. Filter by status with {\"status\": \"idle\"} to find available agents."
            },
            {
                "title": "Send a Message",
                "content": "Use 'agent-message' to send a message to a named agent. The message is queued for the agent's next iteration. Required permission: agent.message:x"
            },
            {
                "title": "Delegate a Task",
                "content": "Use 'task-delegate' to hand off a sub-task to another agent. Provide {\"agent\": \"<name>\", \"task\": \"<prompt>\", \"priority\": 1-10}. The delegation is non-blocking — control returns immediately. Use 'task-status' with the returned task ID to monitor completion."
            },
            {
                "title": "Coordination Pattern",
                "content": "1. Call 'think' to plan the delegation strategy. 2. Call 'agent-list' to find available agents. 3. Call 'task-delegate' with the selected agent. 4. Poll 'task-status' periodically until status='completed' or 'failed'. 5. Act on the result."
            }
        ]
    }))
}
```

### Step 4 — Implement `section_tasks()`

```rust
fn section_tasks() -> Result<serde_json::Value, AgentOSError> {
    Ok(serde_json::json!({
        "section": "tasks",
        "title": "Task Lifecycle",
        "summary": "Task states, introspection tools, and how to interpret results.",
        "subsections": [
            {
                "title": "Task States",
                "content": "pending → running → completed | failed | cancelled. A task starts as 'pending' when created. It becomes 'running' when an agent picks it up. Terminal states are 'completed', 'failed', and 'cancelled'."
            },
            {
                "title": "Inspect a Task",
                "content": "Use 'task-status' with {\"task_id\": \"<uuid>\"}. Returns: id, description, status, assigned_agent, created_at, started_at, completed_at, result_preview (first 200 chars), error. Required permission: task.query:r"
            },
            {
                "title": "List Your Tasks",
                "content": "Use 'task-list' with {\"filter\": \"mine\"} (default) for your tasks, or {\"filter\": \"active\"} for all running/pending tasks across agents. Optional 'limit' field (default 20, max 100). Required permission: task.query:r"
            },
            {
                "title": "Best Practices",
                "content": "After delegating, store the returned task ID in a memory block or episodic memory. When the delegated task completes, retrieve the result_preview and decide whether to query the full result via 'memory-search' or 'file-reader'."
            }
        ]
    }))
}
```

### Step 5 — Implement `section_procedural()`

```rust
fn section_procedural() -> Result<serde_json::Value, AgentOSError> {
    Ok(serde_json::json!({
        "section": "procedural",
        "title": "Procedural Memory",
        "summary": "How to record and retrieve step-by-step procedures for future reuse.",
        "subsections": [
            {
                "title": "What is Procedural Memory",
                "content": "Procedural memory stores how-to knowledge: step-by-step procedures, SOPs, and task templates. Unlike semantic memory (facts) or episodic memory (events), procedural memory records *actions* in order. Procedures are shared across agents."
            },
            {
                "title": "Record a Procedure",
                "content": "Use 'procedure-create' with: {\"name\": \"<short name>\", \"description\": \"<what it does>\", \"steps\": [{\"action\": \"...\", \"tool\": \"<tool-name>\", \"expected_outcome\": \"...\"}], \"preconditions\": [...], \"postconditions\": [...], \"tags\": [...]}. Required permission: memory.procedural:w"
            },
            {
                "title": "Find a Procedure",
                "content": "Use 'procedure-search' with {\"query\": \"<description of what you want to do>\", \"top_k\": 5}. Returns procedures ranked by semantic similarity. Check 'steps' array for the exact sequence. Required permission: memory.procedural:r"
            },
            {
                "title": "When to Record",
                "content": "Record a procedure when you successfully complete a multi-step task that you are likely to repeat (or that other agents may need). Include the tools used in each step's 'tool' field so future agents can validate they have the right permissions before starting."
            }
        ]
    }))
}
```

### Step 6 — Update `index` section

Find the existing `section_index()` function and add the three new sections to its listing. The index should describe all 12 sections with a one-line summary.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-tools/src/agent_manual.rs` | Add `Agents`, `Tasks`, `Procedural` variants; implement 3 content functions; update `index` |

---

## Verification

```bash
cargo build -p agentos-tools
cargo test -p agentos-tools -- agent_manual
```

Manual spot-check:
```bash
# Assuming kernel is running
agentctl tool call agent-manual '{"section":"agents"}'
agentctl tool call agent-manual '{"section":"tasks"}'
agentctl tool call agent-manual '{"section":"procedural"}'
agentctl tool call agent-manual '{"section":"index"}'
# index output must list "agents", "tasks", "procedural"
```
