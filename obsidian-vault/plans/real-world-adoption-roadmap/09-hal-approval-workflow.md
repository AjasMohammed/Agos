---
title: HAL Device Approval Workflow
tags:
  - hal
  - hardware
  - security
  - kernel
  - plan
  - v3
date: 2026-03-25
status: completed
effort: 2d
priority: low
---

# Phase 9 — HAL Device Approval Workflow

> Wire the HAL (Hardware Abstraction Layer) per-device quarantine and approval workflow so that agents cannot access hardware devices (GPU, sensors, storage, network interfaces) without explicit user approval. This directly unlocks IoT, robotics, and industrial automation use cases.

---

## Why This Phase

AgentOS has a unique competitive advantage: a built-in Hardware Abstraction Layer. No other agent framework has this. The HAL already registers devices via `HardwareRegistry` and tracks `DeviceStatus` — but the access gate is not wired. Agents can currently call HAL tools without any device-level approval check.

The real-world use cases this unlocks:
- **Robotics** — agents that control actuators need human approval before first access
- **Industrial automation** — compliance mandates that hardware access is audited and approved
- **IoT edge deployment** — device access approval prevents rogue agents from accessing sensors
- **GPU compute** — expensive GPU tasks need approval when cost exceeds a threshold

---

## Current → Target State

| Area | Current | Target |
|------|---------|--------|
| Device registration | `HardwareRegistry` stores `DeviceProfile` | Unchanged |
| Device access | HAL tools execute without device-level check | Tools check device status before executing |
| Device status | `DeviceStatus` enum exists (Pending/Approved/Quarantined) | Status enforced: Pending → blocks access, Quarantined → hard-denies |
| Approval workflow | `agentctl hal approve` CLI exists but not wired | Approval changes status to Approved; access unblocked |
| Escalation | None | New device access creates PendingEscalation (reuses existing escalation machinery) |
| Audit | None for device access | `DeviceAccessGranted`, `DeviceAccessDenied` audit events |

---

## Architecture

```
Agent calls HAL tool (e.g., hardware-info, gpu-exec)
     │
     ▼
HAL Tool Executor
     │
     ├── Check device_registry.get_status(device_id)
     │      ├── Approved → proceed normally
     │      ├── Pending → create escalation, return "awaiting approval" error
     │      └── Quarantined → return hard-deny error (AgentOSError::DeviceQuarantined)
     │
     ▼ (if Approved)
Execute HAL operation
     │
     ▼
Audit: DeviceAccessGranted(agent_id, device_id, operation)
```

---

## Detailed Subtasks

### Subtask 9.1 — DeviceAccessGate in HAL executor

**File:** `crates/agentos-hal/src/hal.rs`

Add a `DeviceAccessGate` check to the HAL execution path. Every HAL tool call that targets a specific device must pass through this gate:

```rust
pub struct DeviceAccessGate {
    registry: Arc<HardwareRegistry>,
    escalation_manager: Arc<EscalationManager>,
    audit: Arc<AuditLog>,
}

impl DeviceAccessGate {
    pub async fn check(
        &self,
        agent_id: &AgentID,
        task_id: &TaskID,
        device_id: &str,
        operation: HalOperation,
    ) -> Result<()> {
        let status = self.registry.get_device_status(device_id).await?;

        match status {
            DeviceStatus::Approved => {
                // Log access
                self.audit.log(AuditEvent::DeviceAccessGranted {
                    agent_id: agent_id.clone(),
                    device_id: device_id.to_string(),
                    operation: operation.to_string(),
                }).await?;
                Ok(())
            }

            DeviceStatus::Quarantined => {
                self.audit.log(AuditEvent::DeviceAccessDenied {
                    agent_id: agent_id.clone(),
                    device_id: device_id.to_string(),
                    reason: "device quarantined".to_string(),
                }).await?;
                Err(AgentOSError::DeviceQuarantined(device_id.to_string()))
            }

            DeviceStatus::Pending => {
                // Create escalation for user to approve/deny
                let escalation_id = self.escalation_manager.create_escalation(
                    EscalationRequest {
                        agent_id: agent_id.clone(),
                        task_id: task_id.clone(),
                        reason: format!(
                            "Agent '{}' requests access to device '{}' for operation '{}'",
                            agent_id, device_id, operation
                        ),
                        kind: EscalationKind::DeviceAccessRequest,
                        metadata: json!({
                            "device_id": device_id,
                            "operation": operation.to_string(),
                        }),
                    }
                ).await?;

                Err(AgentOSError::DeviceAccessPending {
                    device_id: device_id.to_string(),
                    escalation_id,
                })
            }
        }
    }
}
```

---

### Subtask 9.2 — Wire DeviceAccessGate into HAL driver calls

**File:** `crates/agentos-hal/src/hal.rs`

Each driver's `execute()` method receives a `ToolContext` that now includes `DeviceAccessGate`. Call the gate before any device-specific operation:

```rust
// In GpuDriver::execute():
ctx.device_gate.check(&ctx.agent_id, &ctx.task_id, &device_id, HalOperation::Read).await?;
// ... proceed with GPU query

// In SensorDriver::execute():
ctx.device_gate.check(&ctx.agent_id, &ctx.task_id, &sensor_id, HalOperation::Read).await?;
// ... proceed with sensor read

// In StorageDriver::execute():
ctx.device_gate.check(&ctx.agent_id, &ctx.task_id, &mount_point, HalOperation::Read).await?;
```

For system-wide queries (e.g., `hardware-info` listing all devices), skip device gate (listing is always allowed; access to a specific device requires approval).

---

### Subtask 9.3 — `agentctl hal approve/deny` wired to registry

**File:** `crates/agentos-cli/src/commands/hal.rs`

The `hal approve` and `hal deny` CLI commands already exist in the CLI module but call unimplemented kernel handlers. Wire them:

**File:** `crates/agentos-kernel/src/commands/agent.rs` (or new `hal.rs` command handler)

```rust
KernelCommand::HalApproveDevice { device_id, agent_id } => {
    ctx.hardware_registry.set_device_status(&device_id, DeviceStatus::Approved).await?;

    // Find any pending escalation for this device+agent and auto-approve it
    if let Some(agent_id) = agent_id {
        ctx.escalation_manager.auto_resolve_device_escalation(&device_id, &agent_id, true).await?;
    }

    ctx.audit.log(AuditEvent::DeviceApproved { device_id, approved_by: "operator".to_string() }).await?;
    respond(KernelResponse::Ok)
}

KernelCommand::HalDenyDevice { device_id, reason } => {
    ctx.hardware_registry.set_device_status(&device_id, DeviceStatus::Quarantined).await?;
    ctx.audit.log(AuditEvent::DeviceQuarantined { device_id, reason }).await?;
    respond(KernelResponse::Ok)
}
```

---

### Subtask 9.4 — Auto-register discovered devices as Pending

**File:** `crates/agentos-hal/src/hal.rs`

On kernel boot, the HAL system driver discovers hardware devices (CPU, GPU, network interfaces, mounted storage). Currently these are just returned as data. Now auto-register them in `HardwareRegistry` with status `Pending` if not already registered:

```rust
pub async fn register_discovered_devices(
    registry: &HardwareRegistry,
    hal: &HalSystem,
) -> Result<()> {
    // Discover devices
    let devices = hal.list_devices().await?;
    for device in devices {
        if registry.get_device_status(&device.id).await?.is_none() {
            registry.register_device(DeviceProfile {
                id: device.id,
                kind: device.kind,
                name: device.name,
                status: DeviceStatus::Pending,   // requires approval on first access
                registered_at: Utc::now(),
            }).await?;
        }
    }
    Ok(())
}
```

**Exception:** Core system device (CPU, RAM) are auto-approved on boot. Only GPU, external storage, sensors, and network devices require approval.

---

### Subtask 9.5 — New error types

**File:** `crates/agentos-types/src/lib.rs` (or error.rs)

```rust
// Add to AgentOSError:
#[error("Device '{0}' is quarantined and access is permanently denied")]
DeviceQuarantined(String),

#[error("Device '{0}' access is pending approval (escalation: {1})")]
DeviceAccessPending { device_id: String, escalation_id: String },
```

---

### Subtask 9.6 — New audit events

**File:** `crates/agentos-audit/src/log.rs`

Add to `AuditEventType`:
```rust
DeviceAccessGranted,    // agent accessed an approved device
DeviceAccessDenied,     // agent denied (quarantined or escalation timeout)
DeviceApproved,         // operator approved a pending device
DeviceQuarantined,      // operator or auto-rule quarantined a device
DeviceAccessEscalated,  // access request created a pending escalation
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-hal/src/hal.rs` | Modified — add DeviceAccessGate check in driver dispatch |
| `crates/agentos-hal/src/drivers/gpu.rs` | Modified — call gate before GPU ops |
| `crates/agentos-hal/src/drivers/sensor.rs` | Modified — call gate before sensor reads |
| `crates/agentos-hal/src/drivers/storage.rs` | Modified — call gate before storage ops |
| `crates/agentos-kernel/src/commands/` | Modified — wire hal approve/deny handlers |
| `crates/agentos-types/src/lib.rs` | Modified — add DeviceQuarantined, DeviceAccessPending errors |
| `crates/agentos-audit/src/log.rs` | Modified — add 5 new device audit events |

---

## Dependencies

- No other phases required
- Requires escalation manager (already complete)
- Requires HardwareRegistry (already exists)

---

## Test Plan

1. **Approved device access** — register device as Approved, call HAL tool, assert success + audit event
2. **Quarantined device** — register device as Quarantined, call HAL tool, assert `DeviceQuarantined` error
3. **Pending creates escalation** — register device as Pending (new), call HAL tool, assert `DeviceAccessPending` error + escalation created
4. **Approve unblocks access** — device Pending, call tool (creates escalation), `agentctl hal approve <device>`, retry tool call, assert success
5. **Auto-register on boot** — boot kernel, call `agentctl hal list`, assert GPU and sensor devices appear with status `pending`

---

## Verification

```bash
cargo build -p agentos-hal -p agentos-kernel
cargo test -p agentos-hal -- device_access

# Manual test
agentctl hal list                          # shows discovered devices with status
agentctl task run --agent myagent "Report GPU temperature"
# → should fail with "Device access pending approval"
agentctl hal approve gpu-0                 # approve GPU
agentctl task run --agent myagent "Report GPU temperature"
# → should succeed
```

## Completion

Implemented on 2026-03-28.

The HAL now enforces per-device approval through a kernel-backed access gate, creates and resolves device access escalations, writes device access audit events, and auto-registers discovered hardware with boot-time defaults.

Core system devices stay globally approved, removable storage is approval-gated, and approved devices now support agent-specific deny handling without revoking access that was already granted to other agents.

---

## Related

- [[Real World Adoption Roadmap Plan]]
- [[02-web-ui-completion]] — device approval requests visible in notification inbox
