---
title: User-Agent Communication Research
tags:
  - kernel
  - tools
  - research
  - plan
date: 2026-03-24
status: complete
effort: 1d
priority: high
---

# User-Agent Communication Research

> Synthesis of NotebookLM research on human-in-the-loop patterns, agent notification systems, and production bidirectional communication design — applied to AgentOS.

---

## 1. Human-in-the-Loop Architectural Patterns

Research from the AI Agent Frameworks & AgentOS 2025–2026 notebook identifies four primary HITL communication patterns used in production systems:

### Interrupt-Driven (Escalation)
The agent triggers a "pause point" when it needs human input. Execution is blocked (or delegated to other tasks) until a human resolves the interrupt.
- **AgentOS today**: `PendingEscalation` with `blocking: bool` and 5-minute auto-deny
- **LangGraph**: checkpoint-based graph pause — saves exact graph state, agent uses zero compute while waiting, resumes byte-for-byte identical after human edits the planned action
- **Key insight**: The checkpoint/state-save pattern (LangGraph) is strictly superior to a simple blocking lock for long-running tasks. The task can survive a kernel restart.

### Event-Sourcing + Pub/Sub
All state changes are expressed as events. Human-facing consumers subscribe to relevant event types.
- **AgentOS today**: EventBus with typed subscriptions. `TaskCompleted`, `TaskFailed`, `BudgetWarning` etc. are all event types — but no consumer delivers them to a human channel.
- **Gap**: A delivery adapter that maps `EventType` → `UserMessage` is missing.

### Polling
Human or agent periodically checks a status flag.
- **AgentOS today**: `agentctl escalation list` and `agentctl task list` — pure polling
- **Verdict**: Polling is the worst UX. It is the current default. Replace with push.

### Graph-Based State Persistence (LangGraph pattern)
Agent workflow is a DAG with named checkpoints. Human-review is a named node in the graph. Agent suspends at the node, human modifies state, graph resumes.
- **AgentOS equivalent**: `TaskState::Waiting` + serialized context snapshot. The `ContextManager` already has `checkpoint()`. Wiring `Waiting` tasks to a durable wait for user response implements this pattern.

### Winner for AgentOS
**Interrupt-driven + event-sourced delivery** with **durable task suspension** (LangGraph-style). The task parks at `TaskState::Waiting`, the event bus fires a `UserQuestionPending` event, the NotificationRouter delivers via all active channels, and the user responds from any channel.

---

## 2. How Leading Frameworks Signal Task Completion & Notifications

| Framework | Notification Mechanism | Human Feedback | Task Completion Signal |
|-----------|----------------------|----------------|----------------------|
| **LangGraph** | Checkpointed graph state; UI polls checkpoint store | Human edits graph state at checkpoint node, resumes | Graph reaches terminal node; callbacks fire |
| **CrewAI Flows** | Flow manager delegates to Crew; Crew returns to Flow | `HumanInputTool` in task pipeline; Flow waits at step | Flow.kickoff() returns when all Crew tasks complete |
| **AutoGen / AG2** | User Proxy Agent in conversation loop | Natural-language dialogue; human types in console or WebSocket | Termination condition met in conversation (e.g. `TERMINATE` keyword) |
| **PydanticAI** | Structured validation flags tool calls for human review before execution | Approval checkpoint before tool execution | Task result returned as validated Pydantic model |
| **Google ADK** | Bidirectional streaming for audio/video channels | Maintains open streaming session; human can interrupt at any time | Session end event |
| **mcp-agent** | Durable execution with automatic resume on API failure | MCP tool: `human_input` returns user response | Tool chain completion + callback |

### Key Takeaways for AgentOS
1. **Every mature framework has a named "ask human" primitive** — AutoGen calls it "User Proxy", CrewAI calls it "Human Input Tool", mcp-agent calls it `human_input`. AgentOS needs `ask-user`.
2. **Structured responses beat natural language** — PydanticAI's validation approach means the agent always gets a typed answer, not a raw string it has to parse.
3. **The user doesn't need to know they're talking to a tool** — the notification/question can be surfaced in chat, web UI, or CLI with identical underlying protocol.
4. **Completion is an event, not a poll** — all frameworks emit a terminal event/callback. AgentOS already has `TaskCompleted` in the event bus — it just needs to route it to the user.

---

## 3. Ask_User Design Principles

From research synthesis across LangGraph, CrewAI, mcp-agent, and PydanticAI:

### Structural Requirements
- **Context, not just question**: Agent must provide why it's asking, what decision point it's at, and what options exist. The user should be able to respond intelligently without reading the full task history.
- **Structured options or free text**: Multiple-choice responses are better than free text when the options are enumerable. The tool should accept both: `options: Option<Vec<String>>`.
- **Timeout is mandatory**: Every interactive request must have an auto-action fallback. Agents must not be able to block indefinitely.
- **Blocking vs. non-blocking**: Some questions need an answer before the agent can continue (blocking). Others are "I'll continue but would appreciate feedback" (non-blocking, fire-and-forget with optional reply).

### Anti-Patterns to Avoid
- **Vague questions**: "What should I do?" — rejected; must have `context_summary` and `decision_point`
- **Infinite blocking**: Auto-deny/approve after configurable timeout (default 10 min)
- **Notification spam**: `user.interact` permission required; rate-limited (max 3 active blocking questions per agent)
- **Channel lock-in**: User should be able to respond from any channel, not forced back to the originating channel

### Recommended ask_user Data Model (from research)
Based on what leads to the most actionable human responses:
```
{
  question:         String,           // The actual question
  context_summary:  String,           // Why the agent is asking (1-3 sentences)
  decision_point:   String,           // What choice the agent faces
  options:          Option<Vec<String>>,  // If enumerable; null = free text
  urgency:          Low | Normal | High | Critical,
  timeout:          Duration,         // How long to wait
  auto_action:      String,           // What happens if no response
  blocking:         bool,             // Halt task until answered?
}
```

---

## 4. Notification Delivery: Technology Trade-offs

### Server-Sent Events (SSE) — Recommended for Web
- **Pros**: Unidirectional push over HTTP/1.1, works through proxies, already implemented in agentos-web (chat.rs), simple reconnect logic, no WebSocket upgrade needed
- **Cons**: Unidirectional (client cannot send via SSE — needs a separate HTTP POST for replies)
- **Verdict**: Best fit for web notification delivery. SSE for push, HTMX POST for responses.

### WebSockets — Overkill for now
- **Pros**: Bidirectional, low latency
- **Cons**: More complex state management, not yet in agentos-web, firewall issues in some corporate environments
- **Verdict**: Future option if real-time bidirectional chat requires it. Not needed for Phase 2.

### Webhooks — Recommended for External Systems
- **Pros**: Simple, universally supported, stateless, can target Slack, Discord, PagerDuty, anything with an HTTP endpoint
- **Cons**: No acknowledgment by default, must implement retry + SSRF protection
- **Verdict**: The escalation system already has SSRF-safe webhook delivery. Reuse and generalize.

### Desktop Notifications (notify-rust)
- **Pros**: Native OS integration on Linux, zero network overhead, user sees it even if not at terminal or browser
- **Cons**: Requires desktop environment, no response path (user must open CLI/web to respond)
- **Verdict**: Great for task completion signals; not suitable for interactive questions.

### CLI Long-Poll / Push
- **Pros**: Works in headless environments, consistent with existing agentctl UX
- **Cons**: User must have a terminal open, no push (must poll unless we implement a watch command)
- **Verdict**: Implement `agentctl notifications watch` (streaming) in addition to `list`.

### Prioritized Delivery Strategy
For a **local-first, privacy-first** system (from research):
1. **Always write to User Inbox first** (SQLite, survives restarts)
2. **Then deliver to all active adapters in parallel** (SSE if web session open, desktop if available, webhook if configured)
3. **No adapter is authoritative** — any adapter failure is non-fatal; inbox is the ground truth

---

## 5. Channel-Agnostic Unified API Patterns

### MCP as Universal Bus (from research)
The Model Context Protocol provides a JSON-RPC 2.0 interface that acts as a "universal USB-C port." For AgentOS, this suggests: define the notification protocol in terms of an MCP-compatible tool signature, making future MCP integration trivial.

### Infobip-Style Orchestration Layer (from research)
Production messaging systems (Infobip AgentOS, Twilio) abstract 15+ channels behind a single API. The key pattern:
- **Unified send API**: `send(message: UserMessage, channels: Vec<Channel>)`
- **Channel abstraction**: each channel implements the same interface
- **Fallback chain**: try primary channel, fall back to secondary if unavailable
- **Priority routing**: urgent messages try all channels simultaneously; low-priority tries cheapest first

### Applied to AgentOS
```
NotificationRouter::deliver(msg: UserMessage)
  → determine priority-ordered channel list from config
  → write to UserInbox (SQLite) — always first
  → for each adapter in parallel:
      adapter.deliver(&msg).await
  → track delivery_status per channel in UserMessage
```

---

## 6. User Response → Agent Feedback Loop

The reverse channel (user → agent) is equally important. From research:

### Patterns
- **LangGraph**: user edits checkpoint state directly; graph reads modified state on resume
- **CrewAI**: user response injected as task context for next step
- **AutoGen**: user types in conversation; User Proxy Agent forwards to assistant
- **mcp-agent**: `human_input` tool returns user string to calling agent

### For AgentOS
User response travels this path:
```
User types response (any channel)
  → CLI: `agentctl respond <notification-id> "answer"`
  → Web: HTMX POST to /notifications/{id}/respond
  → Webhook: inbound HTTP POST (future)
    ↓
  KernelCommand::RespondToNotification { id, response_text, channel }
    ↓
  ResponseRouter (kernel)
    → find matching UserMessage in Inbox
    → if blocking (task in TaskState::Waiting):
        → send via oneshot::Sender<UserResponse>
        → task executor wakes, returns UserResponse as tool output
    → if non-blocking:
        → create AgentMessage to the originating agent's inbox
        → emit UserResponseReceived event
    → write audit log: UserResponseReceived
```

The `oneshot::channel` is the key mechanism — it's the Rust-idiomatic way to "park a task waiting for one response" without spinning a thread.

---

## Summary: Architectural Principles for AgentOS UNIS

1. **Inbox-first**: Always persist to SQLite inbox before attempting delivery. Delivery is best-effort; inbox is the source of truth.
2. **Channel-agnostic dispatch**: `NotificationRouter` is the single dispatcher; adapters are plugins. The kernel never hard-codes a channel.
3. **Interrupt-driven with durable parking**: Blocking questions park the task in `TaskState::Waiting` with a `oneshot` receiver — zero CPU, survives restarts if paired with context snapshot.
4. **Structured interaction model**: `UserMessage` carries full context (question, decision point, options, urgency, timeout, auto-action). User never needs to ask "what's this about?"
5. **Response from any channel**: User can respond via CLI, web, or future mobile — kernel routes response to the waiting task regardless of which channel received it.
6. **Security-gated**: `user.notify` for notifications, `user.interact` for blocking questions. Explicit grant required.
7. **Fully audited**: `NotificationSent`, `NotificationDelivered`, `NotificationRead`, `UserResponseReceived` events in audit log.
8. **Timeout + auto-action on every interactive request**: Prevents hung tasks. Default 10 min → auto-deny.

---

## Related

- [[User-Agent Communication Plan]] — master plan
- [[User-Agent Communication Data Flow]] — flow diagrams
- [[01-user-message-type-and-router]] — Phase 1
