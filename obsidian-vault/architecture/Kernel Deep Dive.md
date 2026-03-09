---
title: Kernel Deep Dive
tags: [architecture, kernel]
---

# Kernel Deep Dive

The Inference Kernel (`agentos-kernel`) is the central orchestrator of AgentOS. It manages all subsystems, routes tasks, and enforces security.

**Source:** `crates/agentos-kernel/src/kernel.rs`

## Kernel Struct

The kernel holds `Arc` references to all subsystems:

```rust
pub struct Kernel {
    config: KernelConfig,
    audit: Arc<AuditLog>,
    vault: Arc<SecretsVault>,
    capability_engine: Arc<CapabilityEngine>,
    scheduler: Arc<TaskScheduler>,
    context_manager: Arc<ContextManager>,
    tool_registry: Arc<RwLock<ToolRegistry>>,
    agent_registry: Arc<RwLock<AgentRegistry>>,
    bus: Arc<BusServer>,
    tool_runner: Arc<ToolRunner>,
    sandbox: Arc<SandboxExecutor>,
    router: Arc<TaskRouter>,
    active_llms: Arc<RwLock<HashMap<AgentID, Arc<dyn LLMCore>>>>,
    message_bus: Arc<AgentMessageBus>,
    profile_manager: Arc<ProfileManager>,
    episodic_memory: Arc<EpisodicStore>,
    schedule_manager: Arc<ScheduleManager>,
    background_pool: Arc<BackgroundPool>,
    hal: Arc<HardwareAbstractionLayer>,
    pipeline_engine: Arc<PipelineEngine>,
}
```

## Subsystem Details

### Task Scheduler

**Source:** `crates/agentos-kernel/src/scheduler.rs`

- Priority-based `BinaryHeap` queue
- Higher priority tasks dequeued first
- FIFO within same priority (by creation time)
- Task states: `Queued → Running → Waiting → Complete/Failed/Cancelled`

### Task Router

**Source:** `crates/agentos-kernel/src/router.rs`

Routes tasks to agents using:
1. **Routing Rules** - Regex pattern matching on prompt text
   - `preferred_agent` + optional `fallback_agent`
2. **Routing Strategies** (fallback when no rules match):
   - `CapabilityFirst` (default) - Largest context window
   - `CostFirst` - Cheapest model
   - `LatencyFirst` - Fastest model
   - `RoundRobin` - Even distribution

### Context Manager

**Source:** `crates/agentos-kernel/src/context.rs`

- Per-task rolling context windows
- Max entries configurable (default 100)
- Entry roles: `System`, `User`, `Assistant`, `ToolResult`
- Evicts oldest non-System entry when full
- Metadata: tool_name, tool_id, intent_id, token estimates

### Agent Registry

**Source:** `crates/agentos-kernel/src/agent_registry.rs`

- In-memory registry with name index
- Manages agent profiles + role assignments
- Default "base" role with minimal permissions (`fs.user_data:rw`)
- Persists to disk via JSON serialization

### Agent Message Bus

**Source:** `crates/agentos-kernel/src/agent_message_bus.rs`

- Per-agent `mpsc` channel inboxes
- Message targets: `Direct(AgentID)`, `DirectByName(String)`, `Group(GroupID)`, `Broadcast`
- Message history for audit/retrieval
- Agent groups: `group_id → Vec<AgentID>`

### Schedule Manager

**Source:** `crates/agentos-kernel/src/schedule_manager.rs`

- Cron-expression based job scheduling
- Jobs: agent + task template
- States: Active, Paused, Deleted

### Background Pool

**Source:** `crates/agentos-kernel/src/background_pool.rs`

- Tokio-spawned background tasks
- Track status, logs, and completion
- Support for detached execution

## Command Processing

When the kernel receives a `KernelCommand` via the bus, it dispatches to the appropriate handler:

```
KernelCommand::ConnectAgent → agent_registry.register() + llm.health_check()
KernelCommand::RunTask → scheduler.enqueue() + route + execute loop
KernelCommand::ListTools → tool_registry.list()
KernelCommand::SetSecret → vault.store_secret()
KernelCommand::GrantPermission → agent_registry.grant_permission()
KernelCommand::CreateScheduledJob → schedule_manager.create()
... (35+ command variants)
```

## Task Execution Loop

See [[Task Execution Flow]] for the detailed step-by-step process.

The core loop:
1. Dequeue task from scheduler
2. Build context window with system prompt
3. Issue capability token
4. Call LLM with context → get response
5. Parse tool calls from response
6. Validate capability token + permissions
7. Execute tools, collect results
8. Push results to context
9. Repeat from step 4 until LLM signals done
10. Mark task complete/failed
