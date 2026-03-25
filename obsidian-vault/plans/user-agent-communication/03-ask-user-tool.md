---
title: "Phase 3: ask_user Tool + Task Blocking/Resumption"
tags:
  - kernel
  - tools
  - plan
  - phase-3
date: 2026-03-24
status: planned
effort: 2.5d
priority: high
---

# Phase 3: ask_user Tool + Task Blocking/Resumption

> Add the `ask-user` built-in tool, the `WaitingTaskMap` in the kernel, and the response routing that wakes a parked task when the user replies. This is the "human-in-the-loop" primitive that lets agents pause and wait for user input from any channel.

**Depends on**: [[01-user-message-type-and-router]] (Phase 1) and [[02-sse-delivery-and-web-inbox]] (Phase 2 — user needs at least one interactive channel to respond through)
**Blocks**: Nothing (leaf node)

---

## Why This Phase

Every mature agentic framework (LangGraph, CrewAI, mcp-agent, AutoGen) has a first-class primitive for pausing execution and waiting for human input. Without this, agents are forced to either:
- Guess when they're uncertain (bad outcomes)
- Fail and report an error (wasted work)
- Make a blocking escalation that only offers approve/deny (not expressive enough)

The `ask-user` tool gives agents a structured, typed way to ask a question, present options, and receive a real answer — then continue with that context. It is the most important user-facing capability gap in AgentOS today.

---

## Current State vs. Target

| Item | Current | Target |
|------|---------|--------|
| `ask-user` tool | Does not exist | Tool in `agentos-tools` using `AgentTool` trait |
| `WaitingTaskMap` | Does not exist | `HashMap<NotificationID, oneshot::Sender<UserResponse>>` in kernel |
| `TaskState::Waiting` | Exists in enum | Actually used: task parks here until user responds |
| `KernelCommand::RespondToNotification` | Added in Phase 1 | Response routing wakes the correct task |
| `agentctl notifications respond` | Added in Phase 1 (basic) | Enhanced: validates notification exists, is a Question, not expired |
| Web response form | Added in Phase 2 | Wired to actually wake the blocked task |
| `user.interact` permission | Added in Phase 1 | Enforced in tool execution |
| Rate limiter: max 3 concurrent per agent | Added in Phase 1 | Enforced when tool is called |

---

## Critical Architectural Decision: oneshot vs. escalation vs. poll

Three options for how a task waits:

| Approach | Pros | Cons |
|----------|------|------|
| `oneshot::channel` (this design) | Zero CPU while waiting, idiomatic Rust, simple | Non-durable: kernel restart loses the wait |
| Poll `UserInbox` on interval | Simple, durable | Burns executor time, adds latency |
| Re-queue as new `PendingEscalation` | Reuses existing infra, durable | Approval-only semantics; loses typed response |

**Decision**: Use `oneshot::channel` for Phase 3. Durability (surviving kernel restarts) is a Phase 3+ concern. If the kernel restarts while a task is waiting, the task fails with `KernelRestarted` — the agent can retry. This matches the existing escalation behavior (which also loses in-memory state on restart).

---

## Detailed Subtasks

### 3.1 — Create `WaitingTaskMap` in kernel

**File**: `crates/agentos-kernel/src/notification_router.rs`

The `NotificationRouter` already has:
```rust
waiting_tasks: Arc<RwLock<HashMap<NotificationID, oneshot::Sender<UserResponse>>>>,
```
(Defined in Phase 1 spec.) Implement the actual storage and lookup:

```rust
impl NotificationRouter {
    /// Register a oneshot sender for a blocking question.
    /// Called by handle_ask_user BEFORE parking the task.
    pub async fn register_waiter(
        &self,
        notification_id: NotificationID,
        tx: oneshot::Sender<UserResponse>,
    ) {
        self.waiting_tasks.write().await.insert(notification_id, tx);
    }

    /// Called by handle_respond_to_notification.
    /// Sends the user response to the waiting task.
    pub async fn route_response(
        &self,
        notification_id: &NotificationID,
        response: UserResponse,
    ) -> Result<(), AgentOSError> {
        let mut map = self.waiting_tasks.write().await;
        if let Some(tx) = map.remove(notification_id) {
            // If the task already timed out and dropped the receiver, this returns Err — that's fine.
            let _ = tx.send(response);
            Ok(())
        } else {
            Err(AgentOSError::NotFound(format!("No waiting task for notification {notification_id}")))
        }
    }
}
```

---

### 3.2 — Create the `ask-user` tool

**File**: `crates/agentos-tools/src/ask_user.rs` (new file)

This is a **kernel-context tool** — it does not execute sandboxed but instead calls back into the kernel via an in-process callback. (Same pattern as `agent-message`, `task-delegate`, etc. which return `None` from `build_single_tool`.)

```rust
use agentos_types::{
    AgentID, TaskID, TraceID, NotificationPriority, InteractionRequest, UserResponse,
};

pub struct AskUserTool;

impl AgentTool for AskUserTool {
    fn manifest(&self) -> ToolManifest { ... }

    async fn execute(
        &self,
        payload: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, AgentOSError> {
        let input: AskUserInput = serde_json::from_value(payload)?;

        // 1. Validate user.interact permission
        ctx.require_permission("user.interact", PermissionOp::Execute)?;

        // 2. Check rate limit (max 3 concurrent blocking per agent)
        ctx.kernel().notification_router.check_interact_rate_limit(&ctx.agent_id())?;

        // 3. Build UserMessage
        let msg = UserMessage {
            id: NotificationID::new(),
            from: NotificationSource::Agent(ctx.agent_id()),
            task_id: Some(ctx.task_id()),
            trace_id: ctx.trace_id(),
            kind: UserMessageKind::Question {
                question: input.question.clone(),
                options: input.options.clone(),
                free_text_allowed: input.free_text_allowed,
            },
            priority: input.urgency.unwrap_or(NotificationPriority::Urgent),
            subject: format!("Agent needs your input: {}", truncate(&input.question, 60)),
            body: format_question_body(&input),
            interaction: Some(InteractionRequest {
                timeout_secs: input.timeout_secs.unwrap_or(600),
                auto_action: input.auto_action.unwrap_or_else(|| "<auto-denied>".to_string()),
                blocking: input.blocking,
                max_concurrent: 3,
            }),
            delivery_status: HashMap::new(),
            response: None,
            created_at: Utc::now(),
            expires_at: Some(Utc::now() + Duration::from_secs(input.timeout_secs.unwrap_or(600))),
            read: false,
        };

        if input.blocking {
            // 4a. Create oneshot channel
            let (tx, rx) = oneshot::channel::<UserResponse>();

            // 5. Register waiter BEFORE delivering (to avoid race)
            ctx.kernel().notification_router.register_waiter(msg.id.clone(), tx).await;

            // 6. Deliver notification to all channels
            ctx.kernel().notification_router.deliver(msg).await?;

            // 7. Park task: yield control, await user response OR timeout
            let timeout = Duration::from_secs(input.timeout_secs.unwrap_or(600));
            let response = tokio::time::timeout(timeout, rx).await;

            match response {
                Ok(Ok(user_response)) => {
                    // User responded
                    Ok(ToolOutput::text(user_response.text))
                }
                Ok(Err(_)) => {
                    // oneshot sender was dropped (kernel restart edge case)
                    Err(AgentOSError::TaskInterrupted("ask-user waiter dropped".into()))
                }
                Err(_) => {
                    // Timeout — return auto_action text
                    Ok(ToolOutput::text(input.auto_action.unwrap_or_else(|| "<auto-denied>".to_string())))
                }
            }
        } else {
            // 4b. Non-blocking: deliver and return immediately
            ctx.kernel().notification_router.deliver(msg).await?;
            Ok(ToolOutput::text("Notification sent. User will respond asynchronously."))
        }
    }
}

/// JSON input schema for ask-user
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AskUserInput {
    /// The question to ask the user.
    pub question: String,
    /// Context: why is the agent asking? What decision does it face?
    pub context_summary: String,
    /// Specific decision point (e.g. "Should I overwrite the existing config file?")
    pub decision_point: String,
    /// If set, user must pick one of these. If null, free-text response.
    pub options: Option<Vec<String>>,
    /// Whether user can type a custom answer even if options are provided.
    #[serde(default = "default_true")]
    pub free_text_allowed: bool,
    /// Whether the task should pause until user responds (default: true)
    #[serde(default = "default_true")]
    pub blocking: bool,
    /// Seconds to wait before auto_action fires (default: 600 = 10 minutes)
    pub timeout_secs: Option<u64>,
    /// Text returned to agent if timeout expires (default: "<auto-denied>")
    pub auto_action: Option<String>,
    /// Message urgency (default: Urgent)
    pub urgency: Option<NotificationPriority>,
}
```

Tool manifest:
```toml
# Inline in AskUserTool::manifest()
name = "ask-user"
description = "Ask the user a question and optionally wait for their response. Use when the agent needs human input to continue."
permissions_required = ["user.interact"]
executor = "Kernel"   # kernel-context, not sandboxed
```

---

### 3.3 — Register `ask-user` in tool factory

**File**: `crates/agentos-tools/src/factory.rs`

```rust
"ask-user" => Some(Box::new(AskUserTool::new())),
```

Unlike most tools, `AskUserTool` needs a handle to the kernel's `NotificationRouter`. The `ToolExecutionContext` already carries a kernel reference — verify that `ctx.kernel()` is accessible from the tool execution path. If not, pass `Arc<NotificationRouter>` through the context.

---

### 3.4 — Handle `TaskState::Waiting` in task executor

**File**: `crates/agentos-kernel/src/task_executor.rs`

The task executor loop currently drives LLM inference → tool calls → repeat. When `ask-user` is called with `blocking: true`, the `execute()` method above already handles the park via `tokio::time::timeout(rx).await` — the executor is naturally suspended at that await point. No separate state machine change is needed.

However, we need to ensure:
1. When a task is in this suspended await, `task.state` is set to `TaskState::Waiting` (not `Running`).
2. This state change is visible to `agentctl task list`.

Wrap the blocking `ask-user` await:
```rust
// In task_executor.rs, before calling ask-user tool execute():
task.set_state(TaskState::Waiting).await;
let result = tool.execute(payload, &ctx).await;
task.set_state(TaskState::Running).await;
```

`set_state` must also emit a `StatusUpdate` via the bus (wired in Phase 1, subtask 1.9).

---

### 3.5 — Enhance `agentctl notifications respond`

**File**: `crates/agentos-cli/src/commands/notifications.rs`

```
agentctl notifications respond <notification-id> [--text "answer"] [--option "choice"]
```

Validation before sending:
- Fetch notification: if not found → error "Notification not found"
- If `kind != Question` → error "This notification does not require a response"
- If `response.is_some()` → error "Already responded"
- If `expires_at` has passed → error "Question has expired (auto-actioned)"

```rust
pub async fn cmd_respond(bus: &BusClient, id: &str, text: String) -> Result<()> {
    // 1. Fetch notification
    let notif = bus.get_notification(id.parse()?).await?;
    // 2. Validate
    validate_question(&notif)?;
    // 3. Send response
    bus.send_command(KernelCommand::RespondToNotification {
        notification_id: notif.id,
        response_text: text,
        channel: DeliveryChannel::Cli,
    }).await?;
    println!("Response sent.");
    Ok(())
}
```

---

### 3.6 — `notify-user` tool (fire-and-forget companion)

**File**: `crates/agentos-tools/src/notify_user.rs` (new file, simpler than ask-user)

```rust
pub struct NotifyUserTool;

#[derive(Deserialize, JsonSchema)]
pub struct NotifyUserInput {
    pub subject: String,
    pub body: String,
    pub priority: Option<NotificationPriority>,
}
```

This uses `user.notify` permission and creates `UserMessageKind::Notification`. Register in factory:
```rust
"notify-user" => Some(Box::new(NotifyUserTool::new())),
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-tools/src/ask_user.rs` | NEW — AskUserTool impl |
| `crates/agentos-tools/src/notify_user.rs` | NEW — NotifyUserTool impl |
| `crates/agentos-tools/src/factory.rs` | Register ask-user and notify-user |
| `crates/agentos-kernel/src/notification_router.rs` | Implement `register_waiter`, `route_response` |
| `crates/agentos-kernel/src/task_executor.rs` | Set TaskState::Waiting around blocking tool calls |
| `crates/agentos-cli/src/commands/notifications.rs` | Enhance respond subcommand with validation |

---

## Test Plan

```rust
#[tokio::test]
async fn test_ask_user_blocking_receives_response() {
    let mut kernel = setup_kernel().await;
    // spawn task that calls ask-user with blocking=true
    let task_handle = tokio::spawn(async move {
        kernel.run_agent_task("use the ask-user tool to ask: proceed?").await
    });
    // wait for notification to appear in inbox
    tokio::time::sleep(Duration::from_millis(500)).await;
    let notifs = kernel.list_notifications(false, 1).await;
    assert_eq!(notifs.len(), 1);
    assert!(matches!(notifs[0].kind, UserMessageKind::Question { .. }));
    // respond
    kernel.respond_to_notification(notifs[0].id.clone(), "yes".into(), DeliveryChannel::Cli).await.unwrap();
    // task should complete with user's answer
    let result = task_handle.await.unwrap();
    assert!(result.contains("yes"));
}

#[tokio::test]
async fn test_ask_user_timeout_returns_auto_action() {
    // ask-user with timeout=1s and auto_action="<denied>"
    // wait 2 seconds
    // task should get "<denied>" as response, not hang
}

#[tokio::test]
async fn test_ask_user_requires_user_interact_permission() {
    // agent without user.interact permission
    // ask-user → expect PermissionDenied
}

#[tokio::test]
async fn test_ask_user_rate_limit_concurrent_blocking() {
    // spawn agent, send 4 blocking ask-user calls concurrently
    // 4th should fail with RateLimitExceeded (max 3 concurrent)
}

#[tokio::test]
async fn test_notify_user_fire_and_forget() {
    // agent calls notify-user
    // no blocking — returns immediately
    // inbox contains the notification
}

#[tokio::test]
async fn test_task_state_is_waiting_during_ask_user() {
    // spawn task with ask-user blocking=true
    // before responding: check task.state == TaskState::Waiting
    // respond → check task.state == TaskState::Running → Complete
}
```

---

## Verification

```bash
# Build
cargo build -p agentos-tools -p agentos-kernel -p agentos-cli

# Tests
cargo test -p agentos-tools -- ask_user
cargo test -p agentos-kernel -- notification

# Manual test
agentctl kernel start &

# In another terminal: start an agent that uses ask-user
agentctl task run --agent my-agent "ask the user if you should continue, then report their answer"

# Agent should show as Waiting in task list
agentctl task list
# status: Waiting

# Respond from CLI
agentctl notifications list
agentctl notifications respond <id> --text "yes, continue"

# Task should complete
agentctl task list
# status: Complete

# Task output should include the user's answer
agentctl task logs <task-id>
```
