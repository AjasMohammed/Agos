---
title: Chat Integration Tests
tags:
  - web
  - testing
  - v3
  - plan
date: 2026-03-18
status: planned
effort: 1d
priority: medium
---

# Phase 07 -- Chat Integration Tests

> Write end-to-end integration tests for the chat system covering the tool execution loop, SSE streaming, error handling, and persistence, using `MockLLMCore` and an in-memory test server.

---

## Why This Phase

Phases 01-06 add significant new functionality (tool loop, SSE streaming, task assignment, schema migrations). Integration tests verify the components work together correctly, catch regressions, and serve as executable documentation. The codebase already uses `MockLLMCore` for deterministic testing and `tempfile` for filesystem isolation.

---

## Current State -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Chat-specific tests | None | 8+ integration tests covering all major paths |
| Tool loop test | None | MockLLM returns tool call, then answer; verify both stored |
| SSE test | None | Connect to SSE endpoint; verify event sequence |
| Error path test | None | MockLLM fails; verify error event and message |
| Store migration test | None | Open DB twice; verify idempotent migration |
| Concurrent send test | None | Two concurrent sends; verify no data corruption |

---

## What to Do

### Step 1: Create the test module

Create `crates/agentos-web/tests/chat_integration.rs`. This file will:

1. Set up a test kernel with `MockLLMCore`.
2. Create a `WebServer` or directly test the router using `tower::ServiceExt`.
3. Exercise the chat endpoints.

### Step 2: Write test helpers

```rust
use agentos_kernel::Kernel;
use agentos_llm::MockLLMCore;
use agentos_web::{AppState, WebServer};
use tempfile::TempDir;

async fn setup_test_app() -> (AppState, TempDir) {
    let tmp = TempDir::new().unwrap();
    // ... build kernel with MockLLMCore, create AppState ...
}
```

### Step 3: Write the tests

1. **`test_chat_tool_call_executed_and_stored`**
   - Configure `MockLLMCore` to return a tool call JSON block on the first `infer()` call, then a plain text answer on the second.
   - POST to `/chat/new` with a message.
   - Verify the response redirects to a session page.
   - GET the session page, verify:
     - User message is stored.
     - Tool call record is stored (role = "tool").
     - Assistant message contains the plain text answer, not the JSON block.

2. **`test_chat_no_tool_call_direct_answer`**
   - Configure `MockLLMCore` to return plain text immediately.
   - POST to `/chat/new`, GET the session.
   - Verify only user + assistant messages exist, no tool records.

3. **`test_chat_max_tool_iterations`**
   - Configure `MockLLMCore` to always return a tool call.
   - POST to `/chat/new`.
   - Verify the assistant message contains "Maximum tool call limit reached".
   - Verify exactly 10 tool records are stored.

4. **`test_chat_tool_error_handled`**
   - Configure `MockLLMCore` to return a call to a nonexistent tool, then a plain answer.
   - POST to `/chat/new`.
   - Verify the tool record contains an error, and the LLM got a second chance to answer.

5. **`test_chat_sse_event_sequence`**
   - POST a message, then connect to the SSE stream endpoint.
   - Collect events until `chat-done`.
   - Verify sequence: `chat-thinking` -> (`chat-tool-start` -> `chat-tool-result` ->) -> `chat-done`.

6. **`test_chat_session_deletion`**
   - Create a session with messages.
   - DELETE the session.
   - Verify GET returns 404.

7. **`test_chat_store_migration_idempotent`**
   - Open `ChatStore` on the same path twice.
   - Verify no errors and version is correct.

8. **`test_chat_concurrent_sends`**
   - Create a session.
   - Send two messages concurrently (tokio::join!).
   - Verify both messages and both responses are stored in correct order.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-web/tests/chat_integration.rs` | New file with 8+ integration tests |

---

## Dependencies

All previous phases (01-06) must be complete.

---

## Test Plan

- `cargo test -p agentos-web -- chat_integration --nocapture` must pass all 8 tests.
- No tests should hit real LLM APIs (all use `MockLLMCore`).
- No tests should leave temporary files behind (all use `tempfile::TempDir`).

---

## Verification

```bash
cargo test -p agentos-web -- chat_integration --nocapture
cargo clippy -p agentos-web -- -D warnings
```
