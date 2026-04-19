---
title: "Agent Conversation Chat Plan"
tags:
  - kernel
  - web
  - agents
  - plan
date: 2026-04-17
status: in-progress
effort: 2d
priority: high
---

# Agent Conversation Chat Plan

> Enable multiple agents to converse with each other in a real-time chat interface observable by the user, with full streaming of text, tool calls, and thinking.

---

## Why This Matters

When a user tells an agent to "conduct a debate" or "discuss X with agent B", the agent responds once and stops — there is no mechanism for continuous back-and-forth agent communication. Users want to see agents interact seamlessly like humans: responding to each other, using tools, reasoning out loud.

## Current State

| Component | Status |
|-----------|--------|
| `AgentMessageBus` | Direct/group/broadcast send, no turn-taking loop |
| Web `/chat` | User↔single-agent, streaming SSE works well |
| Web `/a2a` | Static history view only |
| Teams/sub-agents | Spawn-and-await, no live conversational loop |

## Target Architecture

```
User → POST /agent-chat/new (topic, participants, max_turns)
          ↓
  ConvoOrchestrator (spawned tokio task)
    ├── Turn 1: kernel.chat_infer_streaming(agent_A, ...)
    │     → SSE: TurnStart, TextChunk, ToolStart, ToolResult, TurnEnd
    ├── Turn 2: kernel.chat_infer_streaming(agent_B, ...)
    │     → SSE: TurnStart, TextChunk, ..., TurnEnd
    └── ...until max_turns or stopped
          ↓ ConversationDone
  ConvoStore (SQLite) ← persists each turn
  Browser ← SSE stream renders real-time chat bubbles
```

## Phase Overview

| Phase | Name | Effort | Dependencies | Status |
|-------|------|--------|--------------|--------|
| 1 | ConvoStore + ConvoStreamEvent | 0.5d | None | planned |
| 2 | ConvoInFlight + Orchestrator | 0.5d | Phase 1 | planned |
| 3 | HTTP handlers | 0.5d | Phase 2 | planned |
| 4 | Templates + JS | 0.5d | Phase 3 | planned |

## Key Design Decisions

1. **Web-layer orchestration** — no kernel changes needed; the orchestrator calls `kernel.chat_infer_streaming` for each turn, sequentially, in a detached tokio task.
2. **Stateless history building** — each agent's turn receives the full prior conversation as a flat prompt, not via `history` pairs. Simple and avoids role-perspective confusion.
3. **ConvoStreamEvent wraps ChatStreamEvent** — the SSE protocol adds `agent` and `turn` fields to all events so the browser knows which agent is speaking.
4. **SQLite persistence** — each completed turn is saved immediately; the UI page loads prior turns on refresh.
5. **Single SSE stream per conversation** — one `GET /agent-chat/{id}/stream` endpoint serves the entire conversation, replaying buffered events for late subscribers.

## Risks

| Risk | Mitigation |
|------|-----------|
| Agent offline mid-conversation | Emit `Error` event, mark convo `error`, show in UI |
| Long conversations accumulate events | Cap buffer at 10,000 events (coalesce old TextChunks) |
| Both agents agree and stop early | Max-turns limit + natural "conversation complete" detection |
