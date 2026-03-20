---
title: Chat Interface
tags:
  - web
  - llm
  - tools
  - v3
  - next-steps
date: 2026-03-18
status: planned
effort: 10d
priority: high
---

# Chat Interface

> Fix tool call execution in the web chat and build a full-featured streaming chat interface with inline tool activity, task assignment, and agent selection.

---

## Current State

The chat interface at `crates/agentos-web/` has basic session management and message persistence, but the LLM's tool call responses are shown as raw JSON instead of being executed. The UI uses synchronous POST-redirect-GET with no streaming or real-time feedback.

## Goal / Target State

A production chat interface where:
- Tool calls are automatically detected, executed by the kernel, and re-inferred until a natural-language answer is produced.
- Responses stream via SSE with inline tool activity indicators.
- Users can assign kernel tasks from chat and see their status inline.
- Sessions can be searched, deleted, and switched without leaving the conversation.

## Sub-tasks

| # | Task | Files | Status |
|---|------|-------|--------|
| 01 | [[28-01-Add Chat Tool Execution Loop to Kernel]] | `kernel.rs`, `chat.rs` | planned |
| 02 | [[28-02-Extend ChatStore Schema for Tool Metadata]] | `chat_store.rs`, `chat.rs` | planned |
| 03 | [[28-03-Add Chat SSE Streaming Endpoint]] | `kernel.rs`, `chat.rs`, `router.rs`, `Cargo.toml` | planned |
| 04 | [[28-04-Rewrite Chat Conversation Template with HTMX]] | `chat_conversation.html`, `chat_message.html`, `templates.rs` | planned |
| 05 | [[28-05-Add Chat Session Management Features]] | `chat_store.rs`, `chat.rs`, `router.rs`, `chat.html` | planned |
| 06 | [[28-06-Add Task Assignment from Chat]] | `chat_store.rs`, `chat.rs`, `tasks.rs`, `router.rs` | planned |
| 07 | [[28-07-Chat Integration Tests]] | `tests/chat_integration.rs` | planned |

## Verification

```bash
cargo build -p agentos-kernel -p agentos-web
cargo test -p agentos-kernel -- chat --nocapture
cargo test -p agentos-web -- --nocapture
cargo clippy -p agentos-kernel -p agentos-web -- -D warnings
cargo fmt --all -- --check
```

## Related

- [[Chat Interface Plan]] -- master plan with architecture, design decisions, and risk analysis
- [[01-chat-tool-execution-loop]] -- Phase 01 detail
- [[02-chat-store-tool-metadata]] -- Phase 02 detail
- [[03-chat-sse-streaming-endpoint]] -- Phase 03 detail
- [[04-chat-conversation-template-htmx]] -- Phase 04 detail
- [[05-chat-agent-selection-and-history]] -- Phase 05 detail
- [[06-chat-task-assignment-from-chat]] -- Phase 06 detail
- [[07-chat-integration-tests]] -- Phase 07 detail
