---
title: "Phase 2: State Serialization"
tags:
  - kernel
  - durability
  - plan
date: 2026-04-03
status: complete
effort: 2d
priority: high
---

# Phase 2: State Serialization

> Write task checkpoints at every tool-call boundary in the task executor, serializing `ContextWindow` and `AgentTask` to JSON and encrypting the context blob with `agentos-vault` before persisting to the `CheckpointStore`.

---

## Why This Phase

The `CheckpointStore` from Phase 1 provides the persistence layer, but nothing writes to it yet. This phase wires checkpoint writing into the task execution loop at the natural transaction boundary: after each tool call completes. This captures:

- The full `ContextWindow` (all entries, including tool results)
- The `AgentTask` metadata (state, iteration count, prompt)
- The tool call history (which tools were called, in what order)

The context blob is encrypted because agent context windows can contain user secrets, API keys, and sensitive data from tool results.

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `ContextWindow` serialization | `#[derive(Serialize, Deserialize)]` already present | Serialized to JSON and encrypted with vault key at tool-call boundaries |
| `AgentTask` serialization | `#[derive(Serialize, Deserialize)]` already present (partial -- `CapabilityToken` serializes) | Full JSON serialization of task metadata for checkpoint |
| Checkpoint trigger | None | After every tool-call execution in `task_executor.rs` |
| Encryption | Not applied to task state | AES-256-GCM via `agentos-vault` proxy |
| Tool call history | Not tracked per-task | `Vec<ToolCallRecord>` accumulated during execution, checkpointed |

## What to Do

### 1. Define `ToolCallRecord`

Open `crates/agentos-types/src/task.rs`. Add:

```rust
/// Record of a single tool call during task execution, for checkpoint replay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub tool_name: String,
    pub tool_call_id: Option<String>,
    pub input_json: String,
    pub output_json: String,
    pub called_at: chrono::DateTime<chrono::Utc>,
    pub duration_ms: u64,
    pub success: bool,
}
```

Re-export from `crates/agentos-types/src/lib.rs`:

```rust
pub use task::{
    // ... existing exports ...
    ToolCallRecord,
};
```

### 2. Accumulate tool call history in the executor

Open `crates/agentos-kernel/src/task_executor.rs`. In the `execute_task_sync` method, add a mutable `Vec<ToolCallRecord>` at the start of the execution loop:

```rust
let mut tool_call_history: Vec<ToolCallRecord> = Vec::new();
```

After each tool call completes (there is a section where tool results are processed and pushed to context), record the call:

```rust
tool_call_history.push(ToolCallRecord {
    tool_name: tool_name.to_string(),
    tool_call_id: call_id.clone(),
    input_json: serde_json::to_string(&tool_input).unwrap_or_default(),
    output_json: serde_json::to_string(&tool_result).unwrap_or_default(),
    called_at: chrono::Utc::now(),
    duration_ms: tool_start.elapsed().as_millis() as u64,
    success: tool_result.get("error").is_none(),
});
```

### 3. Write checkpoint after each tool call

After the tool call record is added, write a checkpoint:

```rust
if let Some(ref cp_store) = self.checkpoint_store {
    let context = self.context_manager.get_context(&task.id).await?;
    let context_json = serde_json::to_vec(&context).map_err(|e| {
        anyhow::anyhow!("checkpoint: failed to serialize context: {e}")
    })?;

    // Encrypt context blob if vault is available.
    let context_blob = if let Some(ref vault) = self.vault {
        vault.encrypt_bytes(&context_json).await.unwrap_or(context_json)
    } else {
        context_json
    };

    let task_state_json = serde_json::to_string(&CheckpointTaskState {
        task_id: task.id,
        agent_id: task.agent_id,
        state: task.state.clone(),
        original_prompt: task.original_prompt.clone(),
        iteration: iteration_count,
        parent_task_id: task.parent_task_id,
        spawn_depth: task.spawn_depth,
        autonomous: task.autonomous,
    }).unwrap_or_default();

    let history_json = serde_json::to_string(&tool_call_history).unwrap_or_default();

    let record = CheckpointRecord {
        id: uuid::Uuid::new_v4(),
        task_id: task.id,
        agent_id: task.agent_id,
        step_num: tool_call_history.len() as u32,
        created_at: chrono::Utc::now(),
        context_blob,
        task_state_json,
        tool_call_history_json: history_json,
    };

    if let Err(e) = cp_store.write(&record).await {
        tracing::warn!(
            task_id = %task.id,
            step = record.step_num,
            error = %e,
            "checkpoint write failed -- task continues without checkpoint"
        );
    }
}
```

### 4. Define `CheckpointTaskState`

Open `crates/agentos-kernel/src/checkpoint_store.rs`. Add:

```rust
use agentos_types::{AgentID, TaskID, TaskState};

/// Lightweight serializable snapshot of task metadata for checkpointing.
/// Excludes `CapabilityToken` (re-issued on resume) and `ContextWindow` (stored separately).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointTaskState {
    pub task_id: TaskID,
    pub agent_id: AgentID,
    pub state: TaskState,
    pub original_prompt: String,
    pub iteration: u32,
    pub parent_task_id: Option<TaskID>,
    pub spawn_depth: u8,
    pub autonomous: bool,
}
```

### 5. Delete checkpoint on normal task completion

In the async task completion handler (from [[01-async-task-execution]]), after storing the result:

```rust
// Clean up checkpoint on normal completion (no resume needed).
if let Some(ref cp_store) = kernel.checkpoint_store {
    if let Err(e) = cp_store.delete_for_task(&task_clone.id).await {
        tracing::warn!(
            task_id = %task_clone.id,
            error = %e,
            "failed to delete checkpoint after task completion"
        );
    }
}
```

### 6. Add vault encryption helper

If `agentos-vault` does not already expose a byte-level encrypt method, add to `crates/agentos-vault/src/lib.rs`:

```rust
/// Encrypt arbitrary bytes with the vault's master key.
pub async fn encrypt_bytes(&self, plaintext: &[u8]) -> Result<Vec<u8>, AgentOSError> {
    // Use AES-256-GCM with a random nonce, prepend nonce to ciphertext.
    // ... (follows existing vault encryption pattern)
}

/// Decrypt bytes encrypted by `encrypt_bytes`.
pub async fn decrypt_bytes(&self, ciphertext: &[u8]) -> Result<Vec<u8>, AgentOSError> {
    // ... 
}
```

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-types/src/task.rs` | Add `ToolCallRecord` struct |
| `crates/agentos-types/src/lib.rs` | Re-export `ToolCallRecord` |
| `crates/agentos-kernel/src/task_executor.rs` | Add `tool_call_history` accumulation; write checkpoint after each tool call |
| `crates/agentos-kernel/src/checkpoint_store.rs` | Add `CheckpointTaskState` struct |
| `crates/agentos-vault/src/lib.rs` | Add `encrypt_bytes()` / `decrypt_bytes()` if not already present |

## Prerequisites

[[01-checkpoint-schema-and-storage]] must be complete first -- this phase writes to the `CheckpointStore`.

## Test Plan

- **Unit test `test_checkpoint_written_after_tool_call`:** Set up a kernel with `CheckpointStore`, execute a task with a mock tool that returns a result. Assert `get_latest` returns a checkpoint with `step_num == 1`.
- **Unit test `test_checkpoint_includes_context`:** Write a checkpoint, deserialize the `context_blob` (skip encryption in test by using `None` vault), assert the deserialized `ContextWindow` has the expected entry count.
- **Unit test `test_checkpoint_deleted_on_completion`:** Execute a task to completion. Assert `get_latest` returns `None` for that task.
- **Unit test `test_tool_call_history_serialization`:** Serialize and deserialize a `Vec<ToolCallRecord>`, assert round-trip fidelity.
- **Unit test `test_checkpoint_task_state_serialization`:** Serialize and deserialize `CheckpointTaskState`, assert all fields match.

## Verification

```bash
cargo build -p agentos-kernel -p agentos-types -p agentos-vault
cargo test -p agentos-kernel -- checkpoint --nocapture
cargo test -p agentos-types -- --nocapture
cargo clippy -p agentos-kernel -p agentos-types -p agentos-vault -- -D warnings
cargo fmt --all -- --check
```
