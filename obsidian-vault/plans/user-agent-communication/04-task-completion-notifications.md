---
title: "Phase 4: Task Completion Auto-Notifications"
tags:
  - kernel
  - plan
  - phase-4
date: 2026-03-24
status: planned
effort: 1d
priority: medium
---

# Phase 4: Task Completion Auto-Notifications

> Automatically send a `UserMessage` (kind=`TaskComplete`) whenever a task the user initiated finishes — success or failure. User stops polling `agentctl task list`.

**Depends on**: [[01-user-message-type-and-router]] (Phase 1)
**Blocks**: Nothing

---

## Why This Phase

The most basic UX expectation when you ask an agent to do something is: "tell me when it's done." AgentOS currently requires the user to poll `agentctl task list`. Phase 4 closes this gap by hooking into the existing `TaskCompleted` / `TaskFailed` event system to automatically generate a `UserMessage`.

This is the highest return-on-investment phase after Phase 1 — it requires almost no new architecture, just wiring the existing event and notification systems together.

---

## Current State vs. Target

| Item | Current | Target |
|------|---------|--------|
| `TaskCompleted` event | Emitted to EventBus | Also triggers UserMessage to notification inbox |
| `TaskFailed` event | Emitted to EventBus | Also triggers UserMessage to notification inbox |
| `task_completion.rs` | Writes episodic memory + emits events | Also calls NotificationRouter |
| Task completion summary | Not generated | Generated: outcome, duration, cost, tool calls, summary |
| Notification opt-out | N/A | Config flag per-agent: `notify_on_complete = true` |

---

## Detailed Subtasks

### 4.1 — Hook into task completion in `task_completion.rs`

**File**: `crates/agentos-kernel/src/task_completion.rs`

Find the function that runs when a task completes (likely called from `task_executor.rs`). After writing episodic memory and emitting `TaskCompleted` event, add:

```rust
pub async fn on_task_complete(
    kernel: &Kernel,
    task: &AgentTask,
    result: &TaskResult,
    outcome: TaskOutcome,
) {
    // ... existing: write episodic memory, emit event ...

    // NEW: send completion notification to user
    if should_notify_on_complete(kernel, task) {
        let summary = build_task_summary(task, result, &outcome);
        let msg = UserMessage {
            id: NotificationID::new(),
            from: NotificationSource::Agent(task.agent_id),
            task_id: Some(task.id),
            trace_id: task.history.last().map(|m| m.trace_id).unwrap_or_else(TraceID::new),
            kind: UserMessageKind::TaskComplete {
                task_id: task.id,
                outcome: outcome.clone(),
                summary: summary.clone(),
                duration_ms: task.started_at.map(|s| {
                    Utc::now().signed_duration_since(s).num_milliseconds() as u64
                }).unwrap_or(0),
                iterations: result.iterations,
                cost_usd: result.cost.as_ref().map(|c| c.total_cost_usd),
                tool_calls: result.tool_calls_count,
            },
            priority: match outcome {
                TaskOutcome::Success => NotificationPriority::Info,
                TaskOutcome::Failed | TaskOutcome::TimedOut => NotificationPriority::Warning,
                TaskOutcome::Cancelled => NotificationPriority::Info,
            },
            subject: format_completion_subject(&outcome, &task.original_prompt),
            body: format_completion_body(task, result, &outcome, &summary),
            interaction: None,  // no response needed
            delivery_status: HashMap::new(),
            response: None,
            created_at: Utc::now(),
            expires_at: None,  // completion notifications don't expire
            read: false,
        };

        if let Err(e) = kernel.notification_router.deliver(msg).await {
            tracing::warn!("Failed to send task completion notification: {e}");
            // non-fatal — task is still complete
        }
    }
}
```

---

### 4.2 — Build the completion summary

**File**: `crates/agentos-kernel/src/task_completion.rs`

```rust
fn format_completion_subject(outcome: &TaskOutcome, prompt: &str) -> String {
    let icon = match outcome {
        TaskOutcome::Success => "✓",
        TaskOutcome::Failed => "✗",
        TaskOutcome::Cancelled => "○",
        TaskOutcome::TimedOut => "⏱",
    };
    let verb = match outcome {
        TaskOutcome::Success => "completed",
        TaskOutcome::Failed => "failed",
        TaskOutcome::Cancelled => "cancelled",
        TaskOutcome::TimedOut => "timed out",
    };
    let short_prompt = prompt.chars().take(50).collect::<String>();
    format!("{icon} Task {verb}: {short_prompt}...")
}

fn format_completion_body(
    task: &AgentTask,
    result: &TaskResult,
    outcome: &TaskOutcome,
    summary: &str,
) -> String {
    // Markdown body for the notification
    let duration_s = ...; // calculate from task.started_at
    let cost_str = result.cost.as_ref()
        .map(|c| format!("${:.4}", c.total_cost_usd))
        .unwrap_or_else(|| "N/A".to_string());

    format!(
        "## Task {outcome}\n\n\
        **Original request:** {prompt}\n\n\
        **Summary:** {summary}\n\n\
        | Metric | Value |\n\
        |--------|-------|\n\
        | Duration | {duration_s:.1}s |\n\
        | Iterations | {iterations} |\n\
        | Tool calls | {tool_calls} |\n\
        | Cost | {cost} |\n",
        outcome = format!("{:?}", outcome),
        prompt = task.original_prompt,
        summary = summary,
        duration_s = duration_s,
        iterations = result.iterations,
        tool_calls = result.tool_calls_count,
        cost = cost_str,
    )
}

fn build_task_summary(task: &AgentTask, result: &TaskResult, outcome: &TaskOutcome) -> String {
    // Use the last assistant message from the task as the summary
    // Fall back to outcome description
    result.final_response
        .as_deref()
        .map(|s| s.chars().take(500).collect())
        .unwrap_or_else(|| format!("Task {outcome:?} after {} iterations", result.iterations))
}
```

---

### 4.3 — Add config opt-out

**File**: `config/default.toml`

```toml
[notifications]
inbox_path = "data/notifications.db"
max_inbox_size = 1000

# Automatically notify user when a task completes or fails
notify_on_task_complete = true
notify_on_task_failed = true

# Notification rate limit per agent
max_notifications_per_minute = 10
max_concurrent_blocking_questions = 3
```

**File**: `crates/agentos-kernel/src/config.rs` (or wherever kernel config is deserialized)

```rust
pub struct NotificationsConfig {
    pub inbox_path: PathBuf,
    pub max_inbox_size: usize,
    pub notify_on_task_complete: bool,
    pub notify_on_task_failed: bool,
    pub max_notifications_per_minute: u32,
    pub max_concurrent_blocking_questions: u8,
}
```

The `should_notify_on_complete()` function checks:
```rust
fn should_notify_on_complete(kernel: &Kernel, task: &AgentTask) -> bool {
    // 1. Is this a user-initiated task (not delegated sub-task)?
    // Sub-tasks (parent_task.is_some()) don't notify — only the root task does
    if task.parent_task.is_some() {
        return false;
    }
    // 2. Check config flag
    kernel.config.notifications.notify_on_task_complete
}
```

---

### 4.4 — Handle task failure notification

**File**: `crates/agentos-kernel/src/task_executor.rs`

On task failure / timeout, call the same `on_task_complete` with the appropriate `TaskOutcome`:

```rust
// On task success
on_task_complete(&kernel, &task, &result, TaskOutcome::Success).await;

// On task failure
on_task_complete(&kernel, &task, &result, TaskOutcome::Failed).await;

// On task timeout
on_task_complete(&kernel, &task, &result, TaskOutcome::TimedOut).await;
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/task_completion.rs` | Add `on_task_complete()` notification call |
| `crates/agentos-kernel/src/task_executor.rs` | Call on_task_complete on all terminal states |
| `config/default.toml` | Add `[notifications]` section |
| `crates/agentos-kernel/src/config.rs` | Add `NotificationsConfig` struct |

---

## Test Plan

```rust
#[tokio::test]
async fn test_task_completion_generates_notification() {
    let kernel = setup_kernel().await;
    // run a simple task to completion
    let task_id = kernel.submit_task("echo hello").await;
    wait_for_task_complete(task_id).await;
    // check inbox
    let notifs = kernel.list_notifications(false, 10).await;
    let completion = notifs.iter().find(|n| matches!(n.kind, UserMessageKind::TaskComplete { .. }));
    assert!(completion.is_some());
    let kind = &completion.unwrap().kind;
    if let UserMessageKind::TaskComplete { outcome, .. } = kind {
        assert_eq!(*outcome, TaskOutcome::Success);
    }
}

#[tokio::test]
async fn test_task_failure_generates_warning_notification() {
    // run a task that will fail
    // check notification with priority=Warning and outcome=Failed
}

#[tokio::test]
async fn test_subtask_does_not_generate_notification() {
    // run a task that delegates a sub-task
    // only one completion notification should appear (for the root task)
}

#[tokio::test]
async fn test_notify_on_complete_false_skips_notification() {
    // set notify_on_task_complete = false in config
    // run a task to completion
    // inbox should remain empty
}

#[tokio::test]
async fn test_completion_subject_format() {
    assert_eq!(
        format_completion_subject(&TaskOutcome::Success, "write a poem about Rust"),
        "✓ Task completed: write a poem about Rust..."
    );
}
```

---

## Verification

```bash
cargo build -p agentos-kernel
cargo test -p agentos-kernel -- task_completion

# Integration test
agentctl kernel start &
agentctl task run --agent my-agent "summarize this: hello world"
# wait for task to complete...

# Check notification appeared automatically
agentctl notifications list
# expect: row with "✓ Task completed: summarize this: hello wo..."

agentctl notifications read <id>
# expect: full body with duration, cost, summary
```
