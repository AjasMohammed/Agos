use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Which section of the agent manual to query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ManualSection {
    Index,
    Tools,
    ToolDetail,
    Permissions,
    Memory,
    Events,
    Commands,
    Errors,
    Feedback,
    Agents,
    Tasks,
    Procedural,
}

impl ManualSection {
    /// Parse from a string. Returns None for unrecognized sections.
    // Returns Option<Self> rather than Result, so this cannot implement std::str::FromStr.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "index" => Some(Self::Index),
            "tools" => Some(Self::Tools),
            "tool-detail" => Some(Self::ToolDetail),
            "permissions" => Some(Self::Permissions),
            "memory" => Some(Self::Memory),
            "events" => Some(Self::Events),
            "commands" => Some(Self::Commands),
            "errors" => Some(Self::Errors),
            "feedback" => Some(Self::Feedback),
            "agents" => Some(Self::Agents),
            "tasks" => Some(Self::Tasks),
            "procedural" => Some(Self::Procedural),
            _ => None,
        }
    }

    /// All valid section names for the index listing.
    pub fn all_names() -> &'static [&'static str] {
        &[
            "index",
            "tools",
            "tool-detail",
            "permissions",
            "memory",
            "events",
            "commands",
            "errors",
            "feedback",
            "agents",
            "tasks",
            "procedural",
        ]
    }
}

/// Lightweight summary of a registered tool, injected at construction time.
/// Avoids holding a reference to the live ToolRegistry.
#[derive(Debug, Clone, Serialize)]
pub struct ToolSummary {
    pub name: String,
    pub description: String,
    pub version: String,
    /// Permission strings from the manifest, e.g. ["fs.user_data:r"]
    pub permissions: Vec<String>,
    /// Optional JSON Schema for the tool's input payload.
    pub input_schema: Option<serde_json::Value>,
    /// Trust tier: "core", "verified", "community"
    pub trust_tier: String,
}

/// The agent-manual tool. Provides queryable OS documentation.
pub struct AgentManualTool {
    tool_summaries: Vec<ToolSummary>,
}

impl AgentManualTool {
    pub fn new(tool_summaries: Vec<ToolSummary>) -> Self {
        Self { tool_summaries }
    }

    fn schema_type_string(schema: &serde_json::Value) -> String {
        if let Some(type_value) = schema.get("type") {
            if let Some(type_name) = type_value.as_str() {
                return type_name.to_string();
            }
            if let Some(type_arr) = type_value.as_array() {
                let mut names: Vec<String> = type_arr
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect();
                names.sort();
                names.dedup();
                if !names.is_empty() {
                    return names.join("|");
                }
            }
        }

        if schema.get("oneOf").is_some() {
            return "oneOf".to_string();
        }
        if schema.get("anyOf").is_some() {
            return "anyOf".to_string();
        }

        "any".to_string()
    }

    fn summarize_input_schema(schema: Option<&serde_json::Value>) -> Option<serde_json::Value> {
        let schema = schema?;
        let obj = schema.as_object()?;

        let required: HashSet<String> = obj
            .get("required")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        let mut fields = Vec::new();
        if let Some(properties) = obj.get("properties").and_then(|v| v.as_object()) {
            let mut names: Vec<&String> = properties.keys().collect();
            names.sort();

            for name in names {
                if let Some(field_schema) = properties.get(name) {
                    let mut field = serde_json::Map::new();
                    field.insert("name".to_string(), serde_json::Value::String(name.clone()));
                    field.insert(
                        "type".to_string(),
                        serde_json::Value::String(Self::schema_type_string(field_schema)),
                    );
                    field.insert(
                        "required".to_string(),
                        serde_json::Value::Bool(required.contains(name.as_str())),
                    );
                    if let Some(description) =
                        field_schema.get("description").and_then(|v| v.as_str())
                    {
                        field.insert(
                            "description".to_string(),
                            serde_json::Value::String(description.to_string()),
                        );
                    }
                    if let Some(default_value) = field_schema.get("default") {
                        field.insert("default".to_string(), default_value.clone());
                    }
                    if let Some(enum_values) = field_schema.get("enum") {
                        field.insert("enum".to_string(), enum_values.clone());
                    }

                    fields.push(serde_json::Value::Object(field));
                }
            }
        }

        let mut required_names: Vec<String> = required.into_iter().collect();
        required_names.sort();
        let required_fields: Vec<serde_json::Value> = required_names
            .into_iter()
            .map(serde_json::Value::String)
            .collect();

        let mut summary = serde_json::Map::new();
        summary.insert(
            "type".to_string(),
            serde_json::Value::String(Self::schema_type_string(schema)),
        );
        summary.insert(
            "required".to_string(),
            serde_json::Value::Array(required_fields),
        );
        summary.insert("fields".to_string(), serde_json::Value::Array(fields));
        if let Some(any_of) = obj.get("anyOf") {
            summary.insert("any_of".to_string(), any_of.clone());
        }
        if let Some(one_of) = obj.get("oneOf") {
            summary.insert("one_of".to_string(), one_of.clone());
        }

        Some(serde_json::Value::Object(summary))
    }

    /// Build ToolSummary list from a slice of RegisteredTool references.
    /// Called by the kernel/runner when constructing the tool.
    pub fn summaries_from_registry(tools: &[&agentos_types::RegisteredTool]) -> Vec<ToolSummary> {
        tools
            .iter()
            .map(|t| ToolSummary {
                name: t.manifest.manifest.name.clone(),
                description: t.manifest.manifest.description.clone(),
                version: t.manifest.manifest.version.clone(),
                permissions: t.manifest.capabilities_required.permissions.clone(),
                input_schema: t.manifest.input_schema.clone(),
                trust_tier: format!("{:?}", t.manifest.manifest.trust_tier).to_lowercase(),
            })
            .collect()
    }

    fn section_index(&self) -> Result<serde_json::Value, AgentOSError> {
        Ok(serde_json::json!({
            "section": "index",
            "description": "AgentOS Manual — query any section for detailed documentation.",
            "sections": [
                {"name": "tools", "description": "List all available tools with permissions"},
                {"name": "tool-detail", "description": "Full documentation for one tool (pass 'name' field)"},
                {"name": "permissions", "description": "Permission types, resource classes, and rwx model"},
                {"name": "memory", "description": "Memory tiers (semantic, episodic, procedural) and usage"},
                {"name": "events", "description": "Subscribable event types organized by category"},
                {"name": "commands", "description": "Kernel commands invokable via tool calls"},
                {"name": "errors", "description": "Common error patterns and recovery strategies"},
                {"name": "feedback", "description": "How to emit structured [FEEDBACK] blocks"},
                {"name": "agents", "description": "Peer discovery, agent-message, and task delegation patterns"},
                {"name": "tasks", "description": "Task lifecycle, status inspection, and task-list usage"},
                {"name": "procedural", "description": "Procedural memory: record and retrieve step-by-step procedures"}
            ],
            "usage": "Call agent-manual with {\"section\": \"<name>\"} to get details. For tool-detail, also pass {\"name\": \"<tool-name>\"}."
        }))
    }

    fn section_tools(&self) -> Result<serde_json::Value, AgentOSError> {
        let tools: Vec<serde_json::Value> = self
            .tool_summaries
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "permissions": t.permissions,
                    "trust_tier": t.trust_tier,
                })
            })
            .collect();

        Ok(serde_json::json!({
            "section": "tools",
            "count": tools.len(),
            "tools": tools,
            "hint": "Use {\"section\": \"tool-detail\", \"name\": \"<tool-name>\"} for full schema and docs."
        }))
    }

    fn section_tool_detail(&self, name: &str) -> Result<serde_json::Value, AgentOSError> {
        let tool = self
            .tool_summaries
            .iter()
            .find(|t| t.name == name)
            .ok_or_else(|| AgentOSError::ToolNotFound(name.to_string()))?;

        let input_schema_docs = Self::summarize_input_schema(tool.input_schema.as_ref());
        let input_schema_pretty = tool
            .input_schema
            .as_ref()
            .and_then(|schema| serde_json::to_string_pretty(schema).ok());

        Ok(serde_json::json!({
            "section": "tool-detail",
            "name": tool.name,
            "version": tool.version,
            "description": tool.description,
            "permissions": tool.permissions,
            "trust_tier": tool.trust_tier,
            "input_schema": tool.input_schema,
            "input_schema_docs": input_schema_docs,
            "input_schema_pretty": input_schema_pretty,
        }))
    }

    fn section_permissions(&self) -> Result<serde_json::Value, AgentOSError> {
        Ok(serde_json::json!({
            "section": "permissions",
            "model": "resource:rwx — each permission grants read (r), write (w), and/or execute (x) on a resource class.",
            "resource_classes": [
                {"resource": "fs.user_data", "description": "Read/write files in the agent's data directory", "typical_ops": "r, w"},
                {"resource": "memory.semantic", "description": "Search and write to long-term semantic memory", "typical_ops": "r, w"},
                {"resource": "memory.episodic", "description": "Search and write to task-scoped episodic memory", "typical_ops": "r, w"},
                {"resource": "memory.blocks", "description": "Read/write/delete named memory blocks", "typical_ops": "r, w"},
                {"resource": "network.outbound", "description": "Make outbound HTTP requests (SSRF protection blocks private IPs)", "typical_ops": "x"},
                {"resource": "process.exec", "description": "Execute shell commands via shell-exec tool", "typical_ops": "x"},
                {"resource": "vault.secrets", "description": "Read secrets from the encrypted vault", "typical_ops": "r"},
                {"resource": "hal.devices", "description": "Access hardware devices via HAL", "typical_ops": "r, x"},
                {"resource": "audit.read", "description": "Read the audit log", "typical_ops": "r"},
            ],
            "deny_entries": "Deny rules take precedence over grants. Example: grant fs:/home/user/ but deny fs:/home/user/.ssh/ blocks SSH key access.",
            "path_prefix_matching": "Grants like fs:/home/user/ match all paths under that prefix. Partial segment matches are blocked (fs:/home/user does NOT match fs:/home/username).",
            "expiry": "Permissions can have an expires_at timestamp. Expired permissions are treated as absent."
        }))
    }

    fn section_memory(&self) -> Result<serde_json::Value, AgentOSError> {
        Ok(serde_json::json!({
            "section": "memory",
            "tiers": [
                {
                    "tier": "semantic",
                    "description": "Long-term knowledge store with vector embeddings. Persists across tasks. Searchable by natural language query.",
                    "tools": ["memory-write (scope=semantic)", "memory-search (scope=semantic)"],
                    "permission": "memory.semantic:rw",
                    "key_fields": "key (unique ID), content, tags (comma-separated)",
                    "search": "Hybrid vector + FTS5 search. Returns semantic_score, fts_score, rrf_score. Default min_score=0.3."
                },
                {
                    "tier": "episodic",
                    "description": "Task-scoped event log. Each entry is tied to a task_id and agent_id. Auto-written on task completion.",
                    "tools": ["memory-write (scope=episodic)", "memory-search (scope=episodic)"],
                    "permission": "memory.episodic:rw (cross-task search requires memory.episodic:r)",
                    "key_fields": "content, summary, entry_type (observation/action/tool_call/reflection/error)",
                    "search": "FTS5 search within task scope by default. Pass global=true for cross-task search."
                },
                {
                    "tier": "procedural",
                    "description": "Reusable patterns and procedures extracted from successful task completions. Auto-populated by the consolidation engine.",
                    "tools": ["Not directly writable by agents — populated by kernel extraction/consolidation"],
                    "permission": "Read-only via retrieval gate at task start",
                    "search": "Automatically queried by the kernel when starting a task. Relevant procedures injected into context."
                }
            ],
            "memory_blocks": {
                "description": "Named key-value blocks stored as files. Good for structured data that does not need vector search.",
                "tools": ["memory-block-write", "memory-block-read", "memory-block-list", "memory-block-delete"],
                "permission": "memory.blocks:rw"
            },
            "archival": {
                "description": "Archival memory for large documents. Chunked and indexed with embeddings.",
                "tools": ["archival-insert", "archival-search"],
                "permission": "memory.semantic:rw"
            }
        }))
    }

    fn section_events(&self) -> Result<serde_json::Value, AgentOSError> {
        Ok(serde_json::json!({
            "section": "events",
            "description": "Subscribe to events to get notified when things happen. Use the agent-message or task-delegate tools to act on events.",
            "categories": [
                {
                    "category": "AgentLifecycle",
                    "events": ["AgentAdded", "AgentRemoved", "AgentPermissionGranted", "AgentPermissionRevoked"]
                },
                {
                    "category": "TaskLifecycle",
                    "events": ["TaskStarted", "TaskCompleted", "TaskFailed", "TaskTimedOut", "TaskDelegated", "TaskRetrying", "TaskDeadlockDetected", "TaskPreempted"]
                },
                {
                    "category": "SecurityEvents",
                    "events": ["PromptInjectionAttempt", "CapabilityViolation", "UnauthorizedToolAccess", "SecretsAccessAttempt", "SandboxEscapeAttempt", "AuditLogTamperAttempt", "AgentImpersonationAttempt", "UnverifiedToolInstalled"]
                },
                {
                    "category": "MemoryEvents",
                    "events": ["ContextWindowNearLimit", "ContextWindowExhausted", "EpisodicMemoryWritten", "SemanticMemoryConflict", "MemorySearchFailed", "WorkingMemoryEviction"]
                },
                {
                    "category": "SystemHealth",
                    "events": ["CPUSpikeDetected", "MemoryPressure", "DiskSpaceLow", "DiskSpaceCritical", "ProcessCrashed", "NetworkInterfaceDown", "ContainerResourceQuotaExceeded", "KernelSubsystemError", "BudgetWarning", "BudgetExhausted"]
                },
                {
                    "category": "HardwareEvents",
                    "events": ["GPUAvailable", "GPUMemoryPressure", "SensorReadingThresholdExceeded", "DeviceConnected", "DeviceDisconnected", "HardwareAccessGranted"]
                },
                {
                    "category": "ToolEvents",
                    "events": ["ToolInstalled", "ToolRemoved", "ToolExecutionFailed", "ToolSandboxViolation", "ToolResourceQuotaExceeded", "ToolChecksumMismatch", "ToolRegistryUpdated", "ToolCallStarted", "ToolCallCompleted"]
                },
                {
                    "category": "AgentCommunication",
                    "events": ["DirectMessageReceived", "BroadcastReceived", "DelegationReceived", "DelegationResponseReceived", "MessageDeliveryFailed", "AgentUnreachable"]
                },
                {
                    "category": "ScheduleEvents",
                    "events": ["CronJobFired", "ScheduledTaskMissed", "ScheduledTaskCompleted", "ScheduledTaskFailed"]
                },
                {
                    "category": "ExternalEvents",
                    "events": ["WebhookReceived", "ExternalFileChanged", "ExternalAPIEvent", "ExternalAlertReceived"]
                }
            ],
            "subscribe_hint": "Subscriptions are created via kernel command EventSubscribe. Filter by exact type or category. Supports throttle policies."
        }))
    }

    fn section_commands(&self) -> Result<serde_json::Value, AgentOSError> {
        Ok(serde_json::json!({
            "section": "commands",
            "description": "Commands available in AgentOS. Each entry has a 'kernel_only' field. When kernel_only=false, invoke the command by passing the value of its 'tool' field as the tool name in your tool call. When kernel_only=true, the command is an internal kernel operation that agents cannot invoke directly.",
            "domains": [
                {
                    "domain": "Task Management",
                    "commands": [
                        {"name": "task-delegate", "description": "Delegate a sub-task to another agent", "tool": "task-delegate", "kernel_only": false},
                        {"name": "task-list", "description": "List active and recent tasks", "tool": "task-list", "kernel_only": false},
                        {"name": "task-status", "description": "Inspect status of a specific task by ID", "tool": "task-status", "kernel_only": false},
                        {"name": "RunTask", "description": "Start a new task on a specific or auto-routed agent", "kernel_only": true},
                        {"name": "CancelTask", "description": "Cancel a running task by ID", "kernel_only": true},
                        {"name": "GetTaskLogs", "description": "Get execution logs for a specific task", "kernel_only": true}
                    ]
                },
                {
                    "domain": "Agent Communication",
                    "commands": [
                        {"name": "agent-message", "description": "Send a direct message to another agent", "tool": "agent-message", "kernel_only": false},
                        {"name": "agent-list", "description": "List registered agents and their status", "tool": "agent-list", "kernel_only": false},
                        {"name": "BroadcastToGroup", "description": "Broadcast a message to all agents in a group", "kernel_only": true},
                        {"name": "CreateAgentGroup", "description": "Create a named group of agents", "kernel_only": true}
                    ]
                },
                {
                    "domain": "Memory",
                    "commands": [
                        {"name": "memory-search", "description": "Search semantic or episodic memory", "tool": "memory-search", "kernel_only": false},
                        {"name": "memory-write", "description": "Write to semantic or episodic memory", "tool": "memory-write", "kernel_only": false},
                        {"name": "memory-block-read", "description": "Read a named memory block by key", "tool": "memory-block-read", "kernel_only": false},
                        {"name": "memory-block-write", "description": "Write or update a named memory block", "tool": "memory-block-write", "kernel_only": false},
                        {"name": "memory-block-list", "description": "List all named memory blocks", "tool": "memory-block-list", "kernel_only": false},
                        {"name": "memory-block-delete", "description": "Delete a named memory block by key", "tool": "memory-block-delete", "kernel_only": false},
                        {"name": "archival-insert", "description": "Insert a large document into archival memory", "tool": "archival-insert", "kernel_only": false},
                        {"name": "archival-search", "description": "Search archival memory by query", "tool": "archival-search", "kernel_only": false}
                    ]
                },
                {
                    "domain": "File System",
                    "commands": [
                        {"name": "file-reader", "description": "Read files, list directories, with pagination", "tool": "file-reader", "kernel_only": false},
                        {"name": "file-writer", "description": "Write files with create_only/overwrite modes and size guards", "tool": "file-writer", "kernel_only": false}
                    ]
                },
                {
                    "domain": "Network",
                    "commands": [
                        {"name": "http-client", "description": "HTTP requests with secret injection and SSRF protection", "tool": "http-client", "kernel_only": false}
                    ]
                },
                {
                    "domain": "System",
                    "commands": [
                        {"name": "shell-exec", "description": "Execute shell commands with timeout", "tool": "shell-exec", "kernel_only": false},
                        {"name": "sys-monitor", "description": "Get CPU, memory, disk stats", "tool": "sys-monitor", "kernel_only": false},
                        {"name": "process-manager", "description": "List/kill processes", "tool": "process-manager", "kernel_only": false},
                        {"name": "network-monitor", "description": "Network interface stats", "tool": "network-monitor", "kernel_only": false},
                        {"name": "hardware-info", "description": "Hardware and HAL device info", "tool": "hardware-info", "kernel_only": false}
                    ]
                },
                {
                    "domain": "Data",
                    "commands": [
                        {"name": "data-parser", "description": "Parse JSON, CSV, TOML, YAML data", "tool": "data-parser", "kernel_only": false}
                    ]
                },
                {
                    "domain": "Events & Scheduling",
                    "commands": [
                        {"name": "EventSubscribe", "description": "Subscribe to OS events (filter by type or category)", "kernel_only": true},
                        {"name": "EventUnsubscribe", "description": "Remove an event subscription", "kernel_only": true},
                        {"name": "CreateSchedule", "description": "Create a cron-scheduled recurring task", "kernel_only": true},
                        {"name": "RunBackground", "description": "Run a task in the background pool", "kernel_only": true}
                    ]
                },
                {
                    "domain": "Security & Escalation",
                    "commands": [
                        {"name": "ListEscalations", "description": "List pending and resolved escalation requests", "kernel_only": true},
                        {"name": "ResolveEscalation", "description": "Approve or deny a pending escalation", "kernel_only": true},
                        {"name": "RollbackTask", "description": "Rollback a task to a previous checkpoint", "kernel_only": true}
                    ]
                },
                {
                    "domain": "Pipeline",
                    "commands": [
                        {"name": "RunPipeline", "description": "Execute a multi-step pipeline", "kernel_only": true},
                        {"name": "PipelineStatus", "description": "Check status of a pipeline run", "kernel_only": true},
                        {"name": "PipelineList", "description": "List installed pipelines", "kernel_only": true}
                    ]
                }
            ]
        }))
    }

    fn section_errors(&self) -> Result<serde_json::Value, AgentOSError> {
        Ok(serde_json::json!({
            "section": "errors",
            "description": "Common AgentOS errors and how to handle them.",
            "errors": [
                {
                    "error": "PermissionDenied",
                    "pattern": "{resource} requires {operation}",
                    "cause": "Agent lacks the required permission for this resource/operation.",
                    "recovery": "Check which permissions you have. Request escalation if the operation is necessary."
                },
                {
                    "error": "ToolNotFound",
                    "pattern": "Tool not found: {name}",
                    "cause": "The requested tool is not installed or the name is misspelled.",
                    "recovery": "Query {\"section\": \"tools\"} to see available tools. Check spelling."
                },
                {
                    "error": "ToolExecutionFailed",
                    "pattern": "{tool_name}: {reason}",
                    "cause": "The tool ran but encountered an error (bad input, I/O failure, timeout).",
                    "recovery": "Read the reason string. Common causes: invalid path, network timeout, malformed input. Fix input and retry."
                },
                {
                    "error": "SchemaValidation",
                    "pattern": "Schema validation failed: {details}",
                    "cause": "The input payload does not match the tool's expected schema.",
                    "recovery": "Query {\"section\": \"tool-detail\", \"name\": \"<tool>\"} to see the input schema."
                },
                {
                    "error": "FileLocked",
                    "pattern": "File '{path}' is locked by agent {holder}",
                    "cause": "Another agent has an exclusive write lock on this file.",
                    "recovery": "Wait and retry, or read a different file. Locks are released after write completes."
                },
                {
                    "error": "TaskTimeout",
                    "pattern": "Task timed out: {task_id}",
                    "cause": "The task exceeded its configured timeout.",
                    "recovery": "Break work into smaller sub-tasks. Delegate to other agents if needed."
                },
                {
                    "error": "ToolBlocked",
                    "pattern": "Tool '{name}' is blocked",
                    "cause": "The tool has been revoked and cannot be loaded.",
                    "recovery": "Use an alternative tool. This tool was blocked by an administrator."
                },
                {
                    "error": "NoLLMConnected",
                    "pattern": "No LLM connected",
                    "cause": "No LLM adapter is available for inference.",
                    "recovery": "This is a system configuration issue. Cannot be resolved by the agent."
                },
                {
                    "error": "BudgetExhausted",
                    "pattern": "Budget check: HardLimit",
                    "cause": "The agent's token or cost budget has been exceeded.",
                    "recovery": "Complete the current task with available context. Model may be auto-downgraded."
                }
            ]
        }))
    }

    fn section_agents(&self) -> Result<serde_json::Value, AgentOSError> {
        Ok(serde_json::json!({
            "section": "agents",
            "title": "Agent Discovery & Coordination",
            "summary": "How to find available agents and coordinate with them.",
            "subsections": [
                {
                    "title": "Discover Peers",
                    "content": "Use 'agent-list' to see all registered agents with their status. Filter by status with {\"status\": \"idle\"} to find available agents. Required permission: agent.registry:r"
                },
                {
                    "title": "Send a Message",
                    "content": "Use 'agent-message' to send a message to a named agent. The message is queued for the agent's next iteration. Required permission: agent.message:x"
                },
                {
                    "title": "Delegate a Task",
                    "content": "Use 'task-delegate' to hand off a sub-task to another agent. Provide {\"agent\": \"<name>\", \"task\": \"<prompt>\", \"priority\": 1-10}. The delegation is non-blocking — control returns immediately. Use 'task-status' with the returned task ID to monitor completion."
                },
                {
                    "title": "Coordination Pattern",
                    "content": "1. Call 'think' to plan the delegation strategy. 2. Call 'agent-list' to find available agents. 3. Call 'task-delegate' with the selected agent. 4. Poll 'task-status' until status='complete' or 'failed'. 5. Act on the result."
                }
            ]
        }))
    }

    fn section_tasks(&self) -> Result<serde_json::Value, AgentOSError> {
        Ok(serde_json::json!({
            "section": "tasks",
            "title": "Task Lifecycle",
            "summary": "Task states, introspection tools, and how to interpret results.",
            "subsections": [
                {
                    "title": "Task States",
                    "content": "queued → running → complete | failed | cancelled. A task starts as 'queued' when created. It becomes 'running' when an agent picks it up. Terminal states are 'complete', 'failed', and 'cancelled'. 'waiting' means the task is paused waiting for a sub-agent or tool."
                },
                {
                    "title": "Inspect a Task",
                    "content": "Use 'task-status' with {\"task_id\": \"<uuid>\"}. Returns: id, description, status, agent_id, created_at, started_at. Required permission: task.query:r"
                },
                {
                    "title": "List Your Tasks",
                    "content": "Use 'task-list' with {\"filter\": \"mine\"} (default) for your tasks, or {\"filter\": \"active\"} for all running/queued tasks across agents. Optional 'limit' field (default 20, max 100). Required permission: task.query:r"
                },
                {
                    "title": "Best Practices",
                    "content": "After delegating, store the returned task ID in episodic memory or a memory block. Poll 'task-status' to detect completion. Use 'memory-search' or 'file-reader' to retrieve detailed results written by the delegated task."
                }
            ]
        }))
    }

    fn section_procedural(&self) -> Result<serde_json::Value, AgentOSError> {
        Ok(serde_json::json!({
            "section": "procedural",
            "title": "Procedural Memory",
            "summary": "How to record and retrieve step-by-step procedures for future reuse.",
            "subsections": [
                {
                    "title": "What is Procedural Memory",
                    "content": "Procedural memory stores how-to knowledge: step-by-step procedures, SOPs, and task templates. Unlike semantic memory (facts) or episodic memory (events), procedural memory records *actions* in order. Procedures are shared across agents and survive across restarts."
                },
                {
                    "title": "Record a Procedure",
                    "content": "Use 'procedure-create' with: {\"name\": \"<short name>\", \"description\": \"<what it does>\", \"steps\": [{\"action\": \"...\", \"tool\": \"<tool-name>\", \"expected_outcome\": \"...\"}], \"preconditions\": [...], \"postconditions\": [...], \"tags\": [...]}. Required permission: memory.procedural:w"
                },
                {
                    "title": "Find a Procedure",
                    "content": "Use 'procedure-search' with {\"query\": \"<description of what you want to do>\", \"top_k\": 5}. Returns procedures ranked by semantic similarity. Check the 'steps' array for the exact sequence. Required permission: memory.procedural:r"
                },
                {
                    "title": "When to Record",
                    "content": "Record a procedure when you successfully complete a multi-step task you are likely to repeat. Include the tools used in each step's 'tool' field so future agents can validate they have the right permissions before starting."
                }
            ]
        }))
    }

    fn section_feedback(&self) -> Result<serde_json::Value, AgentOSError> {
        Ok(serde_json::json!({
            "section": "feedback",
            "description": "Emit structured [FEEDBACK] blocks to report observations about the OS, tools, or task execution quality.",
            "format": {
                "block_start": "[FEEDBACK]",
                "block_end": "[/FEEDBACK]",
                "fields": [
                    {"field": "category", "required": true, "values": ["bug", "ux", "performance", "suggestion", "documentation"]},
                    {"field": "severity", "required": true, "values": ["low", "medium", "high", "critical"]},
                    {"field": "component", "required": true, "description": "Which tool, system, or feature the feedback is about"},
                    {"field": "description", "required": true, "description": "Clear description of the issue or suggestion"},
                    {"field": "reproduction", "required": false, "description": "Steps to reproduce (for bugs)"},
                    {"field": "expected", "required": false, "description": "What should have happened"},
                    {"field": "actual", "required": false, "description": "What actually happened"}
                ]
            },
            "example": "[FEEDBACK]\ncategory: bug\nseverity: medium\ncomponent: file-reader\ndescription: file-reader returns empty content for symlinked files\nexpected: Should follow symlink and return target file content\nactual: Returns {\"content\": \"\", \"size_bytes\": 0}\n[/FEEDBACK]"
        }))
    }
}

#[async_trait]
impl AgentTool for AgentManualTool {
    fn name(&self) -> &str {
        "agent-manual"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        // No permissions required — this is read-only public documentation.
        vec![]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let section_str = payload
            .get("section")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation(
                    "agent-manual requires 'section' field. Valid sections: index, tools, tool-detail, permissions, memory, events, commands, errors, feedback".into(),
                )
            })?;

        let section = ManualSection::from_str(section_str).ok_or_else(|| {
            AgentOSError::SchemaValidation(format!(
                "Unknown manual section '{}'. Valid sections: {}",
                section_str,
                ManualSection::all_names().join(", ")
            ))
        })?;

        match section {
            ManualSection::Index => self.section_index(),
            ManualSection::Tools => self.section_tools(),
            ManualSection::ToolDetail => {
                let name = payload
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        AgentOSError::SchemaValidation(
                            "tool-detail section requires 'name' field".into(),
                        )
                    })?;
                self.section_tool_detail(name)
            }
            ManualSection::Permissions => self.section_permissions(),
            ManualSection::Memory => self.section_memory(),
            ManualSection::Events => self.section_events(),
            ManualSection::Commands => self.section_commands(),
            ManualSection::Errors => self.section_errors(),
            ManualSection::Feedback => self.section_feedback(),
            ManualSection::Agents => self.section_agents(),
            ManualSection::Tasks => self.section_tasks(),
            ManualSection::Procedural => self.section_procedural(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manual_section_from_str() {
        assert_eq!(ManualSection::from_str("index"), Some(ManualSection::Index));
        assert_eq!(ManualSection::from_str("tools"), Some(ManualSection::Tools));
        assert_eq!(
            ManualSection::from_str("tool-detail"),
            Some(ManualSection::ToolDetail)
        );
        assert_eq!(
            ManualSection::from_str("permissions"),
            Some(ManualSection::Permissions)
        );
        assert_eq!(
            ManualSection::from_str("memory"),
            Some(ManualSection::Memory)
        );
        assert_eq!(
            ManualSection::from_str("events"),
            Some(ManualSection::Events)
        );
        assert_eq!(
            ManualSection::from_str("commands"),
            Some(ManualSection::Commands)
        );
        assert_eq!(
            ManualSection::from_str("errors"),
            Some(ManualSection::Errors)
        );
        assert_eq!(
            ManualSection::from_str("feedback"),
            Some(ManualSection::Feedback)
        );
        assert_eq!(
            ManualSection::from_str("agents"),
            Some(ManualSection::Agents)
        );
        assert_eq!(ManualSection::from_str("tasks"), Some(ManualSection::Tasks));
        assert_eq!(
            ManualSection::from_str("procedural"),
            Some(ManualSection::Procedural)
        );
        assert_eq!(ManualSection::from_str("nonexistent"), None);
    }

    #[test]
    fn test_all_names_count() {
        assert_eq!(ManualSection::all_names().len(), 12);
    }

    #[test]
    fn test_summaries_from_registry_empty() {
        let summaries = AgentManualTool::summaries_from_registry(&[]);
        assert!(summaries.is_empty());
    }

    fn make_test_summaries() -> Vec<ToolSummary> {
        vec![
            ToolSummary {
                name: "file-reader".into(),
                description: "Read files".into(),
                version: "1.1.0".into(),
                permissions: vec!["fs.user_data:r".into()],
                input_schema: None,
                trust_tier: "core".into(),
            },
            ToolSummary {
                name: "http-client".into(),
                description: "HTTP requests".into(),
                version: "1.0.0".into(),
                permissions: vec!["network.outbound:x".into()],
                input_schema: None,
                trust_tier: "core".into(),
            },
        ]
    }

    #[test]
    fn test_section_index_has_all_sections() {
        let tool = AgentManualTool::new(vec![]);
        let result = tool.section_index().unwrap();
        let sections = result["sections"].as_array().unwrap();
        assert_eq!(sections.len(), 11); // index is not listed in index
    }

    #[test]
    fn test_section_tools_returns_count() {
        let tool = AgentManualTool::new(make_test_summaries());
        let result = tool.section_tools().unwrap();
        assert_eq!(result["count"], 2);
        assert_eq!(result["tools"][0]["name"], "file-reader");
    }

    #[test]
    fn test_section_tool_detail_found() {
        let tool = AgentManualTool::new(make_test_summaries());
        let result = tool.section_tool_detail("file-reader").unwrap();
        assert_eq!(result["name"], "file-reader");
        assert_eq!(result["version"], "1.1.0");
    }

    #[test]
    fn test_section_tool_detail_not_found() {
        let tool = AgentManualTool::new(make_test_summaries());
        let result = tool.section_tool_detail("nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn test_section_tool_detail_includes_schema_docs() {
        let tool = AgentManualTool::new(vec![ToolSummary {
            name: "file-reader".into(),
            description: "Read files".into(),
            version: "1.1.0".into(),
            permissions: vec!["fs.user_data:r".into()],
            input_schema: Some(serde_json::json!({
                "type": "object",
                "required": ["path"],
                "properties": {
                    "path": { "type": "string", "description": "File path" },
                    "offset": { "type": "integer", "default": 0 }
                }
            })),
            trust_tier: "core".into(),
        }]);

        let result = tool.section_tool_detail("file-reader").unwrap();
        assert_eq!(result["section"], "tool-detail");
        assert!(result["input_schema_docs"]["fields"].is_array());
        assert!(result["input_schema_docs"]["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["name"] == "path" && f["required"] == true));
        assert!(result["input_schema_pretty"].as_str().is_some());
    }

    #[test]
    fn test_section_permissions_has_resource_classes() {
        let tool = AgentManualTool::new(vec![]);
        let result = tool.section_permissions().unwrap();
        let classes = result["resource_classes"].as_array().unwrap();
        assert!(classes.len() >= 5);
    }

    #[test]
    fn test_section_memory_has_three_tiers() {
        let tool = AgentManualTool::new(vec![]);
        let result = tool.section_memory().unwrap();
        let tiers = result["tiers"].as_array().unwrap();
        assert_eq!(tiers.len(), 3);
    }

    #[test]
    fn test_section_events_has_all_categories() {
        let tool = AgentManualTool::new(vec![]);
        let result = tool.section_events().unwrap();
        let categories = result["categories"].as_array().unwrap();
        assert_eq!(categories.len(), 10);
    }

    #[test]
    fn test_section_commands_has_domains() {
        let tool = AgentManualTool::new(vec![]);
        let result = tool.section_commands().unwrap();
        let domains = result["domains"].as_array().unwrap();
        assert!(domains.len() >= 8);
    }

    #[test]
    fn test_section_commands_kernel_only_distinction() {
        let tool = AgentManualTool::new(vec![]);
        let result = tool.section_commands().unwrap();
        let domains = result["domains"].as_array().unwrap();

        // Flatten all commands across all domains
        let all_commands: Vec<&serde_json::Value> = domains
            .iter()
            .flat_map(|d| {
                d["commands"]
                    .as_array()
                    .map(|v| v.iter().collect::<Vec<_>>())
                    .unwrap_or_default()
            })
            .collect();

        // Every command must have a kernel_only field
        for cmd in &all_commands {
            assert!(
                cmd.get("kernel_only").is_some(),
                "command {:?} is missing kernel_only field",
                cmd["name"]
            );
        }

        // Tool-accessible commands have both a "tool" field and kernel_only=false
        let tool_accessible: Vec<&serde_json::Value> = all_commands
            .iter()
            .copied()
            .filter(|c| c["kernel_only"] == false)
            .collect();
        for cmd in &tool_accessible {
            assert!(
                cmd.get("tool").is_some(),
                "tool-accessible command {:?} should have a 'tool' field",
                cmd["name"]
            );
        }

        // Kernel-only commands must not have a "tool" field
        let kernel_only: Vec<&serde_json::Value> = all_commands
            .iter()
            .copied()
            .filter(|c| c["kernel_only"] == true)
            .collect();
        for cmd in &kernel_only {
            assert!(
                cmd.get("tool").is_none(),
                "kernel-only command {:?} must not have a 'tool' field",
                cmd["name"]
            );
        }

        // Sanity: both categories must be non-empty
        assert!(
            !tool_accessible.is_empty(),
            "expected some tool-accessible commands"
        );
        assert!(
            !kernel_only.is_empty(),
            "expected some kernel-only commands"
        );
    }

    #[test]
    fn test_section_errors_has_entries() {
        let tool = AgentManualTool::new(vec![]);
        let result = tool.section_errors().unwrap();
        let errors = result["errors"].as_array().unwrap();
        assert!(errors.len() >= 5);
    }

    #[test]
    fn test_section_feedback_has_format() {
        let tool = AgentManualTool::new(vec![]);
        let result = tool.section_feedback().unwrap();
        assert!(result["format"]["fields"].as_array().unwrap().len() >= 4);
    }

    #[test]
    fn test_section_agents_has_subsections() {
        let tool = AgentManualTool::new(vec![]);
        let result = tool.section_agents().unwrap();
        assert_eq!(result["section"], "agents");
        let subsections = result["subsections"].as_array().unwrap();
        assert!(subsections.len() >= 3);
        // Must include coordination pattern
        let titles: Vec<_> = subsections
            .iter()
            .filter_map(|s| s["title"].as_str())
            .collect();
        assert!(titles.iter().any(|t| t.contains("Coordination")));
    }

    #[test]
    fn test_section_tasks_has_states_and_inspect() {
        let tool = AgentManualTool::new(vec![]);
        let result = tool.section_tasks().unwrap();
        assert_eq!(result["section"], "tasks");
        let subsections = result["subsections"].as_array().unwrap();
        assert!(subsections.len() >= 3);
        let titles: Vec<_> = subsections
            .iter()
            .filter_map(|s| s["title"].as_str())
            .collect();
        assert!(titles.iter().any(|t| t.contains("States")));
        assert!(titles.iter().any(|t| t.contains("Inspect")));
    }

    #[test]
    fn test_section_procedural_has_record_and_find() {
        let tool = AgentManualTool::new(vec![]);
        let result = tool.section_procedural().unwrap();
        assert_eq!(result["section"], "procedural");
        let subsections = result["subsections"].as_array().unwrap();
        assert!(subsections.len() >= 3);
        let titles: Vec<_> = subsections
            .iter()
            .filter_map(|s| s["title"].as_str())
            .collect();
        assert!(titles.iter().any(|t| t.contains("Record")));
        assert!(titles.iter().any(|t| t.contains("Find")));
    }
}
