---
title: User-Agent Communication Data Flow
tags:
  - kernel
  - tools
  - flow
  - plan
date: 2026-03-24
status: planned
effort: 0.5d
priority: high
---

# User-Agent Communication Data Flow

> Visual and narrative walkthrough of the two core flows: agent→user notification delivery, and user→agent response routing.

---

## Flow A: Agent Sends Notification (Fire-and-Forget)

```mermaid
sequenceDiagram
    participant Agent as Agent (task executor)
    participant Tool as notify-user tool
    participant Kernel as Kernel / NotificationRouter
    participant Inbox as UserInbox (SQLite)
    participant CLI as CLI Adapter
    participant SSE as SSE/Web Adapter
    participant Webhook as Webhook Adapter
    participant Desktop as Desktop Adapter
    participant AuditLog as AuditLog

    Agent->>Tool: invoke notify-user { subject, body, priority }
    Tool->>Kernel: KernelCommand::SendUserNotification { msg }
    Kernel->>Kernel: validate user.notify permission
    Kernel->>Inbox: write UserMessage (status=Pending)
    Kernel->>AuditLog: NotificationSent { msg_id, agent_id, priority }
    par Deliver to all active adapters
        Kernel->>CLI: CliAdapter::deliver(msg)
        Kernel->>SSE: SseAdapter::deliver(msg) [if web session open]
        Kernel->>Webhook: WebhookAdapter::deliver(msg) [if configured]
        Kernel->>Desktop: DesktopAdapter::deliver(msg) [if available]
    end
    Kernel->>Inbox: update delivery_status per channel
    Kernel-->>Tool: Ok(notification_id)
    Tool-->>Agent: ToolOutput { notification_id }
```

---

## Flow B: Agent Asks User a Question (Blocking)

```mermaid
sequenceDiagram
    participant Agent as Agent (task executor)
    participant Tool as ask-user tool
    participant Kernel as Kernel / NotificationRouter
    participant Inbox as UserInbox (SQLite)
    participant Channels as All Delivery Adapters
    participant AuditLog as AuditLog
    participant User as User

    Agent->>Tool: invoke ask-user { question, options, timeout=600s, blocking=true }
    Tool->>Kernel: KernelCommand::AskUser { msg, blocking=true }
    Kernel->>Kernel: validate user.interact permission
    Kernel->>Inbox: write UserMessage { kind=Question, status=Pending }
    Kernel->>AuditLog: NotificationSent { kind=Question }
    Kernel->>Channels: deliver to all active adapters
    Note over Kernel: Create oneshot::channel() pair
    Kernel->>Kernel: suspend task → TaskState::Waiting
    Note over Kernel: store oneshot::Sender in WaitingTaskMap

    alt User responds within timeout (via CLI, Web, or Webhook)
        User->>Kernel: KernelCommand::RespondToNotification { id, text, channel }
        Kernel->>Inbox: update UserMessage { status=Responded, response }
        Kernel->>AuditLog: UserResponseReceived { msg_id, channel }
        Kernel->>Kernel: lookup oneshot::Sender from WaitingTaskMap
        Kernel->>Kernel: send UserResponse via oneshot::Sender
        Kernel->>Kernel: task resumes → TaskState::Running
        Kernel-->>Agent: ToolOutput { response_text: "user's answer" }
    else Timeout expires (no response)
        Kernel->>Inbox: update UserMessage { status=AutoActioned }
        Kernel->>AuditLog: NotificationAutoActioned { msg_id, auto_action }
        Kernel->>Kernel: send auto_action response via oneshot::Sender
        Kernel->>Kernel: task resumes → TaskState::Running
        Kernel-->>Agent: ToolOutput { response_text: "<auto-denied>" }
    end
```

---

## Flow C: Task Completion Auto-Notification

```mermaid
sequenceDiagram
    participant TaskExec as TaskExecutor
    participant EventBus as EventBus
    participant Dispatcher as EventDispatcher
    participant Router as NotificationRouter
    participant Inbox as UserInbox
    participant Channels as Delivery Adapters
    participant User as User

    TaskExec->>EventBus: emit TaskCompleted { task_id, outcome, summary, cost }
    EventBus->>Dispatcher: fan out to subscribers
    Dispatcher->>Router: NotificationHook::on_task_complete(event)
    Router->>Router: build UserMessage { kind=TaskComplete, ... }
    Router->>Inbox: write to UserInbox
    Router->>Channels: deliver to all active adapters
    Note over User: CLI badge, web notification bell, desktop popup
    User->>User: reads notification (no response needed)
```

---

## Flow D: User Responds via Web UI

```mermaid
sequenceDiagram
    participant Browser as Browser (HTMX)
    participant Web as agentos-web (Axum)
    participant Bus as IPC Bus (Unix socket)
    participant Kernel as Kernel
    participant Router as ResponseRouter
    participant Task as Suspended Task

    Browser->>Web: POST /notifications/{id}/respond { text: "approved" }
    Web->>Bus: BusMessage::Command(KernelCommand::RespondToNotification { id, text, channel: Web })
    Bus->>Kernel: route to command handler
    Kernel->>Router: route_response(id, UserResponse { text, channel: Web })
    Router->>Router: find matching msg_id in WaitingTaskMap
    alt blocking task waiting
        Router->>Task: oneshot::Sender::send(UserResponse)
        Task->>Task: TaskState: Waiting → Running
        Task-->>Kernel: task resumes
    else non-blocking notification
        Router->>Router: create AgentMessage to originating agent
        Router->>Router: emit UserResponseReceived event
    end
    Kernel-->>Web: KernelResponse::Success
    Web-->>Browser: HTMX partial: notification marked as responded
```

---

## Flow E: User Responds via CLI

```mermaid
sequenceDiagram
    participant CLI as agentctl notifications respond
    participant Bus as IPC Bus
    participant Kernel as Kernel
    participant Router as ResponseRouter

    CLI->>Bus: BusMessage::Command(KernelCommand::RespondToNotification { id, text, channel: Cli })
    Bus->>Kernel: dispatch
    Kernel->>Router: route_response(...)
    Note over Router: same as Flow D from here
    Kernel-->>Bus: KernelResponse::Success
    Bus-->>CLI: print "Response sent to agent."
```

---

## Internal Kernel Component Relationships

```
┌─────────────────────────────────────────────────────────────┐
│                         KERNEL                               │
│                                                             │
│  TaskExecutor ──ask-user──► NotificationRouter              │
│       │                           │                         │
│       │ TaskState::Waiting         ├── CliDeliveryAdapter   │
│       │ (oneshot::Receiver)        ├── SseDeliveryAdapter   │
│       │                           ├── WebhookAdapter        │
│       │                           └── DesktopAdapter        │
│       │                                    │                │
│       │                               UserInbox (SQLite)    │
│       │                                    │                │
│  ResponseRouter ◄── KernelCommand::Respond ┘                │
│       │                                                     │
│       └──► oneshot::Sender ──► TaskExecutor (resume)        │
│                                                             │
│  EventDispatcher ──► NotificationRouter (task complete hook)│
│                                                             │
│  AuditLog (receives all notification events)                │
└─────────────────────────────────────────────────────────────┘
        ▲                        ▲
        │                        │
   CLI / Bus               Web (Axum SSE)
```

---

## Data Model: UserMessage

```rust
// agentos-types/src/notification.rs (new file)

pub struct UserMessage {
    pub id: NotificationID,
    pub from: NotificationSource,       // Agent(AgentID) | Kernel | System
    pub task_id: Option<TaskID>,        // originating task (for drill-down)
    pub trace_id: TraceID,
    pub kind: UserMessageKind,
    pub priority: NotificationPriority,
    pub subject: String,                // ≤80 chars — fits CLI one-liner, email subject
    pub body: String,                   // Full markdown body
    pub interaction: Option<InteractionRequest>,
    pub delivery_status: HashMap<DeliveryChannel, DeliveryStatus>,
    pub response: Option<UserResponse>,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub read: bool,
}

pub enum UserMessageKind {
    Notification,
    Question {
        question: String,
        options: Option<Vec<String>>,  // None = free text; Some = multiple choice
        free_text_allowed: bool,
    },
    TaskComplete {
        task_id: TaskID,
        outcome: TaskOutcome,       // Success | Failed | Cancelled | TimedOut
        summary: String,
        duration_ms: u64,
        iterations: u32,
        cost: Option<InferenceCost>,
        tool_calls: u32,
    },
    StatusUpdate {
        task_id: TaskID,
        old_state: TaskState,
        new_state: TaskState,
        detail: Option<String>,
    },
}

pub enum NotificationPriority {
    Info,       // Informational, no action needed
    Warning,    // Something to be aware of
    Urgent,     // Needs attention soon
    Critical,   // Needs immediate attention (blocks agent)
}

pub struct InteractionRequest {
    pub timeout: Duration,          // How long to wait for response
    pub auto_action: String,        // Text to use if timeout expires (e.g. "<auto-denied>")
    pub blocking: bool,             // true = park task in TaskState::Waiting
    pub max_active: u8,             // Max concurrent blocking questions from this agent (default 3)
}

pub struct UserResponse {
    pub text: String,
    pub responded_at: DateTime<Utc>,
    pub channel: DeliveryChannel,
}

pub enum DeliveryChannel {
    Cli,
    Web,
    Webhook,
    Desktop,
    Slack,
}

pub enum DeliveryStatus {
    Pending,
    Delivered { at: DateTime<Utc> },
    Failed { reason: String },
    Skipped,   // adapter not active/available
}

pub enum TaskOutcome {
    Success,
    Failed,
    Cancelled,
    TimedOut,
}
```

---

## Permission Model

```
user.notify  (write)   — send fire-and-forget UserMessage
user.interact (execute) — send blocking Question (ask_user)
user.status  (write)   — send StatusUpdate (auto-granted to all agents)
```

Default agent `PermissionSet` includes `user.status`. `user.notify` requires explicit grant. `user.interact` requires explicit grant and is rate-limited.

---

## Related

- [[User-Agent Communication Plan]] — master plan
- [[User-Agent Communication Research]] — research synthesis
- [[01-user-message-type-and-router]] — Phase 1 implementation
- [[03-ask-user-tool]] — Phase 3 — blocking ask pattern
