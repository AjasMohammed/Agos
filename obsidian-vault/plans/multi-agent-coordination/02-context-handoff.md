---
title: "Phase 02 — Context Handoff"
tags:
  - kernel
  - agents
  - context
  - v4
  - plan
date: 2026-04-02
status: planned
effort: 2d
priority: critical
---

# Phase 02 — Context Handoff

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> When a parent task spawns a child, pass a slice of the parent's context window as the child's initial context. When the child completes, inject its result back into the parent context as a synthetic tool response.

---

## Why This Phase

Sub-agent spawning (Phase 1) creates the child task, but the child starts with an empty context. That means the parent can't say "here's what I've figured out so far, now go do the specific part." And when the child finishes, its result disappears unless the parent is told about it. This phase wires the information flow in both directions.

---

## Current → Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Child task context | Empty on start | Seeded with a slice from parent's context |
| Child result | Written to task store | Also injected into parent context as tool response |
| Context slice | No mechanism | `ContextSlice` struct; parent selects which messages to pass |
| Parent awaiting | `AwaitSubAgents` returns raw strings | Returns structured `SubAgentResult` injected into context |

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-types/src/context.rs` | Add `ContextSlice` and `SubAgentResult` types |
| `crates/agentos-kernel/src/commands/sub_agent.rs` | Accept optional `context_slice` in `cmd_spawn_sub_agent`; seed child context |
| `crates/agentos-kernel/src/context.rs` | Add `inject_sub_agent_result()` method to `ContextManager` |
| `crates/agentos-kernel/src/commands/sub_agent.rs` | `cmd_await_sub_agents` — inject results into parent context |

---

## Detailed Tasks

### Task 1: Add `ContextSlice` and `SubAgentResult` types

**Files:**
- Modify: `crates/agentos-types/src/context.rs` (or lib.rs if context types are inline)

- [ ] **Step 1: Read context types**

```bash
grep -rn "pub struct.*Context\|pub enum.*Context" crates/agentos-types/src/
```

Identify where context-related types live.

- [ ] **Step 2: Add `ContextSlice`**

```rust
/// A portable slice of a context window passed from parent to child agent.
/// Contains a bounded number of messages selected by the parent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextSlice {
    /// Selected messages from the parent's context window (most recent N).
    pub messages: Vec<ContextEntry>,
    /// Human-readable label for audit purposes.
    pub label: String,
}

impl ContextSlice {
    /// Take the last `n` messages from a context window.
    pub fn last_n(context: &ContextWindow, n: usize, label: impl Into<String>) -> Self {
        let messages = context
            .entries()
            .iter()
            .rev()
            .take(n)
            .rev()
            .cloned()
            .collect();
        Self { messages, label: label.into() }
    }
}
```

- [ ] **Step 3: Add `SubAgentResult`**

```rust
/// The result of a completed sub-agent task, ready to be injected into the parent context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentResult {
    pub child_task_id: TaskID,
    pub agent_name: String,
    /// The final output text of the child task (truncated to 8KB).
    pub output: String,
    /// Whether the child completed successfully.
    pub success: bool,
}
```

- [ ] **Step 4: Write a unit test**

```rust
#[test]
fn test_context_slice_last_n() {
    let mut window = ContextWindow::new(/* capacity */ 10_000);
    for i in 0..5 {
        window.push(ContextEntry::user(format!("message {}", i)));
    }
    let slice = ContextSlice::last_n(&window, 3, "test");
    assert_eq!(slice.messages.len(), 3);
    // Should have messages 2, 3, 4
    assert!(slice.messages[0].content().contains("message 2"));
}
```

- [ ] **Step 5: Run the test**

```bash
cargo test -p agentos-types test_context_slice_last_n
```

Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/agentos-types/src/
git commit -m "feat(types): add ContextSlice and SubAgentResult for sub-agent context handoff"
```

---

### Task 2: Seed child context on spawn

**Files:**
- Modify: `crates/agentos-bus/src/message.rs` — add `context_slice: Option<ContextSlice>` to `SpawnSubAgent`
- Modify: `crates/agentos-kernel/src/commands/sub_agent.rs` — use it when building the child's context window

- [ ] **Step 1: Add `context_slice` field to `SpawnSubAgent`**

In `crates/agentos-bus/src/message.rs`, update the `SpawnSubAgent` variant:

```rust
SpawnSubAgent {
    parent_task_id: TaskID,
    agent_name: String,
    prompt: String,
    requested_permissions: Vec<String>,
    /// Optional slice of the parent's context to seed the child with.
    #[serde(default)]
    context_slice: Option<agentos_types::ContextSlice>,
},
```

- [ ] **Step 2: Use the slice in `cmd_spawn_sub_agent`**

In `sub_agent.rs`, after building `child_task`, seed its context window:

```rust
if let Some(slice) = context_slice {
    let context_manager = self.context_manager.clone();
    let child_ctx_id = child_task.id;
    // Pre-populate the child's context window with the parent's slice.
    context_manager
        .seed_from_slice(child_ctx_id, &slice)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!("failed to seed child context: {}", e);
        });
}
```

- [ ] **Step 3: Add `seed_from_slice` to `ContextManager`**

In `crates/agentos-kernel/src/context.rs`, add:

```rust
/// Pre-populate a new context window with messages from a `ContextSlice`.
/// Called when a sub-agent is spawned with a parent context.
pub async fn seed_from_slice(
    &self,
    task_id: TaskID,
    slice: &agentos_types::ContextSlice,
) -> Result<(), AgentOSError> {
    let mut windows = self.windows.write().await;
    let window = windows.entry(task_id).or_insert_with(|| {
        ContextWindow::new(self.config.max_tokens)
    });
    for entry in &slice.messages {
        window.push(entry.clone());
    }
    Ok(())
}
```

- [ ] **Step 4: Write the test**

```rust
#[tokio::test]
async fn test_child_context_seeded_from_parent_slice() {
    let kernel = setup_kernel().await;
    let agent = register_mock_agent(&kernel, "worker").await;
    let parent_id = TaskID::new();

    // Give the parent a context window with some messages.
    let slice = ContextSlice {
        messages: vec![ContextEntry::user("parent message")],
        label: "test slice".to_string(),
    };

    let resp = kernel
        .cmd_spawn_sub_agent(
            parent_id,
            "worker",
            "do something",
            &["read".to_string()],
            Some(slice),
        )
        .await;

    let child_id = match resp {
        KernelResponse::SubAgentSpawned { child_task_id } => child_task_id,
        other => panic!("expected SubAgentSpawned, got {:?}", other),
    };

    // Child's context should contain the seeded message.
    let child_ctx = kernel.context_manager.get_window(child_id).await.unwrap();
    assert!(child_ctx
        .entries()
        .iter()
        .any(|e| e.content().contains("parent message")));
}
```

- [ ] **Step 5: Run it**

```bash
cargo test -p agentos-kernel test_child_context_seeded_from_parent_slice
```

Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/agentos-bus/src/message.rs \
        crates/agentos-kernel/src/commands/sub_agent.rs \
        crates/agentos-kernel/src/context.rs
git commit -m "feat(kernel): seed child agent context from parent ContextSlice on spawn"
```

---

### Task 3: Inject child result into parent context on completion

**Files:**
- Modify: `crates/agentos-kernel/src/task_completion.rs`
- Modify: `crates/agentos-kernel/src/context.rs` — add `inject_sub_agent_result()`

- [ ] **Step 1: Add `inject_sub_agent_result` to `ContextManager`**

```rust
/// Inject a completed sub-agent's result into the parent task's context window
/// as a synthetic tool call/response pair. This lets the parent LLM reason about
/// the child's output naturally.
pub async fn inject_sub_agent_result(
    &self,
    parent_task_id: TaskID,
    result: &agentos_types::SubAgentResult,
) -> Result<(), AgentOSError> {
    let mut windows = self.windows.write().await;
    let window = match windows.get_mut(&parent_task_id) {
        Some(w) => w,
        None => return Ok(()), // parent context gone — task likely cancelled
    };
    // Inject as a synthetic tool call + response so the LLM sees it naturally.
    window.push(ContextEntry::tool_call(
        "delegate_task",
        serde_json::json!({
            "agent": result.agent_name,
            "task_id": result.child_task_id.to_string(),
        }),
    ));
    window.push(ContextEntry::tool_response(
        "delegate_task",
        serde_json::json!({
            "success": result.success,
            "output": result.output,
        })
        .to_string(),
    ));
    Ok(())
}
```

- [ ] **Step 2: Call it from `task_completion.rs`**

In the task completion handler (the function that runs after a task finishes), add:

```rust
// If this task has a parent, inject our result into the parent's context.
if let Some(parent_id) = completed_task.parent_task_id {
    let result = agentos_types::SubAgentResult {
        child_task_id: completed_task.id,
        agent_name: agent_name.clone(),
        output: output_summary.chars().take(8192).collect(),
        success: completed_task.status == TaskStatus::Completed,
    };
    if let Err(e) = self.context_manager.inject_sub_agent_result(parent_id, &result).await {
        tracing::warn!("failed to inject sub-agent result into parent context: {}", e);
    }
}
```

- [ ] **Step 3: Write the test**

```rust
#[tokio::test]
async fn test_child_result_injected_into_parent_context() {
    let kernel = setup_kernel().await;
    let parent_id = TaskID::new();

    let result = SubAgentResult {
        child_task_id: TaskID::new(),
        agent_name: "worker".to_string(),
        output: "I found the answer: 42".to_string(),
        success: true,
    };

    kernel
        .context_manager
        .inject_sub_agent_result(parent_id, &result)
        .await
        .unwrap();

    let window = kernel.context_manager.get_window(parent_id).await.unwrap();
    let has_result = window
        .entries()
        .iter()
        .any(|e| e.content().contains("I found the answer: 42"));
    assert!(has_result, "child result should be visible in parent context");
}
```

- [ ] **Step 4: Run it**

```bash
cargo test -p agentos-kernel test_child_result_injected_into_parent_context
```

Expected: PASS

- [ ] **Step 5: Run full suite**

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add crates/agentos-kernel/src/context.rs \
        crates/agentos-kernel/src/task_completion.rs
git commit -m "feat(kernel): inject sub-agent results into parent context window on completion"
```

---

## Verification

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```

## Dependencies

- Requires: [[01-sub-agent-spawning]]
- Blocks: [[03-coordination-tools]]
