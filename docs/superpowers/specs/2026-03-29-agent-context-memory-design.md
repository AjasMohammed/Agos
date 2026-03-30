# Agent Context Memory — Design Spec

> A per-agent, self-curated markdown document that is always injected into the context window at task start, enabling agents to learn and adapt across invocations without retraining.

**Date:** 2026-03-29
**Status:** Approved
**Approach:** SQLite-backed with render-to-markdown injection (Option A — next-invocation semantics)

---

## 1. Problem

AgentOS agents start every invocation with a blank slate beyond what the retrieval gate surfaces. In **task mode** the full chat history compensates, but in **autonomous/trigger mode** — where there is no prior conversation — agents lack persistent self-knowledge about:

- Ecosystem patterns they've previously discovered
- Mistakes they've made and corrections they've learned
- Preferences for how to interact with specific tools or other agents
- Domain knowledge accumulated over many tasks

The existing memory tiers (episodic, semantic, procedural) are **queryable stores** behind the retrieval gate. They require the agent to know what to ask for. What's missing is an **always-present, agent-curated context document** — analogous to how `CLAUDE.md` works for human developers — that the agent maintains for its own future self.

## 2. Solution Overview

Each registered agent gets a **context memory** — a single markdown document stored in SQLite with version history. The kernel injects it into the context window at every task start (pinned, never evicted). The agent updates it via a dedicated tool. Updates take effect on the **next invocation**, not the current one.

### Key Properties

| Property | Value |
|----------|-------|
| Storage | SQLite (`context_memory.db`) |
| Format | Free-form markdown |
| Injection timing | Task start, before retrieval gate |
| Injection role | `System` role, `Knowledge` category |
| Update semantics | Next invocation only (Option A) |
| Versioning | Every update archived; rollback via CLI |
| Token cap | 4,096 tokens (configurable) |
| Security | Write-own-only, injection-scanned, audit-logged |

## 3. Storage Schema

New database: `context_memory.db` (path relative to `data_dir`, configurable).

```sql
-- Hot path: one row per agent, current version only
CREATE TABLE context_memory (
    agent_id     TEXT PRIMARY KEY,
    content      TEXT NOT NULL DEFAULT '',
    token_count  INTEGER NOT NULL DEFAULT 0,
    version      INTEGER NOT NULL DEFAULT 0,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);

-- Cold path: version history for rollback and audit
CREATE TABLE context_memory_history (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    agent_id     TEXT NOT NULL,
    content      TEXT NOT NULL,
    token_count  INTEGER NOT NULL,
    version      INTEGER NOT NULL,
    updated_at   TEXT NOT NULL,
    reason       TEXT,
    UNIQUE(agent_id, version)
);
CREATE INDEX idx_cmh_agent ON context_memory_history(agent_id, version DESC);
```

### Why Two Tables

- `context_memory` is the **hot path** — queried once per task start. Single-row lookup by primary key.
- `context_memory_history` is **cold storage** — only accessed for rollback or audit review. Append-only.
- Keeps the hot-path query trivial: `SELECT content FROM context_memory WHERE agent_id = ?`

### Version Semantics

- Version 0 = initial empty state (created at agent registration)
- Each update increments version by 1
- Before overwriting `context_memory`, the current content is copied to `context_memory_history`
- Max versions retained: configurable (`max_versions`, default 50). Oldest versions pruned on write.

## 4. Token Budget and Estimation

- **Default cap: 4,096 tokens** — enough for substantial self-knowledge, small enough to not dominate context
- **Estimation method:** `content.len() / chars_per_token` using the `chars_per_token` value from `[context_budget]` config (default 4.0)
- **Enforcement:** The `context-memory-update` tool rejects writes exceeding the cap with error `ContextMemoryTooLarge { current_tokens, max_tokens }`
- **Injection budget:** Drawn from the Knowledge category (30% of total context budget). The context compiler accounts for it when budgeting other knowledge entries.

## 5. Context Injection

### Injection Point

In `context_injector.rs`, the context memory is injected **after the system prompt and before the user prompt**:

```
1. System prompt (pinned, importance=1.0)         ← existing
2. Agent context memory (pinned, importance=0.95)  ← NEW
3. User prompt (pinned)                            ← existing
4. Injection scan                                  ← existing
5. Retrieval gate                                  ← existing
```

### ContextEntry Properties

| Field | Value |
|-------|-------|
| `role` | `System` |
| `category` | `Knowledge` |
| `importance` | `0.95` |
| `pinned` | `true` |
| `is_summary` | `false` |
| `partition` | `Active` |

### Delimiter Wrapping

Content is wrapped in XML-style delimiters for clear boundary signaling to the LLM:

```
<agent-context-memory>
This is the agent's self-curated context, written by the agent for its own future invocations.
The agent can update this document using the context-memory-update tool.

{content from context_memory table}
</agent-context-memory>
```

The preamble line tells the LLM what this block is and how to use it.

### Empty Memory Handling

If the agent has no context memory (version 0, empty content), inject a brief bootstrapping prompt instead:

```
<agent-context-memory>
You have an empty context memory. As you work, use the context-memory-update tool to save
important patterns, preferences, and knowledge you want to remember for future tasks.
Write in markdown. Be concise — you have a 4096-token budget.
</agent-context-memory>
```

This teaches the agent the feature exists on its very first task.

## 6. Tools

### 6.1 `context-memory-update`

**Purpose:** Write or replace the agent's context memory document.

**Input Schema:**
```json
{
  "type": "object",
  "required": ["content"],
  "properties": {
    "content": {
      "type": "string",
      "description": "Markdown content for the agent's persistent context memory. This will be injected into your context window at the start of every future task."
    },
    "reason": {
      "type": "string",
      "description": "Brief explanation of why this update was made (stored in version history)."
    }
  }
}
```

**Behavior:**
1. Extract `agent_id` from capability token (write-own-only enforcement)
2. Strip any `<agent-context-memory>` / `</agent-context-memory>` tags from content (delimiter injection defense)
3. Run content through `injection_scanner` — reject if high-confidence threat detected
4. Estimate token count: `content.len() / chars_per_token`
5. If exceeds `max_tokens`, return `ContextMemoryTooLarge` error with current count and limit
6. Archive current version to `context_memory_history`
7. Prune history if versions exceed `max_versions`
8. Write new content to `context_memory` with incremented version
9. Log `AuditEvent::ContextMemoryUpdated { agent_id, version, token_count, reason }`
10. Return success with new version number and token count

**Permission:** `memory.context:w`

**Manifest:** `tools/core/context-memory-update.toml`

### 6.2 `context-memory-read`

**Purpose:** Read the agent's current context memory (for review before updating).

**Input Schema:**
```json
{
  "type": "object",
  "properties": {}
}
```

**Behavior:**
1. Extract `agent_id` from capability token
2. Query `context_memory` table
3. Return `{ content, version, token_count, updated_at }` or `{ content: "", version: 0 }` if none exists

**Permission:** `memory.context:r`

**Manifest:** `tools/core/context-memory-read.toml`

## 7. CLI Commands

Under the existing `agentctl agent` command group, add a `memory` subcommand:

```
agentctl agent memory show <agent-id>
    Display the current context memory content, version, and token count.

agentctl agent memory history <agent-id> [--limit N]
    List version history (version, updated_at, token_count, reason).

agentctl agent memory rollback <agent-id> <version>
    Restore a specific version from history. Creates a new version (not destructive).

agentctl agent memory clear <agent-id>
    Reset to empty content (version incremented, old content archived).

agentctl agent memory set <agent-id> --file <path>
    Manually set content from a markdown file (for bootstrapping).
    Validates token budget before writing.
```

All CLI commands go through the bus as `KernelCommand::ContextMemory*` variants.

## 8. Kernel Commands

New variants in `KernelCommand` enum:

```rust
ContextMemoryRead { agent_id: AgentID }
ContextMemoryUpdate { agent_id: AgentID, content: String, reason: Option<String> }
ContextMemoryHistory { agent_id: AgentID, limit: usize }
ContextMemoryRollback { agent_id: AgentID, version: u32 }
ContextMemoryClear { agent_id: AgentID }
ContextMemorySet { agent_id: AgentID, content: String }
```

Dispatched from `run_loop.rs` to a new `commands/context_memory.rs` handler module.

## 9. Security

### Write-Own-Only
The tool extracts `agent_id` from the authenticated `CapabilityToken`. An agent can never specify a different agent_id. CLI commands bypass this (operator privilege).

### Injection Scanning
Content is passed through `injection_scanner::scan()` before storage. If a high-confidence injection attempt is detected, the write is rejected with `AgentOSError::InjectionDetected` and an `AuditEvent::InjectionBlocked` entry is logged.

### Delimiter Defense
Any occurrence of `<agent-context-memory>` or `</agent-context-memory>` in the content is stripped before storage. This prevents an agent from breaking out of its delimiter boundary.

### Audit Trail
Every update logs `AuditEvent::ContextMemoryUpdated` with:
- `agent_id`
- `version` (new version number)
- `token_count`
- `reason` (if provided)
- `timestamp`

### Size Limits
- Token cap enforced at write time (default 4,096)
- Raw content byte cap: `max_tokens * chars_per_token * 2` as a safety backstop (default ~32 KB)

## 10. Autonomous Workflow Compatibility

This feature is specifically designed for **pure agentic workflows**:

| Scenario | Behavior |
|----------|----------|
| Task mode (user-initiated) | Context memory injected alongside full chat history |
| Autonomous/trigger mode (no chat history) | Context memory is the agent's primary self-knowledge |
| Multi-agent chain (agent-call) | Each agent gets its own context memory independently |
| First-ever task (new agent) | Empty memory with bootstrapping prompt |
| Agent discovers ecosystem pattern | Writes to context memory; all future invocations benefit |
| Agent makes a recurring mistake | Writes correction; never repeats it |
| Cross-agent learning | Not supported in V1 (agents only read their own). Future: opt-in shared sections |

### Agent Lifecycle Integration

- **Agent registration:** Creates an empty row in `context_memory` (version 0)
- **Agent deregistration:** Archives context memory to history, deletes from hot table
- **Agent cloning (future):** Could copy context memory to bootstrap new agents

## 11. Configuration

Added to `config/default.toml`:

```toml
[memory.context]
enabled = true
max_tokens = 4096
max_versions = 50
db_path = "context_memory.db"
```

All fields have sensible defaults. `enabled = false` skips injection entirely (for resource-constrained deployments).

## 12. Files Changed

| Crate | File | Change Type |
|-------|------|-------------|
| `agentos-kernel` | `src/context_memory_store.rs` | **New** — SQLite store (open, read, write, rollback, history, clear, prune) |
| `agentos-kernel` | `src/kernel.rs` | **Modify** — Add `context_memory_store: Arc<ContextMemoryStore>` field, initialize in `new()` |
| `agentos-kernel` | `src/context_injector.rs` | **Modify** — Inject context memory after system prompt |
| `agentos-kernel` | `src/commands/context_memory.rs` | **New** — Handler for all ContextMemory kernel commands |
| `agentos-kernel` | `src/commands/mod.rs` | **Modify** — Add `pub mod context_memory;` |
| `agentos-kernel` | `src/run_loop.rs` | **Modify** — Dispatch ContextMemory command variants |
| `agentos-tools` | `src/context_memory_update.rs` | **New** — Update tool implementation |
| `agentos-tools` | `src/context_memory_read.rs` | **New** — Read tool implementation |
| `agentos-tools` | `src/factory.rs` | **Modify** — Register both new tools |
| `agentos-tools` | `src/lib.rs` | **Modify** — Add module declarations |
| `agentos-bus` | `src/message.rs` | **Modify** — Add `ContextMemory*` variants to `KernelCommand` and `KernelResponse` |
| `agentos-types` | `src/error.rs` | **Modify** — Add `ContextMemoryTooLarge` and update `InjectionDetected` if needed |
| `agentos-types` | `src/config.rs` | **Modify** — Add `ContextMemoryConfig` struct |
| `agentos-audit` | `src/lib.rs` | **Modify** — Add `ContextMemoryUpdated` event type |
| `agentos-cli` | `src/commands/agent.rs` | **Modify** — Add `memory` subcommand group (show, history, rollback, clear, set) |
| `tools/core/` | `context-memory-update.toml` | **New** — Tool manifest |
| `tools/core/` | `context-memory-read.toml` | **New** — Tool manifest |
| `config/` | `default.toml` | **Modify** — Add `[memory.context]` section |

## 13. Testing Strategy

### Unit Tests (per crate)

**`context_memory_store.rs`:**
- Write and read back content
- Version increments correctly
- History populated on update
- Rollback restores correct version as a NEW version (version N+1 with old content, doesn't rewrite history)
- Clear archives and resets
- Prune removes oldest versions when exceeding max_versions
- Token budget enforcement (reject oversized writes)
- Concurrent reads don't block

**`context_memory_update.rs` (tool):**
- Successful write returns new version
- Rejects content exceeding token cap
- Strips delimiter tags from content
- Agent can only write own memory (agent_id from token, not input)

**`context_memory_read.rs` (tool):**
- Returns current content and metadata
- Returns empty for new agents

**`context_injector.rs`:**
- Context memory injected at correct position (after system prompt, before user prompt)
- Empty memory injects bootstrapping prompt
- Disabled config skips injection

### Integration Tests

- Full lifecycle: register agent → first task (empty bootstrap) → update memory → second task (memory injected)
- CLI commands: show, history, rollback, clear, set
- Multi-agent: each agent sees only its own context memory
- Injection scanner blocks malicious content

## 14. Non-Goals (V1)

- Cross-agent context memory reading (future: opt-in shared sections)
- Hot-reload within current task (decided: next-invocation only)
- LLM-assisted memory curation (future: consolidation engine suggests updates)
- Memory merge/diff tools (future: structured sections with selective updates)
- Automatic context memory population (agent must explicitly write)

## 15. Future Extensions

1. **Structured sections** — Allow agents to update specific sections (e.g., `## Tools I've Learned`) without rewriting the whole document
2. **Cross-agent sharing** — Opt-in readable sections for multi-agent coordination
3. **Consolidation integration** — The consolidation engine suggests updates based on procedural patterns
4. **Memory templates** — Pre-populated templates for common agent roles
5. **Token-aware compression** — When approaching the cap, offer to summarize older sections
