---
title: "Phase 02 — Pipeline Security Hardening"
tags:
  - kernel
  - security
  - pipeline
  - next-steps
  - v3
date: 2026-03-17
status: complete
effort: 1d
priority: critical
---

# Phase 02 — Pipeline Security Hardening

> Fix `KernelPipelineExecutor` and `OwnedPipelineExecutor` to enforce capability tokens, injection scanning, intent validation, and proper permissions instead of running tools with `PermissionSet::new()` (empty).

---

## Why This Phase

Both pipeline executors in `crates/agentos-kernel/src/commands/pipeline.rs` create `ToolExecutionContext` with `permissions: PermissionSet::new()` -- an empty permission set. This means:

1. **Any tool executed through a pipeline runs with zero permissions.** Tools that check permissions will fail; tools that do not check will run unconstrained.
2. **`OwnedPipelineExecutor::run_agent_task()` does raw LLM inference** without injection scanning, intent validation, risk classification, capability token validation, event emission, or episodic memory recording. This bypasses every security layer the kernel enforces for normal task execution.
3. **No audit trail** is produced for pipeline tool executions beyond what the tool runner itself logs.

This is a critical security gap: an attacker who can trigger a pipeline effectively bypasses the entire security architecture.

---

## Current --> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `KernelPipelineExecutor::run_tool()` permissions | `PermissionSet::new()` (empty) | Agent's actual `PermissionSet` from `CapabilityEngine` |
| `OwnedPipelineExecutor::run_tool()` permissions | `PermissionSet::new()` (empty) | Agent's actual `PermissionSet` from `CapabilityEngine` |
| `OwnedPipelineExecutor::run_agent_task()` injection scanning | None | Full `InjectionScanner` scan on LLM output |
| `OwnedPipelineExecutor::run_agent_task()` intent validation | None | `IntentValidator::validate()` on parsed tool calls |
| `OwnedPipelineExecutor::run_agent_task()` event emission | None | `TaskStarted`, `TaskCompleted`/`TaskFailed` events emitted |
| `OwnedPipelineExecutor::run_agent_task()` audit logging | None | `ToolExecutionStarted`, `ToolExecutionCompleted` audit entries |
| Pipeline agent_id | `AgentID::new()` (random, unregistered) | Resolved from `agent_name` parameter or pipeline owner |

---

## What to Do

### Step 1: Fix the agent_id problem

Both executors create `AgentID::new()` -- a random UUID that is not registered with the agent registry. This means capability lookups, budget checks, and permission resolution all fail silently or return empty results.

**In `cmd_run_pipeline()` (line ~79 and ~139):**

The pipeline must be associated with a real agent. Either:
- (a) Accept an `agent_name` parameter in the `RunPipeline` command and resolve it to an `AgentID`
- (b) Use a default "pipeline-engine" agent that is auto-registered at kernel boot

**Recommended: Option (a)** -- modify the `RunPipeline` bus command to include the requesting agent's name/ID. The CLI already knows which agent is running.

For now, if no agent is specified, use the first registered agent or return an error:

```rust
// In cmd_run_pipeline():
let agent_id = if let Some(agent_name) = agent_name {
    let registry = self.agent_registry.read().await;
    match registry.get_by_name(&agent_name) {
        Some(agent) => agent.id,
        None => return KernelResponse::Error {
            message: format!("Agent '{}' not found for pipeline execution", agent_name),
        },
    }
} else {
    return KernelResponse::Error {
        message: "Pipeline execution requires an agent_name parameter".to_string(),
    };
};
```

### Step 2: Fix `KernelPipelineExecutor::run_tool()` permissions

Open `crates/agentos-kernel/src/commands/pipeline.rs`, line ~272-296.

Replace the empty `PermissionSet::new()` with the agent's actual permissions:

```rust
async fn run_tool(
    &self,
    tool_name: &str,
    input: serde_json::Value,
) -> Result<String, AgentOSError> {
    // Resolve the agent's permissions from the capability engine
    let permissions = self.kernel.capability_engine
        .get_permissions_for_agent(&self.agent_id)
        .await
        .unwrap_or_else(|_| PermissionSet::new());

    let context = ToolExecutionContext {
        data_dir: self.kernel.data_dir.clone(),
        task_id: TaskID::new(),
        agent_id: self.agent_id,  // Use the real agent_id, not AgentID::new()
        trace_id: TraceID::new(),
        permissions,  // Use resolved permissions
        vault: Some(std::sync::Arc::new(agentos_vault::ProxyVault::new(
            self.kernel.vault.clone(),
        ))),
        hal: Some(self.kernel.hal.clone()),
        file_lock_registry: None,
    };

    // Audit log the tool execution
    self.kernel.audit_log(agentos_audit::AuditEntry {
        timestamp: chrono::Utc::now(),
        trace_id: context.trace_id,
        event_type: agentos_audit::AuditEventType::ToolExecutionStarted,
        agent_id: Some(self.agent_id),
        task_id: Some(context.task_id),
        tool_id: None,
        details: serde_json::json!({
            "tool_name": tool_name,
            "source": "pipeline",
        }),
        severity: agentos_audit::AuditSeverity::Info,
        reversible: false,
        rollback_ref: None,
    });

    let result = self.kernel.tool_runner.execute(tool_name, input, context).await?;
    Ok(serde_json::to_string(&result).unwrap_or_default())
}
```

### Step 3: Fix `OwnedPipelineExecutor::run_tool()` permissions

Same fix as Step 2 but for the owned executor (line ~408-428). Since `OwnedPipelineExecutor` holds `Arc` references:

```rust
async fn run_tool(
    &self,
    tool_name: &str,
    input: serde_json::Value,
) -> Result<String, AgentOSError> {
    // Resolve permissions from the capability engine
    let permissions = self.capability_engine
        .get_permissions_for_agent(&self.agent_id)
        .await
        .unwrap_or_else(|_| PermissionSet::new());

    let context = ToolExecutionContext {
        data_dir: self.data_dir.clone(),
        task_id: TaskID::new(),
        agent_id: self.agent_id,  // Real agent_id
        trace_id: TraceID::new(),
        permissions,  // Resolved permissions
        vault: Some(std::sync::Arc::new(agentos_vault::ProxyVault::new(
            self.vault.clone(),
        ))),
        hal: Some(self.hal.clone()),
        file_lock_registry: None,
    };

    let result = self.tool_runner.execute(tool_name, input, context).await?;
    Ok(serde_json::to_string(&result).unwrap_or_default())
}
```

**Add `capability_engine` to `OwnedPipelineExecutor`:**

```rust
pub(crate) struct OwnedPipelineExecutor {
    // ... existing fields ...
    pub(crate) capability_engine: Arc<CapabilityEngine>,
    pub(crate) injection_scanner: Arc<crate::injection_scanner::InjectionScanner>,
    pub(crate) intent_validator: Arc<crate::intent_validator::IntentValidator>,
    pub(crate) event_sender: tokio::sync::mpsc::Sender<agentos_types::EventMessage>,
    pub(crate) audit: Arc<AuditLog>,
}
```

Update the executor construction in `cmd_run_pipeline()` to pass these fields:

```rust
let executor = OwnedPipelineExecutor {
    // ... existing fields ...
    capability_engine: self.capability_engine.clone(),
    injection_scanner: self.injection_scanner.clone(),
    intent_validator: self.intent_validator.clone(),
    event_sender: self.event_sender.clone(),
    audit: self.audit.clone(),
};
```

### Step 4: Harden `OwnedPipelineExecutor::run_agent_task()`

The current implementation (lines 345-406) does raw LLM inference without any security checks. Add:

1. **Injection scanning on LLM output:**

```rust
let inference = llm.infer(&context).await?;

// Scan inference output for injection attempts
let scan_result = self.injection_scanner.scan(&inference.text);
if scan_result.is_suspicious {
    if matches!(scan_result.max_threat, Some(crate::injection_scanner::ThreatLevel::High)) {
        self.audit.append(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
            event_type: agentos_audit::AuditEventType::RiskEscalation,
            agent_id: Some(agent.id),
            task_id: Some(task_id),
            tool_id: None,
            details: serde_json::json!({
                "source": "pipeline",
                "threat_level": "high",
                "matches": scan_result.matches.len(),
            }),
            severity: agentos_audit::AuditSeverity::Warn,
            reversible: false,
            rollback_ref: None,
        }).ok();
    }
}
```

2. **Event emission for task lifecycle:**

```rust
// Before inference:
crate::event_dispatch::emit_signed_event(
    &self.capability_engine,
    &self.audit,
    &self.event_sender,
    EventType::TaskStarted,
    EventSource::TaskScheduler,
    EventSeverity::Info,
    serde_json::json!({
        "task_id": task_id.to_string(),
        "agent_name": agent_name,
        "source": "pipeline",
    }),
    0, TraceID::new(),
    Some(agent.id), Some(task_id),
);

// After successful inference:
crate::event_dispatch::emit_signed_event(
    &self.capability_engine,
    &self.audit,
    &self.event_sender,
    EventType::TaskCompleted,
    EventSource::TaskScheduler,
    EventSeverity::Info,
    serde_json::json!({
        "task_id": task_id.to_string(),
        "agent_name": agent_name,
        "source": "pipeline",
    }),
    0, TraceID::new(),
    Some(agent.id), Some(task_id),
);
```

3. **Budget check before inference** (already exists via `check_budget()` -- good)

### Step 5: Add audit logging for pipeline operations

Add `AuditEntry` logging for pipeline install, run, and remove operations in the existing `cmd_*` methods. The `cmd_install_pipeline()` and `cmd_run_pipeline()` methods should log audit entries similar to other kernel commands.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/commands/pipeline.rs` | Fix permissions in both `run_tool()` impls; add injection scanning, event emission, and audit logging to `OwnedPipelineExecutor::run_agent_task()`; add `capability_engine`, `injection_scanner`, `intent_validator`, `event_sender`, `audit` fields to `OwnedPipelineExecutor`; resolve real `agent_id` instead of `AgentID::new()` |
| `crates/agentos-bus/src/message.rs` | Add `agent_name: Option<String>` to `RunPipeline` command variant (if not already present) |

---

## Prerequisites

None -- this phase is independent. However, it pairs well with Phase 01 (emit missing events) since the pipeline will now emit events.

---

## Test Plan

1. **Permission enforcement test:**
   - Create a pipeline that calls a tool requiring `fs_read` permission
   - Run with an agent that has `fs_read` -- should succeed
   - Run with an agent that does NOT have `fs_read` -- should fail with permission denied
   - Verify the error message references the missing permission

2. **Injection scanning test:**
   - Create a pipeline with an agent task step
   - Mock the LLM to return output containing injection patterns ("ignore previous instructions")
   - Verify an audit entry with `RiskEscalation` is created

3. **Agent resolution test:**
   - Run a pipeline without specifying an agent -- should return an error
   - Run a pipeline with an unregistered agent name -- should return "Agent not found"
   - Run with a valid agent -- should succeed with that agent's permissions

4. **Event emission test:**
   - Run a pipeline and verify `TaskStarted` and `TaskCompleted` events are emitted
   - Verify the events include `"source": "pipeline"` in their payload

5. **Audit trail test:**
   - Run a pipeline with tool execution steps
   - Query the audit log for `ToolExecutionStarted` events
   - Verify entries exist with `"source": "pipeline"`

---

## Verification

```bash
cargo build -p agentos-kernel
cargo test -p agentos-kernel -- pipeline --nocapture
cargo test -p agentos-kernel -- --nocapture
cargo clippy -p agentos-kernel -- -D warnings
```

To manually verify permissions are enforced:
```bash
# Start kernel, connect an agent without fs_write permission
# Install a pipeline that uses the file-write tool
# Run it -- should fail with PermissionDenied
agentctl pipeline install my-pipeline.yaml
agentctl pipeline run my-pipeline --agent test-agent --input "{}"
# Expected: error about missing permission
```

---

## Related

- [[Unwired Features Plan]] -- Parent plan
- [[22-Unwired Features]] -- Next-steps parent index
