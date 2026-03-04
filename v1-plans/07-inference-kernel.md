# Plan 07 — Inference Kernel (`agentos-kernel` crate)

## Goal

Implement the central kernel that orchestrates everything: the task lifecycle, context assembly, intent routing, and the main run loop. The kernel owns all subsystems (vault, capability engine, audit log, bus, LLM adapters, tools) and mediates every interaction.

## Dependencies

- `agentos-types`, `agentos-audit`, `agentos-vault`, `agentos-capability`, `agentos-bus`, `agentos-llm`, `agentos-tools`
- `tokio` (full features)
- `tracing`, `tracing-subscriber`
- `toml`, `serde`, `serde_json`
- `anyhow`

## Architecture

```
Kernel (owns everything)
  ├── BusServer         — accepts connections from CLI
  ├── TaskScheduler     — priority queue of AgentTasks
  ├── ContextManager    — per-task context windows
  ├── CapabilityEngine  — token issuance + validation
  ├── SecretsVault      — encrypted credential store
  ├── AuditLog          — append-only event log
  ├── ToolRegistry      — loaded tools + manifests
  ├── AgentRegistry     — connected LLM agents
  └── LLM adapters      — the actual LLMCore implementations

Main loop:
  1. Accept bus connections (spawns a handler task per connection)
  2. Receive BusMessage from connection
  3. Route to appropriate handler
  4. For task execution: scheduler loop picks tasks, sends to LLM, processes results
```

## Core Struct: `Kernel`

```rust
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct Kernel {
    pub config: KernelConfig,
    pub audit: Arc<AuditLog>,
    pub vault: Arc<SecretsVault>,
    pub capability_engine: Arc<CapabilityEngine>,
    pub scheduler: Arc<TaskScheduler>,
    pub context_manager: Arc<ContextManager>,
    pub tool_registry: Arc<RwLock<ToolRegistry>>,
    pub agent_registry: Arc<RwLock<AgentRegistry>>,
    pub bus: Arc<BusServer>,
    started_at: chrono::DateTime<chrono::Utc>,
}
```

## Config: `KernelConfig`

Loaded from `config/default.toml`:

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct KernelConfig {
    pub kernel: KernelSettings,
    pub secrets: SecretsSettings,
    pub audit: AuditSettings,
    pub tools: ToolsSettings,
    pub bus: BusSettings,
    pub ollama: OllamaSettings,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KernelSettings {
    pub max_concurrent_tasks: usize,
    pub default_task_timeout_secs: u64,
    pub context_window_max_entries: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SecretsSettings {
    pub vault_path: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuditSettings {
    pub log_path: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolsSettings {
    pub core_tools_dir: String,
    pub user_tools_dir: String,
    pub data_dir: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BusSettings {
    pub socket_path: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OllamaSettings {
    pub host: String,
    pub default_model: String,
}
```

## Task Scheduler

```rust
use std::collections::BinaryHeap;
use tokio::sync::Mutex;

pub struct TaskScheduler {
    /// Priority queue — higher priority tasks are dequeued first.
    queue: Mutex<BinaryHeap<PrioritizedTask>>,
    /// All tasks by ID (active + completed).
    tasks: RwLock<HashMap<TaskID, AgentTask>>,
    max_concurrent: usize,
}

#[derive(Eq, PartialEq)]
struct PrioritizedTask {
    priority: u8,
    created_at: chrono::DateTime<chrono::Utc>,
    task_id: TaskID,
}

// Higher priority first; if equal, older tasks first (FIFO within same priority)
impl Ord for PrioritizedTask { /* ... */ }

impl TaskScheduler {
    pub fn new(max_concurrent: usize) -> Self;

    /// Enqueue a new task. Returns the TaskID.
    pub async fn enqueue(&self, task: AgentTask) -> TaskID;

    /// Dequeue the highest-priority task that is in Queued state.
    pub async fn dequeue(&self) -> Option<AgentTask>;

    /// Update a task's state.
    pub async fn update_state(&self, task_id: &TaskID, state: TaskState)
        -> Result<(), AgentOSError>;

    /// Get a task by ID.
    pub async fn get_task(&self, task_id: &TaskID) -> Option<AgentTask>;

    /// List all tasks (for the CLI `task list` command).
    pub async fn list_tasks(&self) -> Vec<TaskSummary>;

    /// Get currently running task count.
    pub async fn running_count(&self) -> usize;

    /// Check for timed-out tasks and mark them as Failed.
    pub async fn check_timeouts(&self) -> Vec<TaskID>;
}
```

## Context Manager

```rust
pub struct ContextManager {
    /// Per-task context windows.
    windows: RwLock<HashMap<TaskID, ContextWindow>>,
    max_entries: usize,
}

impl ContextManager {
    pub fn new(max_entries: usize) -> Self;

    /// Create a new context window for a task with the system prompt.
    pub fn create_context(&self, task_id: TaskID, system_prompt: &str) -> ContextID;

    /// Push an entry into a task's context window.
    pub async fn push_entry(&self, task_id: &TaskID, entry: ContextEntry)
        -> Result<(), AgentOSError>;

    /// Get the full context for assembling an LLM prompt.
    pub async fn get_context(&self, task_id: &TaskID) -> Result<ContextWindow, AgentOSError>;

    /// Push a tool result into context with sanitization wrappers.
    pub async fn push_tool_result(
        &self,
        task_id: &TaskID,
        tool_name: &str,
        result: &serde_json::Value,
    ) -> Result<(), AgentOSError>;

    /// Remove a task's context (on completion/failure).
    pub async fn remove_context(&self, task_id: &TaskID);
}
```

### System Prompt Template

When a task is created, the context manager injects a system prompt:

````
You are an AI agent operating inside AgentOS.
You have access to the following tools:
{list of tools with descriptions}

To use a tool, respond with a JSON block:
```json
{
  "tool": "tool-name",
  "intent_type": "read|write|execute|query",
  "payload": { ... }
}
````

Respond with your analysis, then use tools as needed to complete your task.
When you have completed the task, respond with your final answer.

````

## Agent Registry

```rust
pub struct AgentRegistry {
    agents: HashMap<AgentID, AgentProfile>,
    name_index: HashMap<String, AgentID>,   // lookup by name
}

impl AgentRegistry {
    pub fn new() -> Self;
    pub fn register(&mut self, profile: AgentProfile) -> AgentID;
    pub fn get_by_id(&self, id: &AgentID) -> Option<&AgentProfile>;
    pub fn get_by_name(&self, name: &str) -> Option<&AgentProfile>;
    pub fn list_all(&self) -> Vec<&AgentProfile>;
    pub fn update_status(&mut self, id: &AgentID, status: AgentStatus);
    pub fn remove(&mut self, id: &AgentID);
}
````

## Tool Registry

```rust
pub struct ToolRegistry {
    tools: HashMap<ToolID, RegisteredTool>,
    name_index: HashMap<String, ToolID>,
}

impl ToolRegistry {
    pub fn new() -> Self;

    /// Load all tool manifests from the core and user tool directories.
    pub fn load_from_dirs(core_dir: &Path, user_dir: &Path) -> Result<Self, AgentOSError>;

    /// Register a single tool from its manifest.
    pub fn register(&mut self, manifest: ToolManifest) -> ToolID;

    pub fn get_by_name(&self, name: &str) -> Option<&RegisteredTool>;
    pub fn get_by_id(&self, id: &ToolID) -> Option<&RegisteredTool>;
    pub fn list_all(&self) -> Vec<&RegisteredTool>;
    pub fn remove(&mut self, name: &str) -> Result<(), AgentOSError>;

    /// Get the list of all tools formatted for the system prompt.
    pub fn tools_for_prompt(&self) -> String;
}
```

## Kernel Main Loop

````rust
impl Kernel {
    /// Boot the kernel: load config, open subsystems, start bus, begin accepting.
    pub async fn boot(config_path: &Path, vault_passphrase: &str) -> Result<Self, anyhow::Error> {
        // 1. Load config
        let config = load_config(config_path)?;

        // 2. Open audit log
        let audit = Arc::new(AuditLog::open(Path::new(&config.audit.log_path))?);
        audit.append(AuditEntry::kernel_started())?;

        // 3. Open or initialize secrets vault
        let vault_path = Path::new(&config.secrets.vault_path);
        let vault = if SecretsVault::is_initialized(vault_path) {
            Arc::new(SecretsVault::open(vault_path, vault_passphrase, audit.clone())?)
        } else {
            Arc::new(SecretsVault::initialize(vault_path, vault_passphrase, audit.clone())?)
        };

        // 4. Initialize capability engine
        let capability_engine = Arc::new(CapabilityEngine::new());

        // 5. Load tools
        let tool_registry = Arc::new(RwLock::new(
            ToolRegistry::load_from_dirs(
                Path::new(&config.tools.core_tools_dir),
                Path::new(&config.tools.user_tools_dir),
            )?
        ));

        // 6. Initialize other subsystems
        let scheduler = Arc::new(TaskScheduler::new(config.kernel.max_concurrent_tasks));
        let context_manager = Arc::new(ContextManager::new(config.kernel.context_window_max_entries));
        let agent_registry = Arc::new(RwLock::new(AgentRegistry::new()));

        // 7. Start bus server
        let bus = Arc::new(BusServer::bind(Path::new(&config.bus.socket_path)).await?);

        Ok(Kernel {
            config, audit, vault, capability_engine, scheduler,
            context_manager, tool_registry, agent_registry, bus,
            started_at: chrono::Utc::now(),
        })
    }

    /// The main run loop. Spawns:
    /// 1. A connection acceptor task (accepts CLI connections)
    /// 2. A task executor loop (picks queued tasks, sends to LLMs)
    /// 3. A timeout checker (periodic scan for expired tasks)
    pub async fn run(self: Arc<Self>) -> Result<(), anyhow::Error> {
        let kernel = self.clone();

        // Spawn connection acceptor
        let acceptor = tokio::spawn({
            let kernel = kernel.clone();
            async move {
                loop {
                    match kernel.bus.accept().await {
                        Ok(conn) => {
                            let kernel = kernel.clone();
                            tokio::spawn(async move {
                                kernel.handle_connection(conn).await;
                            });
                        }
                        Err(e) => {
                            tracing::error!("Bus accept error: {}", e);
                        }
                    }
                }
            }
        });

        // Spawn task executor
        let executor = tokio::spawn({
            let kernel = kernel.clone();
            async move {
                kernel.task_executor_loop().await;
            }
        });

        // Spawn timeout checker (every 10 seconds)
        let timeout_checker = tokio::spawn({
            let kernel = kernel.clone();
            async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    kernel.scheduler.check_timeouts().await;
                }
            }
        });

        // Wait for any task to finish (shouldn't happen unless shutdown)
        tokio::select! {
            _ = acceptor => {},
            _ = executor => {},
            _ = timeout_checker => {},
        }

        Ok(())
    }

    /// Handle a single CLI connection.
    async fn handle_connection(self: &Arc<Self>, mut conn: BusConnection) {
        loop {
            match conn.read().await {
                Ok(BusMessage::Command(cmd)) => {
                    let response = self.handle_command(cmd).await;
                    if conn.write(&BusMessage::CommandResponse(response)).await.is_err() {
                        break; // connection closed
                    }
                }
                Err(_) => break, // connection closed
                _ => {} // ignore unexpected message types
            }
        }
    }

    /// Route a KernelCommand to the appropriate handler.
    async fn handle_command(&self, cmd: KernelCommand) -> KernelResponse {
        match cmd {
            KernelCommand::ConnectAgent { name, provider, model } => {
                self.cmd_connect_agent(name, provider, model).await
            }
            KernelCommand::RunTask { agent_name, prompt } => {
                self.cmd_run_task(agent_name, prompt).await
            }
            KernelCommand::ListTasks => {
                self.cmd_list_tasks().await
            }
            KernelCommand::SetSecret { name, value, scope } => {
                self.cmd_set_secret(name, value, scope).await
            }
            KernelCommand::ListSecrets => {
                self.cmd_list_secrets().await
            }
            KernelCommand::GrantPermission { agent_name, permission } => {
                self.cmd_grant_permission(agent_name, permission).await
            }
            KernelCommand::GetStatus => {
                self.cmd_get_status().await
            }
            // ... handle all other commands
            _ => KernelResponse::Error { message: "Command not implemented".into() },
        }
    }

    /// The task executor loop. Dequeues tasks and processes them.
    async fn task_executor_loop(self: &Arc<Self>) {
        loop {
            // Only process if under the concurrent limit
            if self.scheduler.running_count().await >= self.config.kernel.max_concurrent_tasks {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }

            if let Some(mut task) = self.scheduler.dequeue().await {
                let kernel = self.clone();
                tokio::spawn(async move {
                    kernel.execute_task(&mut task).await;
                });
            } else {
                // No tasks queued — sleep briefly
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }

    /// Execute a single task: assemble context, call LLM, process tool calls, repeat.
    async fn execute_task(&self, task: &mut AgentTask) {
        self.scheduler.update_state(&task.id, TaskState::Running).await.ok();

        // 1. Create context with system prompt
        let tools_desc = self.tool_registry.read().await.tools_for_prompt();
        let system_prompt = format!(
            "You are an AI agent operating inside AgentOS.\n\
             Available tools:\n{}\n\
             To use a tool, respond with a JSON block:\n\
             ```json\n{{\"tool\": \"name\", \"intent_type\": \"read\", \"payload\": {{}}}}\n```\n\
             When done, provide your final answer without any tool calls.",
            tools_desc
        );
        self.context_manager.create_context(task.id, &system_prompt);

        // 2. Push the user's prompt into context
        self.context_manager.push_entry(&task.id, ContextEntry {
            role: ContextRole::User,
            content: task.original_prompt.clone(),
            timestamp: chrono::Utc::now(),
            metadata: None,
        }).await.ok();

        // 3. Agent loop: call LLM, check for tool calls, execute tools, repeat
        let max_iterations = 10; // prevent infinite loops
        for _iteration in 0..max_iterations {
            // Get current context
            let context = match self.context_manager.get_context(&task.id).await {
                Ok(ctx) => ctx,
                Err(_) => break,
            };

            // Find the LLM adapter for this agent
            let agent_registry = self.agent_registry.read().await;
            let agent = match agent_registry.get_by_id(&task.agent_id) {
                Some(a) => a,
                None => {
                    self.scheduler.update_state(&task.id, TaskState::Failed).await.ok();
                    return;
                }
            };
            drop(agent_registry);

            // Call LLM — this is the expensive part
            let llm_result = match self.call_llm(&task.agent_id, &context).await {
                Ok(result) => result,
                Err(e) => {
                    tracing::error!("LLM call failed for task {}: {}", task.id, e);
                    self.scheduler.update_state(&task.id, TaskState::Failed).await.ok();
                    return;
                }
            };

            // Push LLM response into context
            self.context_manager.push_entry(&task.id, ContextEntry {
                role: ContextRole::Assistant,
                content: llm_result.text.clone(),
                timestamp: chrono::Utc::now(),
                metadata: None,
            }).await.ok();

            // Check if the response contains a tool call
            if let Some(tool_call) = self.parse_tool_call(&llm_result.text) {
                // Execute the tool call
                match self.execute_tool_call(task, &tool_call).await {
                    Ok(result) => {
                        self.context_manager.push_tool_result(
                            &task.id,
                            &tool_call.tool_name,
                            &result,
                        ).await.ok();
                    }
                    Err(e) => {
                        // Push error as tool result
                        self.context_manager.push_tool_result(
                            &task.id,
                            &tool_call.tool_name,
                            &serde_json::json!({"error": e.to_string()}),
                        ).await.ok();
                    }
                }
                // Continue loop — LLM will see the tool result and decide next action
            } else {
                // No tool call — LLM has provided a final answer
                break;
            }
        }

        self.scheduler.update_state(&task.id, TaskState::Complete).await.ok();
        self.context_manager.remove_context(&task.id).await;
    }
}
````

## Tool Call Parsing

The LLM's response is scanned for JSON blocks that match the tool call format:

````rust
struct ParsedToolCall {
    tool_name: String,
    intent_type: IntentType,
    payload: serde_json::Value,
}

/// Parse the LLM's text response for a tool call JSON block.
/// Looks for ```json ... ``` blocks containing {"tool": "...", "intent_type": "...", "payload": {...}}
fn parse_tool_call(text: &str) -> Option<ParsedToolCall> {
    // Find JSON code blocks
    let json_block_re = regex::Regex::new(r"```json\s*\n([\s\S]*?)\n```").ok()?;

    for cap in json_block_re.captures_iter(text) {
        if let Some(json_str) = cap.get(1) {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(json_str.as_str()) {
                if let (Some(tool), Some(intent_type)) = (
                    value.get("tool").and_then(|v| v.as_str()),
                    value.get("intent_type").and_then(|v| v.as_str()),
                ) {
                    return Some(ParsedToolCall {
                        tool_name: tool.to_string(),
                        intent_type: parse_intent_type(intent_type)?,
                        payload: value.get("payload").cloned().unwrap_or(serde_json::json!({})),
                    });
                }
            }
        }
    }
    None
}
````

## Kernel Binary Entry Point

The kernel runs as a binary (as part of the CLI or standalone):

```rust
// In agentos-kernel/src/lib.rs — export everything for the CLI to use
// The CLI's `main.rs` will boot the kernel and then start the bus server
```

The kernel is started by the CLI via `agentctl start` or by running the kernel binary directly. For Phase 1, the CLI embeds the kernel directly (single process).

## Tests

````rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_tool_call_valid() {
        let text = r#"I need to read a file. Let me do that.
```json
{"tool": "file-reader", "intent_type": "read", "payload": {"path": "/data/report.txt"}}
```"#;
        let call = parse_tool_call(text).unwrap();
        assert_eq!(call.tool_name, "file-reader");
    }

    #[test]
    fn test_parse_tool_call_no_json() {
        let text = "Here is my final answer: the report is complete.";
        assert!(parse_tool_call(text).is_none());
    }

    #[test]
    fn test_parse_tool_call_invalid_json() {
        let text = "```json\n{invalid json}\n```";
        assert!(parse_tool_call(text).is_none());
    }

    #[tokio::test]
    async fn test_task_scheduler_priority_ordering() {
        let scheduler = TaskScheduler::new(10);

        // Enqueue low priority then high priority
        let low_task = /* create task with priority 1 */;
        let high_task = /* create task with priority 10 */;

        scheduler.enqueue(low_task.clone()).await;
        scheduler.enqueue(high_task.clone()).await;

        // High priority should dequeue first
        let first = scheduler.dequeue().await.unwrap();
        assert_eq!(first.priority, 10);
    }
}
````

## Verification

```bash
cargo test -p agentos-kernel
```
