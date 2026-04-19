---
title: "Phase 1: Capability Provider Trait & Registry"
tags:
  - kernel
  - capabilities
  - v4
  - phase-1
date: 2026-04-12
status: planned
effort: 2d
priority: critical
---

# Phase 1: Capability Provider Trait & Registry

> Define the `CapabilityProvider` trait, build the `CapabilityRegistry`, wire it into the kernel, and add new audit event types — the foundation all other phases build on.

---

## Why This Phase

Every managed capability (environments, processes, networking, builds, storage) needs a common abstraction. Without a shared trait and registry, each capability domain would be a one-off tool with ad-hoc policy checks. The provider pattern ensures every domain gets:
- Uniform permission validation
- Policy hook integration
- Structured audit logging
- Resource accounting
- Dynamic capability negotiation (Phase 7)

This is the architectural backbone. All other phases implement specific providers against this trait.

---

## Current State

- Tools implement `AgentTool` trait (`crates/agentos-tools/src/traits.rs:8`)
- `ToolExecutionContext` carries `data_dir`, `task_id`, `agent_id`, `permissions`, `vault`, `hal` (`traits.rs:39-59`)
- HAL has `HalDriver` trait with `query(params) -> Result<Value>` pattern
- Audit log has 83+ event types in `AuditEventType` enum (`crates/agentos-audit/src/log.rs:16`)
- Kernel task executor validates permissions before tool execution (`task_executor.rs`)

## Target State

- New `CapabilityProvider` trait in `crates/agentos-kernel/src/capability_provider.rs`
- New `CapabilityRegistry` that holds `Arc<dyn CapabilityProvider>` per domain
- Registry wired into kernel state and accessible from `ToolExecutionContext`
- New audit event types for capability actions
- New permission resource prefixes: `env.*`, `proc.*`, `net.*`, `build.*`, `storage.*`

---

## Detailed Subtasks

### 1. Define the `CapabilityProvider` trait

**File:** `crates/agentos-kernel/src/capability_provider.rs` (new)

```rust
use agentos_types::*;
use async_trait::async_trait;
use serde_json::Value;

/// A managed capability domain that the kernel mediates on behalf of agents.
///
/// Each provider handles a family of related actions (e.g., all `env.*`
/// operations). The kernel validates permissions and fires policy hooks
/// before calling `execute`. Providers return structured JSON results.
#[async_trait]
pub trait CapabilityProvider: Send + Sync {
    /// Domain prefix for this provider (e.g., "env", "proc", "net").
    fn domain(&self) -> &str;

    /// List of actions this provider supports (e.g., ["install", "create", "destroy"]).
    fn supported_actions(&self) -> Vec<&str>;

    /// Required permission for a given action.
    /// Returns (resource, PermissionOp) pairs the caller must hold.
    fn required_permissions(&self, action: &str) -> Vec<(String, PermissionOp)>;

    /// Execute a capability action.
    ///
    /// The kernel has already validated permissions and fired ToolPre hooks
    /// before calling this method.
    async fn execute(
        &self,
        action: &str,
        params: Value,
        context: &CapabilityContext,
    ) -> Result<CapabilityResult, AgentOSError>;

    /// Human-readable description of this provider for agent manuals.
    fn description(&self) -> &str;
}

/// Context passed to every capability provider execution.
pub struct CapabilityContext {
    pub agent_id: AgentID,
    pub task_id: TaskID,
    pub trace_id: TraceID,
    pub data_dir: std::path::PathBuf,
    pub permissions: PermissionSet,
    pub workspace_paths: Vec<std::path::PathBuf>,
}

/// Structured result from a capability provider.
pub struct CapabilityResult {
    /// Whether the action succeeded.
    pub success: bool,
    /// Structured JSON output (provider-specific schema).
    pub output: Value,
    /// Audit metadata — merged into the audit event.
    pub audit_metadata: Value,
}
```

### 2. Build the `CapabilityRegistry`

**File:** `crates/agentos-kernel/src/capability_registry.rs` (new)

```rust
use super::capability_provider::CapabilityProvider;
use std::collections::HashMap;
use std::sync::Arc;

/// Registry of all capability providers, keyed by domain name.
pub struct CapabilityRegistry {
    providers: HashMap<String, Arc<dyn CapabilityProvider>>,
}

impl CapabilityRegistry {
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
        }
    }

    /// Register a capability provider for a domain (e.g., "env", "proc").
    /// Returns error if domain already registered.
    pub fn register(
        &mut self,
        provider: Arc<dyn CapabilityProvider>,
    ) -> Result<(), AgentOSError> {
        let domain = provider.domain().to_string();
        if self.providers.contains_key(&domain) {
            return Err(AgentOSError::ConfigError(
                format!("Capability domain '{}' already registered", domain),
            ));
        }
        self.providers.insert(domain, provider);
        Ok(())
    }

    /// Look up a provider by domain name.
    pub fn get(&self, domain: &str) -> Option<&Arc<dyn CapabilityProvider>> {
        self.providers.get(domain)
    }

    /// List all registered domains.
    pub fn domains(&self) -> Vec<&str> {
        self.providers.keys().map(|s| s.as_str()).collect()
    }
}
```

### 3. Add new audit event types

**File:** `crates/agentos-audit/src/log.rs`

Add these variants to the `AuditEventType` enum:

```rust
// Kernel-Mediated Capabilities (KMC)
CapabilityRequested,
CapabilityGranted,
CapabilityDenied,
CapabilityExecuted,
CapabilityFailed,

// KMC domain-specific events
PackageInstalled,
PackageRemoved,
EnvironmentCreated,
EnvironmentDestroyed,
ProcessSpawned,
ProcessSignaled,
ProcessTerminated,
NetworkRequestExecuted,
NetworkDestinationBlocked,
StorageZoneCreated,
StorageZoneRevoked,
BuildExecuted,
BuildFailed,
```

### 4. Wire registry into kernel state

**File:** `crates/agentos-kernel/src/kernel.rs`

- Add `capability_registry: Arc<RwLock<CapabilityRegistry>>` to kernel state struct
- Initialize registry during kernel boot (after HAL, before tool registry)
- Pass `Arc` reference into `ToolExecutionContext` as new field

**File:** `crates/agentos-tools/src/traits.rs`

- Add `pub capability_registry: Option<Arc<dyn CapabilityRegistryQuery>>` to `ToolExecutionContext`
- Define `CapabilityRegistryQuery` trait for cross-crate access (avoids circular dependency)

### 5. Create bridge tool: `capability-request`

**File:** `crates/agentos-tools/src/capability_request.rs` (new)

This is the tool that agents call to invoke managed capabilities. It bridges the `AgentTool` interface to the `CapabilityProvider` interface:

```rust
/// Tool that agents use to invoke managed capabilities.
///
/// Usage: { "domain": "env", "action": "install", "params": { "package": "flask" } }
///
/// The tool looks up the capability provider in the registry,
/// validates permissions, and delegates execution.
pub struct CapabilityRequestTool;

impl AgentTool for CapabilityRequestTool {
    fn name(&self) -> &str { "capability-request" }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        // Base permission; domain-specific permissions checked at provider level
        vec![("capability.request".to_string(), PermissionOp::Execute)]
    }

    async fn execute(
        &self,
        payload: Value,
        context: ToolExecutionContext,
    ) -> Result<Value, AgentOSError> {
        let domain = payload["domain"].as_str()
            .ok_or_else(|| AgentOSError::ValidationError("missing 'domain'".into()))?;
        let action = payload["action"].as_str()
            .ok_or_else(|| AgentOSError::ValidationError("missing 'action'".into()))?;
        let params = payload.get("params").cloned().unwrap_or(Value::Object(Default::default()));

        let registry = context.capability_registry.as_ref()
            .ok_or_else(|| AgentOSError::InternalError("capability registry not available".into()))?;

        let provider = registry.get(domain)
            .ok_or_else(|| AgentOSError::ValidationError(format!("unknown domain '{}'", domain)))?;

        // Check domain-specific permissions
        for (resource, op) in provider.required_permissions(action) {
            context.permissions.check(&resource, op)?;
        }

        let cap_context = CapabilityContext {
            agent_id: context.agent_id.clone(),
            task_id: context.task_id.clone(),
            trace_id: context.trace_id.clone(),
            data_dir: context.data_dir.clone(),
            permissions: context.permissions.clone(),
            workspace_paths: context.workspace_paths.clone(),
        };

        let result = provider.execute(action, params, &cap_context).await?;

        Ok(serde_json::json!({
            "success": result.success,
            "output": result.output,
        }))
    }
}
```

### 6. Add convenience per-domain tools

Rather than requiring agents to use the generic `capability-request` tool, also register domain-specific tools that delegate to it:
- `env-install`, `env-create`, `env-destroy`
- `proc-spawn`, `proc-signal`, `proc-list`
- `net-http`, `net-connect`
- `build-run`, `build-test`
- `storage-zone-create`, `storage-zone-grant`

These are thin wrappers that fill in the `domain` field and provide cleaner JSON schemas for LLM tool calling.

### 7. Write tool manifests

**Directory:** `tools/core/`

Create TOML manifests for each new tool with `trust_tier = "core"` and appropriate `risk_class`.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/capability_provider.rs` | NEW — `CapabilityProvider` trait, `CapabilityContext`, `CapabilityResult` |
| `crates/agentos-kernel/src/capability_registry.rs` | NEW — `CapabilityRegistry` struct |
| `crates/agentos-kernel/src/lib.rs` | Add `pub mod capability_provider; pub mod capability_registry;` |
| `crates/agentos-kernel/src/kernel.rs` | Add registry to kernel state, initialize on boot |
| `crates/agentos-tools/src/capability_request.rs` | NEW — Bridge tool |
| `crates/agentos-tools/src/traits.rs` | Add `capability_registry` field to `ToolExecutionContext` |
| `crates/agentos-tools/src/factory.rs` | Register `CapabilityRequestTool` |
| `crates/agentos-tools/src/lib.rs` | Add `pub mod capability_request;` |
| `crates/agentos-audit/src/log.rs` | Add ~18 new `AuditEventType` variants |
| `tools/core/capability-request.toml` | NEW — Tool manifest |

---

## Dependencies

- **Requires:** Nothing (foundational phase)
- **Blocks:** All other phases (2-8)

---

## Test Plan

- [ ] `CapabilityRegistry::register()` succeeds for new domain
- [ ] `CapabilityRegistry::register()` returns error for duplicate domain
- [ ] `CapabilityRegistry::get()` returns `None` for unknown domain
- [ ] `CapabilityRequestTool` returns `ValidationError` for missing domain/action
- [ ] `CapabilityRequestTool` checks domain-specific permissions before executing
- [ ] Mock provider receives correct `CapabilityContext` fields
- [ ] Audit events written for `CapabilityRequested` and `CapabilityExecuted`
- [ ] `CapabilityContext` carries correct `agent_id`, `task_id`, `data_dir`

---

## Verification

```bash
cargo build --workspace
cargo test -p agentos-kernel -- capability
cargo test -p agentos-tools -- capability
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```

---

## Related

- [[Kernel Mediated Capabilities Plan]]
- [[02-managed-environments]] — first provider implementation
