---
title: Implement Section Content Generators
tags:
  - tools
  - v3
  - next-steps
date: 2026-03-18
status: planned
effort: 4h
priority: high
---

# Implement Section Content Generators

> Replace the `todo!()` stubs in `AgentManualTool` with actual content generators for all 9 manual sections. Each response must be compact JSON under ~500 tokens.

---

## Why This Subtask

This is the core of the agent-manual tool -- the actual documentation content. Each section generator produces a structured JSON response that an LLM agent can parse and act on. The content must be accurate, concise, and useful.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `section_index()` | `todo!()` | Returns JSON listing all 9 sections with one-line descriptions |
| `section_tools()` | `todo!()` | Returns compact table of all tools: name, description, permissions |
| `section_tool_detail()` | `todo!()` | Returns full docs for one tool: description, input_schema, permissions, sandbox config |
| `section_permissions()` | `todo!()` | Returns all permission resource types with rwx explanation |
| `section_memory()` | `todo!()` | Returns memory tier descriptions and usage patterns |
| `section_events()` | `todo!()` | Returns all event categories and event types |
| `section_commands()` | `todo!()` | Returns kernel commands organized by domain |
| `section_errors()` | `todo!()` | Returns common error patterns with recovery suggestions |
| `section_feedback()` | `todo!()` | Returns `[FEEDBACK]` block format specification |

---

## What to Do

Open `crates/agentos-tools/src/agent_manual.rs` and replace all 9 `todo!()` method bodies with the implementations below. Every method returns `Result<serde_json::Value, AgentOSError>`.

### 1. `section_index()`

```rust
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
            {"name": "feedback", "description": "How to emit structured [FEEDBACK] blocks"}
        ],
        "usage": "Call agent-manual with {\"section\": \"<name>\"} to get details. For tool-detail, also pass {\"name\": \"<tool-name>\"}."
    }))
}
```

### 2. `section_tools()`

This is the **dynamic** section. It reads from `self.tool_summaries`.

```rust
fn section_tools(&self) -> Result<serde_json::Value, AgentOSError> {
    let tools: Vec<serde_json::Value> = self.tool_summaries.iter().map(|t| {
        serde_json::json!({
            "name": t.name,
            "description": t.description,
            "permissions": t.permissions,
            "trust_tier": t.trust_tier,
        })
    }).collect();

    Ok(serde_json::json!({
        "section": "tools",
        "count": tools.len(),
        "tools": tools,
        "hint": "Use {\"section\": \"tool-detail\", \"name\": \"<tool-name>\"} for full schema and docs."
    }))
}
```

### 3. `section_tool_detail(name)`

Also dynamic -- looks up a specific tool.

```rust
fn section_tool_detail(&self, name: &str) -> Result<serde_json::Value, AgentOSError> {
    let tool = self.tool_summaries.iter().find(|t| t.name == name)
        .ok_or_else(|| AgentOSError::ToolNotFound(name.to_string()))?;

    Ok(serde_json::json!({
        "section": "tool-detail",
        "name": tool.name,
        "version": tool.version,
        "description": tool.description,
        "permissions": tool.permissions,
        "trust_tier": tool.trust_tier,
        "input_schema": tool.input_schema,
    }))
}
```

### 4. `section_permissions()`

Static content describing the permission model. All resource classes are listed.

```rust
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
```

### 5. `section_memory()`

Static content about the 3 memory tiers.

```rust
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
```

### 6. `section_events()`

Static content listing all event categories and their types.

```rust
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
                "events": ["CPUSpikeDetected", "MemoryPressure", "DiskSpaceLow", "DiskSpaceCritical", "ProcessCrashed", "NetworkInterfaceDown", "ContainerResourceQuotaExceeded", "KernelSubsystemError"]
            },
            {
                "category": "HardwareEvents",
                "events": ["GPUAvailable", "GPUMemoryPressure", "SensorReadingThresholdExceeded", "DeviceConnected", "DeviceDisconnected", "HardwareAccessGranted"]
            },
            {
                "category": "ToolEvents",
                "events": ["ToolInstalled", "ToolRemoved", "ToolExecutionFailed", "ToolSandboxViolation", "ToolResourceQuotaExceeded", "ToolChecksumMismatch", "ToolRegistryUpdated"]
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
```

### 7. `section_commands()`

Static content listing kernel commands grouped by domain.

```rust
fn section_commands(&self) -> Result<serde_json::Value, AgentOSError> {
    Ok(serde_json::json!({
        "section": "commands",
        "description": "Kernel commands available via tool calls. Agents interact with these through structured tool calls, not directly.",
        "domains": [
            {
                "domain": "Task Management",
                "commands": [
                    {"name": "task-delegate", "description": "Delegate a sub-task to another agent", "tool": "task-delegate"},
                    {"name": "RunTask", "description": "Start a new task on a specific or auto-routed agent"},
                    {"name": "CancelTask", "description": "Cancel a running task by ID"},
                    {"name": "ListTasks", "description": "List all active and recent tasks"},
                    {"name": "GetTaskLogs", "description": "Get execution logs for a specific task"}
                ]
            },
            {
                "domain": "Agent Communication",
                "commands": [
                    {"name": "agent-message", "description": "Send a direct message to another agent", "tool": "agent-message"},
                    {"name": "BroadcastToGroup", "description": "Broadcast a message to all agents in a group"},
                    {"name": "CreateAgentGroup", "description": "Create a named group of agents"}
                ]
            },
            {
                "domain": "Memory",
                "commands": [
                    {"name": "memory-search", "description": "Search semantic or episodic memory", "tool": "memory-search"},
                    {"name": "memory-write", "description": "Write to semantic or episodic memory", "tool": "memory-write"},
                    {"name": "memory-block-read/write/list/delete", "description": "CRUD operations on named memory blocks", "tool": "memory-block-*"},
                    {"name": "archival-insert/search", "description": "Insert and search large documents", "tool": "archival-*"}
                ]
            },
            {
                "domain": "File System",
                "commands": [
                    {"name": "file-reader", "description": "Read files, list directories, with pagination", "tool": "file-reader"},
                    {"name": "file-writer", "description": "Write files with create_only/overwrite modes and size guards", "tool": "file-writer"}
                ]
            },
            {
                "domain": "Network",
                "commands": [
                    {"name": "http-client", "description": "HTTP requests with secret injection and SSRF protection", "tool": "http-client"}
                ]
            },
            {
                "domain": "System",
                "commands": [
                    {"name": "shell-exec", "description": "Execute shell commands with timeout", "tool": "shell-exec"},
                    {"name": "sys-monitor", "description": "Get CPU, memory, disk stats", "tool": "sys-monitor"},
                    {"name": "process-manager", "description": "List/kill processes", "tool": "process-manager"},
                    {"name": "network-monitor", "description": "Network interface stats", "tool": "network-monitor"},
                    {"name": "hardware-info", "description": "Hardware and HAL device info", "tool": "hardware-info"}
                ]
            },
            {
                "domain": "Data",
                "commands": [
                    {"name": "data-parser", "description": "Parse JSON, CSV, TOML, YAML data", "tool": "data-parser"}
                ]
            },
            {
                "domain": "Events & Scheduling",
                "commands": [
                    {"name": "EventSubscribe", "description": "Subscribe to OS events (filter by type or category)"},
                    {"name": "EventUnsubscribe", "description": "Remove an event subscription"},
                    {"name": "CreateSchedule", "description": "Create a cron-scheduled recurring task"},
                    {"name": "RunBackground", "description": "Run a task in the background pool"}
                ]
            },
            {
                "domain": "Security & Escalation",
                "commands": [
                    {"name": "ListEscalations", "description": "List pending and resolved escalation requests"},
                    {"name": "ResolveEscalation", "description": "Approve or deny a pending escalation"},
                    {"name": "RollbackTask", "description": "Rollback a task to a previous checkpoint"}
                ]
            },
            {
                "domain": "Pipeline",
                "commands": [
                    {"name": "RunPipeline", "description": "Execute a multi-step pipeline"},
                    {"name": "PipelineStatus", "description": "Check status of a pipeline run"},
                    {"name": "PipelineList", "description": "List installed pipelines"}
                ]
            }
        ]
    }))
}
```

### 8. `section_errors()`

Static content about common error patterns.

```rust
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
```

### 9. `section_feedback()`

Static content about the `[FEEDBACK]` block format.

```rust
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
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-tools/src/agent_manual.rs` | Replace all 9 `todo!()` method stubs with actual content generators |

---

## Prerequisites

[[27-01-Define ManualSection Enum and Query Types]] must be complete first (the file and struct skeleton must exist).

---

## Test Plan

- Add tests in `crates/agentos-tools/src/agent_manual.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(sections.len(), 8); // index is not listed in index
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
}
```

- All tests must pass: `cargo test -p agentos-tools -- agent_manual`
- No clippy warnings: `cargo clippy -p agentos-tools -- -D warnings`

---

## Verification

```bash
cargo build -p agentos-tools
cargo test -p agentos-tools -- agent_manual --nocapture
cargo clippy -p agentos-tools -- -D warnings
```
