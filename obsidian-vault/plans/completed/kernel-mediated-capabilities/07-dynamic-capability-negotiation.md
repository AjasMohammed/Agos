---
title: "Phase 7: Dynamic Capability Negotiation"
tags:
  - kernel
  - capabilities
  - security
  - v4
  - phase-7
date: 2026-04-12
status: planned
effort: 2d
priority: high
---

# Phase 7: Dynamic Capability Negotiation

> Allow agents to request capabilities they don't currently hold at runtime — with the kernel checking policy, firing approval hooks, and granting scoped, time-limited tokens.

---

## Why This Phase

Currently, capability tokens are **static** — granted at task start, fixed for the entire task duration. This forces operators to either:
- **Over-provision:** Grant broad permissions upfront "just in case" (weakens security)
- **Under-provision:** Grant minimal permissions and agents fail when they need something unexpected (weakens capability)

Dynamic negotiation solves both problems. When an agent encounters a missing capability, it can request it. The kernel checks policy and either:
1. **Auto-grants** if policy allows (e.g., installing a package from the curated allowlist)
2. **Escalates** if policy requires approval (e.g., accessing a new network destination)
3. **Denies** if policy forbids (e.g., accessing `/etc/shadow`)

The grant is scoped (specific resource), time-limited (TTL), and audited. It uses the **existing `PendingEscalation` system** — no new approval UX needed.

**Research backing:** WASI's capability model grants specific handles for specific resources. Android's runtime permissions request access when needed, not at install time. This is the same principle applied to AI agents.

---

## Current State

- `CapabilityToken` has fixed `permissions: PermissionSet` set at task start (`capability.rs:13`)
- `PendingEscalation` system with auto-deny on expiry exists (`escalation.rs:27`)
- `ApprovalHook` fires on `ToolPre` and can abort tool execution
- `EscalationManager` supports create, resolve, sweep operations
- No mechanism for agents to request new permissions mid-task

## Target State

- `CapabilityBroker` component in kernel handles runtime capability requests
- Agents can call a `request-capability` tool to ask for permissions
- Broker checks policy → auto-grant, escalate, or deny
- Granted capabilities are ephemeral (scoped resource, TTL, auto-revoke)
- Ephemeral grants stored in a `CapabilitySession` per agent
- Existing escalation system used for human-in-the-loop approval

---

## Detailed Subtasks

### 1. Define ephemeral capability grant model

**File:** `crates/agentos-kernel/src/capability_broker.rs` (new)

```rust
use agentos_types::*;
use serde::{Deserialize, Serialize};

/// An ephemeral capability grant — scoped, time-limited, revocable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EphemeralGrant {
    pub grant_id: String,
    pub agent_id: AgentID,
    pub task_id: TaskID,
    /// The capability domain (e.g., "env", "net", "storage")
    pub domain: String,
    /// The specific action (e.g., "install", "http")
    pub action: String,
    /// The specific resource (e.g., package name, URL, path)
    pub resource: String,
    /// Permission entries added by this grant
    pub permissions: Vec<PermissionEntry>,
    pub granted_at: chrono::DateTime<chrono::Utc>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub grant_source: GrantSource,
    pub revoked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GrantSource {
    /// Auto-granted by policy (resource matched allowlist).
    Policy { rule: String },
    /// Granted by operator approval via escalation.
    OperatorApproval { escalation_id: u64 },
}
```

### 2. Implement `CapabilityBroker`

**File:** `crates/agentos-kernel/src/capability_broker.rs`

```rust
pub struct CapabilityBroker {
    /// Active ephemeral grants per agent
    sessions: Arc<RwLock<HashMap<AgentID, Vec<EphemeralGrant>>>>,
    /// Policy configuration loaded from config
    policy: CapabilityPolicy,
    /// Reference to escalation manager for human-in-the-loop
    escalation_manager: Arc<RwLock<EscalationManager>>,
}

impl CapabilityBroker {
    /// Request a capability. Returns:
    /// - Ok(EphemeralGrant) if auto-granted by policy
    /// - Err(CapabilityPending { escalation_id }) if waiting for approval
    /// - Err(CapabilityDenied { reason }) if policy denies
    pub async fn request_capability(
        &self,
        agent_id: &AgentID,
        task_id: &TaskID,
        domain: &str,
        action: &str,
        resource: &str,
    ) -> Result<EphemeralGrant, AgentOSError>;

    /// Check if an agent has an active grant for a capability.
    /// Called by capability providers before execution.
    pub fn has_grant(
        &self,
        agent_id: &AgentID,
        domain: &str,
        action: &str,
        resource: &str,
    ) -> bool;

    /// Called when an escalation is resolved (approved/denied).
    /// If approved, mints the ephemeral grant.
    pub async fn on_escalation_resolved(
        &self,
        escalation_id: u64,
        approved: bool,
    ) -> Result<(), AgentOSError>;

    /// Sweep expired grants. Called by TimeoutChecker.
    pub fn sweep_expired(&self) -> usize;

    /// Revoke all grants for an agent (on disconnect/task end).
    pub fn revoke_all(&self, agent_id: &AgentID) -> usize;
}
```

### 3. Policy evaluation logic

The broker checks requests against a three-tier policy:

```
1. DENY LIST (absolute, never granted)
   e.g., storage zone for /etc/**, package "malicious-pkg"
   → Return CapabilityDenied immediately

2. ALLOW LIST (auto-grant, no human needed)
   e.g., Python packages in curated list, *.github.com destinations
   → Mint ephemeral grant with TTL, return Ok

3. ESCALATION (human review required)
   e.g., unknown package, private network destination
   → Create PendingEscalation, return CapabilityPending
```

Policy is loaded from the per-domain config sections already defined in Phases 2-5:
- `[capabilities.env]` → package allowlists
- `[capabilities.net]` → destination allowlists
- `[capabilities.storage]` → path allowlist patterns
- `[capabilities.proc]` → binary allowlists

### 4. Integration with escalation system

When a capability request requires human approval:

```rust
// In capability_broker.rs
let escalation = PendingEscalation {
    task_id: task_id.clone(),
    agent_id: agent_id.clone(),
    reason: EscalationReason::CapabilityRequest,
    context_summary: format!(
        "Agent requests {}.{} access to '{}'",
        domain, action, resource
    ),
    decision_point: format!("Grant {} capability?", domain),
    options: vec!["Approve".into(), "Deny".into(), "Approve for session".into()],
    urgency: "medium".into(),
    blocking: true,
    auto_action: AutoAction::Deny,  // default-deny on timeout
    metadata: serde_json::json!({
        "type": "capability_request",
        "domain": domain,
        "action": action,
        "resource": resource,
    }),
    // ... timestamps, etc.
};
```

Add `CapabilityRequest` variant to `EscalationReason` enum.

### 5. Integration with capability providers

Modify each provider (from Phases 2-5) to check the broker before executing:

```rust
// In any provider's execute() method:
async fn execute(&self, action: &str, params: Value, ctx: &CapabilityContext) -> Result<...> {
    let resource = extract_resource(action, &params);

    // First check static permissions
    if !ctx.permissions.check(&format!("{}.{}", self.domain(), action), PermissionOp::Execute).is_ok() {
        // Try dynamic grant
        let broker = ctx.capability_broker.as_ref()
            .ok_or(AgentOSError::PermissionDenied(...))?;

        if !broker.has_grant(&ctx.agent_id, self.domain(), action, &resource) {
            // Request capability
            broker.request_capability(
                &ctx.agent_id, &ctx.task_id,
                self.domain(), action, &resource,
            ).await?;
        }
    }

    // Proceed with execution...
}
```

### 6. Wire into kernel lifecycle

- `TimeoutChecker`: sweep expired grants alongside escalation sweep (every 10min)
- Task completion: revoke all grants for completed task
- Agent disconnect: revoke all grants for disconnected agent
- Kernel shutdown: revoke all grants

### 7. `request-capability` tool

**File:** `crates/agentos-tools/src/capability_request_tool.rs`

Agents can also proactively request capabilities before they need them:

```json
{
  "domain": "net",
  "action": "http",
  "resource": "internal-api.company.com:8080",
  "reason": "Need to fetch deployment status from internal API"
}
```

The tool calls the broker directly. If auto-granted, returns confirmation. If escalated, returns "pending approval" and the agent can poll or proceed with other work.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/capability_broker.rs` | NEW — `CapabilityBroker`, `EphemeralGrant`, policy evaluation |
| `crates/agentos-kernel/src/lib.rs` | Add `pub mod capability_broker;` |
| `crates/agentos-kernel/src/kernel.rs` | Initialize broker, wire to lifecycle |
| `crates/agentos-kernel/src/kernel_action.rs` | Add `CapabilityRequest` to `EscalationReason` |
| `crates/agentos-kernel/src/escalation.rs` | Add callback hook for `on_escalation_resolved` |
| `crates/agentos-kernel/src/capability_provider.rs` | Add `capability_broker` field to `CapabilityContext` |
| `crates/agentos-tools/src/capability_request_tool.rs` | NEW — proactive request tool |
| `crates/agentos-tools/src/factory.rs` | Register request tool |
| `tools/core/request-capability.toml` | NEW — manifest |

---

## Dependencies

- **Requires:** Phase 1 (trait and registry)
- **Blocks:** Phase 8 (policy engine builds on broker)

---

## Test Plan

- [ ] Auto-grant: requesting allowed package returns `EphemeralGrant` immediately
- [ ] Auto-deny: requesting denied resource returns `CapabilityDenied` immediately
- [ ] Escalation: requesting unknown resource creates `PendingEscalation`
- [ ] Escalation approval mints ephemeral grant
- [ ] Escalation denial returns `CapabilityDenied` to agent
- [ ] Escalation timeout (5 min) auto-denies via existing `sweep_expired`
- [ ] Grant TTL enforced: expired grants rejected by `has_grant()`
- [ ] `sweep_expired()` cleans up expired grants
- [ ] Task completion revokes all task-specific grants
- [ ] Agent disconnect revokes all agent grants
- [ ] Providers check broker when static permissions insufficient
- [ ] `request-capability` tool returns structured status
- [ ] Audit events: `CapabilityRequested`, `CapabilityGranted`, `CapabilityDenied`
- [ ] "Approve for session" option grants for remaining session, not just once

---

## Verification

```bash
cargo build --workspace
cargo test -p agentos-kernel -- capability_broker
cargo test -p agentos-tools -- capability_request
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```

---

## Related

- [[01-capability-provider-trait]] — prerequisite
- [[08-policy-engine-operator-controls]] — builds on broker for operator UX
- [[Kernel Mediated Capabilities Plan]]
