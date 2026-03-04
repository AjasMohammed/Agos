# Plan 04 — Agent-to-Agent Communication (`agentos-agent-bus`)

## Goal

Implement the **Agent Message Bus** — a kernel-managed communication layer that allows agents to send direct messages, delegate subtasks, and broadcast to groups. This is the foundation for multi-agent collaboration in AgentOS.

Additionally, implement two new tools: `agent-message` and `task-delegate`, which give agents the ability to use the message bus from within task execution.

## Dependencies

- `agentos-types`
- `agentos-bus` (existing IPC layer)
- `agentos-capability`
- `agentos-audit`
- `tokio` (channels, sync primitives)
- `serde`, `serde_json`
- `tracing`

## Architecture

```
Agent A (task running)
    │
    ├── Uses agent-message tool:  { "to": "summarizer", "content": "Please summarize this..." }
    │
    ▼
Kernel receives tool call
    │
    ├── 1. Validate: does Agent A have agent.message:x permission?
    ├── 2. Validate: does target agent exist and have agent.message:r permission?
    ├── 3. Log message to audit log
    ├── 4. Route message through AgentMessageBus
    └── 5. Return delivery confirmation to Agent A
         │
         ▼
    Target agent receives message in its context (if running a task)
    OR message is queued until the agent starts a new task
```

## New Types

```rust
// In agentos-types/src/agent_message.rs

use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    pub id: MessageID,
    pub from: AgentID,
    pub to: MessageTarget,
    pub content: MessageContent,
    pub reply_to: Option<MessageID>,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub trace_id: TraceID,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageTarget {
    Direct(AgentID),
    DirectByName(String),      // resolve name → AgentID at send time
    Group(GroupID),
    Broadcast,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageContent {
    Text(String),
    Structured(serde_json::Value),
    TaskDelegation {
        prompt: String,
        priority: u8,
        timeout_secs: u64,
    },
    TaskResult {
        task_id: TaskID,
        result: serde_json::Value,
    },
}
```

## Core Struct: `AgentMessageBus`

```rust
// In agentos-kernel/src/agent_message_bus.rs (or new crate)

use tokio::sync::mpsc;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct AgentMessageBus {
    /// Per-agent message channels. Each agent has an inbox.
    inboxes: RwLock<HashMap<AgentID, mpsc::UnboundedSender<AgentMessage>>>,
    /// Agent group memberships.
    groups: RwLock<HashMap<GroupID, Vec<AgentID>>>,
    /// Message history for audit and retrieval.
    history: RwLock<Vec<AgentMessage>>,
}

impl AgentMessageBus {
    pub fn new() -> Self;

    /// Register an agent's inbox when they connect.
    pub async fn register_agent(&self, agent_id: AgentID) -> mpsc::UnboundedReceiver<AgentMessage>;

    /// Unregister an agent when they disconnect. Queued messages are lost.
    pub async fn unregister_agent(&self, agent_id: &AgentID);

    /// Send a direct message to a specific agent.
    pub async fn send_direct(
        &self,
        message: AgentMessage,
    ) -> Result<(), AgentOSError>;

    /// Broadcast a message to all connected agents (except sender).
    pub async fn broadcast(
        &self,
        message: AgentMessage,
    ) -> Result<u32, AgentOSError>;  // returns number of recipients

    /// Send to a group.
    pub async fn send_to_group(
        &self,
        group_id: &GroupID,
        message: AgentMessage,
    ) -> Result<u32, AgentOSError>;

    /// Create a named group of agents.
    pub async fn create_group(
        &self,
        group_id: GroupID,
        members: Vec<AgentID>,
    );

    /// Get recent message history for an agent.
    pub async fn get_history(
        &self,
        agent_id: &AgentID,
        limit: usize,
    ) -> Vec<AgentMessage>;

    /// Get pending (undelivered) message count for an agent.
    pub async fn pending_count(&self, agent_id: &AgentID) -> usize;
}
```

## Task Delegation

When Agent A delegates a subtask to Agent B:

```rust
// In the kernel's task execution logic:

impl Kernel {
    async fn handle_task_delegation(
        &self,
        parent_task: &AgentTask,
        target_agent_name: &str,
        prompt: &str,
        priority: u8,
        timeout_secs: u64,
    ) -> Result<serde_json::Value, AgentOSError> {
        // 1. Resolve target agent
        let agent_registry = self.agent_registry.read().await;
        let target = agent_registry.get_by_name(target_agent_name)
            .ok_or(AgentOSError::AgentNotFound(target_agent_name.to_string()))?;

        // 2. Create child task with DOWNSCOPED permissions
        //    Child can NEVER have more permissions than parent
        let child_permissions = parent_task.capability_token.permissions.clone();
        // Intersect with target agent's own permissions
        let effective_permissions = child_permissions.intersect(
            &self.capability_engine.get_permissions(&target.id)?
        );

        // 3. Issue a restricted capability token for the child task
        let child_token = self.capability_engine.issue_token(
            TaskID::new(),
            target.id,
            parent_task.capability_token.allowed_tools.clone(),
            parent_task.capability_token.allowed_intents.clone(),
            Duration::from_secs(timeout_secs),
        )?;

        // 4. Create the child task
        let child_task = AgentTask {
            id: child_token.task_id,
            state: TaskState::Queued,
            agent_id: target.id,
            capability_token: child_token,
            assigned_llm: None,
            priority,
            created_at: chrono::Utc::now(),
            timeout: Duration::from_secs(timeout_secs),
            original_prompt: prompt.to_string(),
            history: Vec::new(),
            parent_task: Some(parent_task.id),  // link to parent
        };

        // 5. Enqueue child task
        self.scheduler.enqueue(child_task.clone()).await;

        // 6. Wait for child to complete (with timeout)
        //    ... poll scheduler for child task state ...

        // 7. Return child's result to parent
        Ok(serde_json::json!({
            "delegated_to": target_agent_name,
            "child_task_id": child_task.id.to_string(),
            "status": "queued",
        }))
    }
}
```

## Tool: `agent-message`

```rust
// In agentos-tools/src/agent_message.rs

pub struct AgentMessageTool;

#[async_trait]
impl AgentTool for AgentMessageTool {
    fn name(&self) -> &str { "agent-message" }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("agent.message".to_string(), PermissionOp::Execute)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let to = payload.get("to").and_then(|v| v.as_str())
            .ok_or_else(|| AgentOSError::SchemaValidation(
                "agent-message requires 'to' field (agent name)".into()
            ))?;
        let content = payload.get("content").and_then(|v| v.as_str())
            .ok_or_else(|| AgentOSError::SchemaValidation(
                "agent-message requires 'content' field".into()
            ))?;

        // The actual message delivery is handled by the kernel, not the tool
        // The tool returns a structured request that the kernel intercepts
        Ok(serde_json::json!({
            "_kernel_action": "send_agent_message",
            "to": to,
            "content": content,
        }))
    }
}
```

## Tool: `task-delegate`

```rust
// In agentos-tools/src/task_delegate.rs

pub struct TaskDelegate;

#[async_trait]
impl AgentTool for TaskDelegate {
    fn name(&self) -> &str { "task-delegate" }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("agent.message".to_string(), PermissionOp::Execute)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let target_agent = payload.get("agent").and_then(|v| v.as_str())
            .ok_or_else(|| AgentOSError::SchemaValidation(
                "task-delegate requires 'agent' field".into()
            ))?;
        let task = payload.get("task").and_then(|v| v.as_str())
            .ok_or_else(|| AgentOSError::SchemaValidation(
                "task-delegate requires 'task' field (the prompt for the sub-agent)".into()
            ))?;
        let priority = payload.get("priority").and_then(|v| v.as_u64()).unwrap_or(5) as u8;

        Ok(serde_json::json!({
            "_kernel_action": "delegate_task",
            "target_agent": target_agent,
            "task": task,
            "priority": priority,
        }))
    }
}
```

## CLI Changes

```bash
# Send a message to an agent (from CLI)
agentctl agent message analyst "Please review the latest error logs"

# List messages for an agent
agentctl agent messages analyst --last 10

# Create an agent group
agentctl agent group create ops-team --members analyst,monitor

# Broadcast to a group
agentctl agent broadcast ops-team "System maintenance starting in 1 hour"
```

New `KernelCommand` variants:

```rust
// Add to agentos-bus/src/message.rs
pub enum KernelCommand {
    // ... existing variants ...
    SendAgentMessage { from_name: String, to_name: String, content: String },
    ListAgentMessages { agent_name: String, limit: u32 },
    CreateAgentGroup { group_name: String, members: Vec<String> },
    BroadcastToGroup { group_name: String, content: String },
}
```

## Agent Directory Injection

When a task starts, the kernel now injects an **agent directory** into the context:

```
[AGENT_DIRECTORY]
You are operating inside AgentOS. The following agents are available:

- analyst (anthropic/claude-sonnet-4) — Status: Idle
  Permissions: agent.message:rx

- summarizer (ollama/llama3.2) — Status: Busy (task-042)
  Permissions: agent.message:rx

To message an agent: use the agent-message tool
To delegate a subtask: use the task-delegate tool
[/AGENT_DIRECTORY]
```

## Tests

```rust
#[tokio::test]
async fn test_direct_message_delivery() {
    let bus = AgentMessageBus::new();
    let agent_a = AgentID::new();
    let agent_b = AgentID::new();

    let mut inbox_b = bus.register_agent(agent_b).await;
    bus.register_agent(agent_a).await;

    let msg = AgentMessage {
        id: MessageID::new(),
        from: agent_a,
        to: MessageTarget::Direct(agent_b),
        content: MessageContent::Text("Hello from A".into()),
        reply_to: None,
        timestamp: chrono::Utc::now(),
        trace_id: TraceID::new(),
    };

    bus.send_direct(msg).await.unwrap();

    let received = inbox_b.recv().await.unwrap();
    assert_eq!(received.from, agent_a);
}

#[tokio::test]
async fn test_broadcast_reaches_all_except_sender() {
    let bus = AgentMessageBus::new();
    let a = AgentID::new();
    let b = AgentID::new();
    let c = AgentID::new();

    bus.register_agent(a).await;
    let mut inbox_b = bus.register_agent(b).await;
    let mut inbox_c = bus.register_agent(c).await;

    let msg = AgentMessage {
        id: MessageID::new(),
        from: a,
        to: MessageTarget::Broadcast,
        content: MessageContent::Text("Hello all".into()),
        reply_to: None,
        timestamp: chrono::Utc::now(),
        trace_id: TraceID::new(),
    };

    let count = bus.broadcast(msg).await.unwrap();
    assert_eq!(count, 2); // b and c, not a

    assert!(inbox_b.try_recv().is_ok());
    assert!(inbox_c.try_recv().is_ok());
}

#[tokio::test]
async fn test_delegation_creates_child_task_with_downscoped_token() {
    // Verify child task's permissions are intersection of parent + target agent
}

#[tokio::test]
async fn test_message_to_nonexistent_agent_fails() {
    let bus = AgentMessageBus::new();
    let msg = /* message to AgentID that doesn't exist */;
    assert!(bus.send_direct(msg).await.is_err());
}
```

## Verification

```bash
cargo test -p agentos-kernel   # message bus tests
cargo test -p agentos-tools    # agent-message and task-delegate tool tests
```
