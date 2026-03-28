---
title: Synchronous Agent RPC
tags:
  - multi-agent
  - kernel
  - rpc
  - plan
  - v3
date: 2026-03-25
status: complete
effort: 6d
priority: medium
---

# Phase 7 — Synchronous Agent RPC

> Add kernel-mediated synchronous agent-to-agent task delegation so agents can request work from specialist agents and await results, enabling supervisor/worker hierarchies like AutoGen and CrewAI without sacrificing AgentOS's security and audit guarantees.

---

## Why This Phase

The ecosystem research shows that **multi-agent collaboration patterns** are the dominant paradigm in production AI systems:

> "CrewAI models agents as 'crews' with defined roles... AutoGen (AG2) treats tasks as asynchronous dialogues where agents negotiate and discuss to reach a goal."

AgentOS currently has pub/sub agent messaging but no synchronous RPC. An agent cannot ask another agent to do something and wait for the result. This limits AgentOS to "loosely coupled" patterns and prevents:

- Supervisor agents that decompose complex tasks and delegate subtasks
- Specialist agents (e.g., a "code reviewer agent" always invoked for code output)
- Handoff patterns where Agent A finishes its part and hands to Agent B

The design constraint: **all RPC must go through the kernel bus**, not direct agent connections. This preserves the security boundary, capability token enforcement, and audit trail.

---

## Current → Target State

| Area | Current | Target |
|------|---------|--------|
| Agent messaging | Pub/sub only (fire and forget) | + synchronous RPC (caller blocks until result) |
| Agent delegation | `task-delegate` tool (spawns background subtask) | + `agent-call` tool (blocks for result, returns output) |
| Supervisor pattern | None | Supervisor agent creates worker subtasks, awaits each |
| Agent handoff | None | Agent A finishes, signals completion to waiting Agent B |
| RPC timeout | N/A | Configurable per-call timeout (default 5 min), auto-escalation on timeout |
| RPC audit | None | Every RPC call/response logged to audit trail |
| Circular dependency | N/A | Kernel detects A→B→A call chains and returns error |

---

## Architecture

```
Supervisor Agent (Task T1)
     │
     │  tool: agent-call "specialist-agent" "Analyze this code: ..."
     │  (blocks here)
     │
     ▼
┌────────────────────────────────────────────────┐
│  Kernel RPC Router                             │
│                                                │
│  1. Validate caller has `agent.call:x` perm    │
│  2. Look up target agent in registry           │
│  3. Check call depth (prevent infinite loops)  │
│  4. Create RPC task (child of T1)              │
│  5. Queue to target agent's task queue         │
│  6. Register T1 as waiter for RPC task         │
│  7. Suspend T1 (free CPU, not dropped)         │
└─────────────────┬──────────────────────────────┘
                  │  (target agent executes RPC task)
                  ▼
Specialist Agent (runs RPC task)
     │
     │  Executes normally, returns result
     │
     ▼
┌────────────────────────────────────────────────┐
│  Kernel RPC Router                             │
│                                                │
│  On RPC task completion:                       │
│  1. Wake up suspended T1                       │
│  2. Inject RPC result into T1 context          │
│  3. T1 resumes from next iteration             │
└────────────────────────────────────────────────┘
```

---

## Detailed Subtasks

### Subtask 7.1 — RpcManager: kernel subsystem for pending calls

**File:** `crates/agentos-kernel/src/rpc_manager.rs` (new)

```rust
use std::collections::HashMap;
use tokio::sync::oneshot;
use crate::types::{TaskID, AgentID};

pub struct RpcCall {
    pub caller_task_id: TaskID,
    pub target_agent_id: AgentID,
    pub rpc_task_id: TaskID,
    pub timeout_at: DateTime<Utc>,
    pub result_tx: oneshot::Sender<RpcResult>,
}

pub struct RpcResult {
    pub output: String,
    pub success: bool,
    pub error: Option<String>,
}

pub struct RpcManager {
    /// pending_calls: rpc_task_id → RpcCall
    pending: Arc<RwLock<HashMap<TaskID, RpcCall>>>,
    /// depth_tracker: caller_task_id → call stack depth
    depths: Arc<RwLock<HashMap<TaskID, u32>>>,
}

const MAX_CALL_DEPTH: u32 = 5;

impl RpcManager {
    pub async fn register_call(
        &self,
        caller_task_id: TaskID,
        target_agent_id: AgentID,
        rpc_task_id: TaskID,
        timeout_secs: u64,
    ) -> Result<oneshot::Receiver<RpcResult>> {
        // Check call depth to prevent infinite delegation chains
        let depth = self.depths.read().await.get(&caller_task_id).copied().unwrap_or(0);
        if depth >= MAX_CALL_DEPTH {
            return Err(AgentOSError::RpcDepthExceeded { max: MAX_CALL_DEPTH });
        }
        let (tx, rx) = oneshot::channel();
        self.pending.write().await.insert(rpc_task_id.clone(), RpcCall {
            caller_task_id,
            target_agent_id,
            rpc_task_id,
            timeout_at: Utc::now() + Duration::seconds(timeout_secs as i64),
            result_tx: tx,
        });
        Ok(rx)
    }

    pub async fn complete_call(
        &self,
        rpc_task_id: &TaskID,
        result: RpcResult,
    ) -> Result<()> {
        if let Some(call) = self.pending.write().await.remove(rpc_task_id) {
            let _ = call.result_tx.send(result);
        }
        Ok(())
    }

    /// Called by TimeoutChecker to sweep expired calls
    pub async fn sweep_expired(&self) -> Vec<TaskID> {
        let mut pending = self.pending.write().await;
        let now = Utc::now();
        let expired: Vec<TaskID> = pending
            .iter()
            .filter(|(_, call)| call.timeout_at < now)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &expired {
            if let Some(call) = pending.remove(id) {
                let _ = call.result_tx.send(RpcResult {
                    output: String::new(),
                    success: false,
                    error: Some("RPC call timed out".to_string()),
                });
            }
        }
        expired
    }
}
```

---

### Subtask 7.2 — `agent-call` tool

**File:** `crates/agentos-tools/src/agent_call.rs` (new)

```rust
pub struct AgentCallTool;

#[async_trait]
impl AgentTool for AgentCallTool {
    fn name(&self) -> &str { "agent-call" }
    fn description(&self) -> &str {
        "Synchronously call another agent with a prompt and wait for its response. \
         Returns the agent's output as a string. Times out after timeout_secs."
    }

    async fn execute(&self, input: ToolInput, ctx: &ToolContext) -> Result<ToolOutput> {
        let target_agent: String = input.get_required("target_agent")?;
        let prompt: String = input.get_required("prompt")?;
        let timeout_secs: u64 = input.get_optional("timeout_secs")?.unwrap_or(300);

        // Validate permission: agent.call:x
        ctx.permissions.check("agent.call", Operation::Execute)?;

        // Find target agent in registry
        let target_id = ctx.agent_registry.find_by_name(&target_agent).await?
            .ok_or(AgentOSError::AgentNotFound(target_agent.clone()))?;

        // Send RpcRequest command to kernel
        let rpc_task_id = ctx.kernel_client.send_command(KernelCommand::AgentRpcCall {
            caller_task_id: ctx.task_id.clone(),
            target_agent_id: target_id,
            prompt: prompt.clone(),
            timeout_secs,
        }).await?;

        // Block (async await) for result — kernel suspends our task
        // This oneshot channel is signaled when the RPC task completes
        let result = ctx.rpc_receiver.await
            .map_err(|_| AgentOSError::RpcAborted)?;

        if result.success {
            Ok(ToolOutput::text(result.output))
        } else {
            Err(AgentOSError::RpcFailed(result.error.unwrap_or_default()))
        }
    }
}
```

**File:** `tools/core/agent-call.toml` (new)

```toml
name = "agent-call"
description = "Synchronously call another agent and await its response"
version = "1.0.0"
trust_tier = "core"

[permissions]
required = ["agent.call:x"]

[input_schema]
type = "object"
properties.target_agent = { type = "string", description = "Name of the agent to call" }
properties.prompt = { type = "string", description = "Task prompt for the target agent" }
properties.timeout_secs = { type = "integer", default = 300, description = "Timeout in seconds" }
required = ["target_agent", "prompt"]
```

---

### Subtask 7.3 — Kernel command: AgentRpcCall

**File:** `crates/agentos-bus/src/message.rs`

```rust
// Add to KernelCommand:
AgentRpcCall {
    caller_task_id: TaskID,
    target_agent_id: AgentID,
    prompt: String,
    timeout_secs: u64,
},
AgentRpcComplete {
    rpc_task_id: TaskID,
    output: String,
    success: bool,
    error: Option<String>,
},
```

**File:** `crates/agentos-kernel/src/commands/agent.rs`

```rust
KernelCommand::AgentRpcCall { caller_task_id, target_agent_id, prompt, timeout_secs } => {
    // 1. Create a new subtask for the target agent
    let rpc_task_id = TaskID::new();
    let rpc_task = AgentTask {
        id: rpc_task_id.clone(),
        agent_id: target_agent_id.clone(),
        parent_task_id: Some(caller_task_id.clone()),
        prompt: prompt.clone(),
        // Inherit budget from parent (reduce remaining budget)
        ..
    };

    // 2. Register call in RpcManager (get result receiver)
    let result_rx = ctx.rpc_manager.register_call(
        caller_task_id.clone(),
        target_agent_id.clone(),
        rpc_task_id.clone(),
        timeout_secs,
    ).await?;

    // 3. Suspend caller task (set status to Suspended, save result_rx in task state)
    ctx.task_registry.suspend_for_rpc(&caller_task_id, result_rx).await?;

    // 4. Queue RPC task to target agent's queue
    ctx.scheduler.enqueue(rpc_task).await?;

    // 5. Audit log
    ctx.audit.log(AuditEvent::AgentRpcCallStarted { caller_task_id, target_agent_id, rpc_task_id }).await?;

    respond(KernelResponse::RpcTaskCreated { rpc_task_id })
}
```

---

### Subtask 7.4 — Task completion: wake up suspended callers

**File:** `crates/agentos-kernel/src/task_completion.rs`

On any task completion, check if it was an RPC task:

```rust
if let Some(parent_task_id) = task.parent_task_id.as_ref() {
    if let Some(caller_is_suspended) = ctx.task_registry.is_suspended_for_rpc(parent_task_id).await {
        // Complete the RPC call
        ctx.rpc_manager.complete_call(
            &task.id,
            RpcResult {
                output: task.result.clone().unwrap_or_default(),
                success: task.status == TaskStatus::Completed,
                error: task.error.clone(),
            }
        ).await?;

        // Resume the caller (it was blocking on the oneshot receiver)
        // The task_executor is already awaiting the oneshot; it will auto-resume
        ctx.task_registry.mark_resuming(parent_task_id).await?;
    }
}
```

---

### Subtask 7.5 — Permission: `agent.call:x`

**File:** `crates/agentos-kernel/src/core_manifests.rs`

Add `agent.call:x` to the default permission set comments. Agents must be explicitly granted this permission to use agent-call:

```bash
agentctl perm grant --agent supervisor-agent --resource agent.call --ops x
```

This prevents agents from calling each other without explicit authorization.

---

### Subtask 7.6 — TimeoutChecker integration

**File:** `crates/agentos-kernel/src/run_loop.rs`

Add `rpc_manager.sweep_expired()` to the existing TimeoutChecker loop (runs every 10min). Expired RPC calls trigger an escalation to the supervisor user (using existing escalation machinery).

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/rpc_manager.rs` | New — RpcManager with pending calls and depth tracking |
| `crates/agentos-tools/src/agent_call.rs` | New — agent-call tool implementation |
| `tools/core/agent-call.toml` | New — tool manifest |
| `crates/agentos-bus/src/message.rs` | Modified — add AgentRpcCall/Complete commands |
| `crates/agentos-kernel/src/commands/agent.rs` | Modified — handle AgentRpcCall |
| `crates/agentos-kernel/src/task_completion.rs` | Modified — wake suspended callers |
| `crates/agentos-kernel/src/context.rs` | Modified — add rpc_manager field |
| `crates/agentos-kernel/src/run_loop.rs` | Modified — add RPC sweep to TimeoutChecker |
| `crates/agentos-kernel/src/core_manifests.rs` | Modified — register agent-call tool |
| `crates/agentos-types/src/lib.rs` | Modified — add AgentOSError::RpcDepthExceeded, RpcAborted, RpcFailed |

---

## Dependencies

- No other phases required
- Requires escalation manager (already complete)
- Requires TimeoutChecker (already complete)

---

## Test Plan

1. **Basic RPC call** — supervisor runs task, uses `agent-call "worker" "sum 1+1"`, worker responds "2", supervisor receives "2" and continues
2. **RPC timeout** — worker task hangs, caller times out after configured timeout, caller receives RpcFailed error, escalation created
3. **Circular call detection** — A calls B, B calls A, assert `RpcDepthExceeded` error on the second call
4. **Depth limit** — A→B→C→D→E (depth 5, at max), assert 6th call returns `RpcDepthExceeded`
5. **Permission enforcement** — agent without `agent.call:x` attempts `agent-call`, assert permission denied
6. **Budget inheritance** — caller has $0.10 budget remaining, RPC task inherits it, task that would cost $0.20 is denied
7. **Audit trail** — run a successful RPC call, check audit log for `AgentRpcCallStarted` and `AgentRpcCallCompleted` events

---

## Verification

```bash
cargo build -p agentos-kernel -p agentos-tools
cargo test -p agentos-kernel -- rpc

# Manual multi-agent test
agentctl agent connect "supervisor" --model claude-sonnet-4-6
agentctl agent connect "worker" --model claude-haiku-4-5
agentctl perm grant --agent supervisor --resource agent.call --ops x

agentctl task run --agent supervisor \
  "Use the agent-call tool to ask the 'worker' agent: 'What is 15 * 7?'. Report back the answer."

agentctl task list  # check both tasks complete
```

---

## Related

- [[Real World Adoption Roadmap Plan]]
- [[02-web-ui-completion]] — supervisor/worker task trees visible in task trace UI
