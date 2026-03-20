---
title: Chat Integration Tests
tags:
  - web
  - testing
  - v3
  - next-steps
date: 2026-03-18
status: planned
effort: 1d
priority: medium
---

# Chat Integration Tests

> Write end-to-end integration tests for the complete chat system: tool execution loop, SSE streaming, session management, task assignment, and error handling, using `MockLLMCore` and an in-memory test server.

---

## Why This Subtask

Subtasks 28-01 through 28-06 add significant new functionality spanning the kernel and web crates. Integration tests verify the components work together correctly across crate boundaries, catch regressions, and serve as executable documentation.

The codebase testing patterns use:
- `MockLLMCore` from `crates/agentos-llm/src/mock.rs` for deterministic LLM responses.
- `tempfile::TempDir` for filesystem isolation.
- `tower::ServiceExt` for testing Axum routes without a running server.
- `serial_test` for tests that share global state.

---

## Current State -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Chat integration tests | None | 8 integration tests in `crates/agentos-web/tests/chat_integration.rs` |
| Tool loop coverage | None | Test with MockLLM returning tool call then answer |
| SSE coverage | None | Test event sequence from stream endpoint |
| Error path coverage | None | Test LLM failure, unknown tool, max iterations |
| Session management coverage | None | Test deletion, pagination |
| Store migration coverage | None | Test idempotent migration |

---

## What to Do

### Step 1: Create test file

Create `crates/agentos-web/tests/chat_integration.rs`.

### Step 2: Write test setup helper

The setup function needs to:
1. Create a `TempDir`.
2. Boot a minimal kernel with `MockLLMCore`.
3. Register a test agent.
4. Create an `AppState` with the kernel.
5. Build the router.
6. Return the router and `AppState` for direct handler testing.

```rust
use agentos_kernel::Kernel;
use agentos_llm::MockLLMCore;
use agentos_web::chat_store::ChatStore;
use agentos_web::AppState;
use std::sync::Arc;
use tempfile::TempDir;

struct TestHarness {
    state: AppState,
    tmp: TempDir,
}

async fn setup() -> TestHarness {
    let tmp = TempDir::new().unwrap();
    // Build a minimal kernel config pointing to tmp dir.
    // Register MockLLMCore with predefined responses.
    // Register a test agent.
    // Create ChatStore at tmp/chat.db.
    // Build AppState.
    todo!("Depends on Kernel::boot() API -- consult kernel.rs for the exact setup")
}
```

Note: The exact kernel setup depends on `Kernel::boot()` which reads `config/default.toml` and initializes many subsystems. For integration tests, consider using the test helper patterns from `crates/agentos-kernel/tests/` if they exist, or use a minimal config with the test data directory.

### Step 3: Write the integration tests

1. **`test_chat_tool_call_executed_and_stored`**

```rust
#[tokio::test]
async fn test_chat_tool_call_executed_and_stored() {
    // Setup MockLLMCore:
    //   Call 1: returns "```json\n{\"tool\": \"agent-manual\", \"intent_type\": \"query\", \"payload\": {\"section\": \"index\"}}\n```"
    //   Call 2: returns "Here is the manual index: ..."
    // Create a new chat session via the handler.
    // Verify:
    //   - 3 messages stored: user, tool, assistant
    //   - tool message has tool_name == "agent-manual"
    //   - assistant message contains the plain text answer, not JSON
}
```

2. **`test_chat_no_tool_call_direct_answer`**

```rust
#[tokio::test]
async fn test_chat_no_tool_call_direct_answer() {
    // MockLLMCore returns plain text immediately.
    // Create session, verify 2 messages: user + assistant.
    // Verify no tool messages.
}
```

3. **`test_chat_max_iterations_reached`**

```rust
#[tokio::test]
async fn test_chat_max_iterations_reached() {
    // MockLLMCore always returns a tool call JSON block (all 10 iterations).
    // Create session.
    // Verify answer contains "Maximum tool call limit reached".
    // Verify exactly 10 tool messages stored.
}
```

4. **`test_chat_tool_error_handled`**

```rust
#[tokio::test]
async fn test_chat_tool_error_handled() {
    // MockLLMCore calls a nonexistent tool on iteration 1, then returns plain text.
    // Verify tool message content contains "error".
    // Verify final answer is plain text (iteration 2 result).
}
```

5. **`test_chat_sse_event_sequence`**

```rust
#[tokio::test]
async fn test_chat_sse_event_sequence() {
    // Create session and send a message.
    // Connect to /chat/{id}/stream.
    // Collect SSE events.
    // Verify: Thinking -> (ToolStart -> ToolResult ->) -> Done
    // Verify Done event contains the answer.
}
```

6. **`test_chat_session_deletion`**

```rust
#[tokio::test]
async fn test_chat_session_deletion() {
    // Create session, add messages.
    // DELETE /chat/{id}.
    // Verify GET /chat/{id} returns 404.
    // Verify get_messages returns empty.
}
```

7. **`test_chat_store_migration_idempotent`**

```rust
#[test]
fn test_chat_store_migration_idempotent() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("chat.db");
    // Open ChatStore, add a message with all fields.
    {
        let store = ChatStore::open(&path).unwrap();
        store.create_session_with_first_message("agent", "hello").unwrap();
    }
    // Open again.
    {
        let store = ChatStore::open(&path).unwrap();
        let sessions = store.list_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
    }
}
```

8. **`test_chat_task_assignment`**

```rust
#[tokio::test]
async fn test_chat_task_assignment() {
    // Send a message starting with "/task summarize report".
    // Verify a task message (role='task') is stored.
    // Verify task_id is populated.
    // Verify a task exists in the kernel scheduler.
}
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-web/tests/chat_integration.rs` | New file with 8 integration tests |

---

## Prerequisites

All previous subtasks (28-01 through 28-06) must be complete.

---

## Test Plan

- `cargo test -p agentos-web -- chat_integration --nocapture` must pass all 8 tests.
- No tests hit real LLM APIs -- all use `MockLLMCore`.
- No tests leave temporary files -- all use `TempDir`.
- Tests must not interfere with each other (no shared state).

---

## Verification

```bash
cargo test -p agentos-web -- chat_integration --nocapture
cargo clippy -p agentos-web -- -D warnings
```
