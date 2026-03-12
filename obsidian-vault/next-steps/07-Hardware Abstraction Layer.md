---
title: Hardware Abstraction Layer — Spec #9
tags:
  - next-steps
  - hal
  - hardware
  - spec-9
date: 2026-03-11
status: not-started
effort: 10h
priority: low
spec-ref: "Spec §9 — Hardware Abstraction Layer with Per-Agent Gating"
---

# Hardware Abstraction Layer (HAL)

> The only major spec item with **zero implementation**. Addresses hardware access control — GPU, camera, microphone, USB — currently absent from all agent frameworks.

---

## Problem

No agent framework has hardware access control. Any agent with the right skill can:
- Access the camera or microphone without user awareness
- Consume unlimited GPU VRAM and starve other agents
- Read from USB storage devices
- Connect to any peripheral

The AgentOS HAL sits between agents and physical hardware. Hardware is **denied by default**; access must be explicitly granted in the agent's Permission Matrix.

---

## Architecture Overview

```
Agent requests hardware (e.g. GPU for inference)
    ↓
HAL Gateway intercepts request
    ↓
Check: is hardware in agent's Permission Matrix? (Spec §2)
    ↓
If denied → return PermissionDenied error + audit log entry
If granted → check current availability (HardwareRegistry)
    ↓
If available → allocate, enforce per-agent caps, return handle
If in use → queue or reject based on resource policy
    ↓
On release → audit log, wake next waiter (similar to ResourceArbiter)
```

---

## What to Build

### Step 1 — `HardwareRegistry` Type

**New file:** `crates/agentos-hal/src/lib.rs` (new crate) or `crates/agentos-kernel/src/hal.rs`

> [!tip] Recommendation
> Start as `crates/agentos-kernel/src/hal.rs` — move to its own crate when it grows. Adding a new crate now adds workspace overhead with no benefit.

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceType {
    Gpu,
    Camera,
    Microphone,
    UsbStorage,
    NetworkInterface,
    Custom(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceStatus {
    Available,
    Locked { held_by: String },        // agent_id
    Quarantined,                        // new/unknown device, pending approval
    DeniedAll,                          // permanently blocked
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareDevice {
    pub id: String,                     // e.g. "gpu:0", "cam:0", "usb:1"
    pub device_type: DeviceType,
    pub description: String,
    pub status: DeviceStatus,
    pub max_memory_mb: Option<u64>,     // For GPU: VRAM cap per agent
    pub granted_to: Vec<String>,        // agent_ids with permission
}

pub struct HardwareRegistry {
    devices: RwLock<HashMap<String, HardwareDevice>>,
}

impl HardwareRegistry {
    pub fn new() -> Self;
    pub async fn register_device(&self, device: HardwareDevice);
    pub async fn request_access(&self, device_id: &str, agent_id: &str) -> Result<(), String>;
    pub async fn release_access(&self, device_id: &str, agent_id: &str);
    pub async fn quarantine_device(&self, device_id: &str);
    pub async fn list_devices(&self) -> Vec<HardwareDevice>;
    pub async fn grant_to_agent(&self, device_id: &str, agent_id: &str) -> Result<(), String>;
    pub async fn revoke_from_agent(&self, device_id: &str, agent_id: &str);
}
```

### Step 2 — GPU Slice Manager

For GPU specifically, add per-agent VRAM caps and time slicing:

```rust
pub struct GpuSliceManager {
    total_vram_mb: u64,
    allocations: RwLock<HashMap<String, u64>>,  // agent_id → MB allocated
    max_per_agent_mb: u64,
}

impl GpuSliceManager {
    pub async fn allocate(&self, agent_id: &str, requested_mb: u64) -> Result<u64, String>;
    pub async fn release(&self, agent_id: &str);
    pub async fn usage_report(&self) -> Vec<(String, u64)>;  // (agent_id, mb_used)
}
```

### Step 3 — New Device Approval Flow

When a new USB or hardware device is detected (kernel hook into OS events — `udev` on Linux):

1. Kernel registers device with `DeviceStatus::Quarantined`
2. Writes an audit entry: `AuditEventType::HardwareDetected`
3. Sends notification to user via configured escalation channel
4. User approves → status changes to `Available`, optionally granted to specific agents
5. User denies → status changes to `DeniedAll`

```rust
// In kernel boot / health loop:
pub async fn sweep_new_hardware(&self) {
    // On Linux: watch /sys/bus for new device events (inotify or udev socket)
    // On macOS: IOKit notifications
    // Stub implementation: poll /sys/bus/usb/devices/ periodically
}
```

### Step 4 — Wire HAL into Kernel

**File:** `crates/agentos-kernel/src/kernel.rs`

```rust
pub struct Kernel {
    // ... existing fields ...
    pub hardware_registry: Arc<crate::hal::HardwareRegistry>,
    pub gpu_slice_manager: Arc<crate::hal::GpuSliceManager>,
}
```

### Step 5 — Permission Matrix Integration

**File:** `crates/agentos-types/src/agent.rs`

The `AgentPermission` struct needs hardware fields:

```rust
pub struct AgentPermission {
    // ... existing fields ...
    pub hardware: HardwarePermission,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HardwarePermission {
    pub gpu: bool,
    pub camera: bool,
    pub microphone: bool,
    pub usb: bool,
    pub max_gpu_memory_mb: Option<u64>,
}
```

### Step 6 — CLI Commands

```bash
agentctl hal list                           # list all hardware devices + status
agentctl hal grant gpu:0 --agent <id>       # grant GPU access to agent
agentctl hal revoke cam:0 --agent <id>      # revoke camera access
agentctl hal approve usb:1                  # approve quarantined device
agentctl hal deny usb:1                     # permanently block device
agentctl hal gpu stats                      # live VRAM usage by agent
```

---

## Out of Scope (For Now)

| Feature | Reason to Defer |
|---|---|
| GPU time-slicing (preemptive) | Requires CUDA/ROCm driver integration — high complexity |
| Microphone/camera stream isolation | OS-level capability (seccomp/sandbox) — separate effort |
| Cross-node hardware inventory | Requires distributed coordination — not yet needed |
| inotify/udev event loop | Platform-specific; use polling as initial implementation |

---

## Testing Plan

| Test | Verifies |
|---|---|
| `test_agent_without_gpu_perm_rejected` | HAL denies access when not in Permission Matrix |
| `test_agent_with_gpu_perm_succeeds` | HAL grants access when permitted |
| `test_quarantined_device_blocks_all` | No agent can access quarantined device |
| `test_gpu_vram_cap_enforced` | Agent cannot exceed `max_gpu_memory_mb` |
| `test_release_wakes_waiter` | Second agent gets GPU after first releases |

---

## Files Changed

| File | Change |
|---|---|
| `crates/agentos-kernel/src/hal.rs` | **NEW** — `HardwareRegistry` + `GpuSliceManager` |
| `crates/agentos-kernel/src/lib.rs` | Add `pub mod hal;` |
| `crates/agentos-kernel/src/kernel.rs` | Add `hardware_registry` + `gpu_slice_manager` to struct + boot |
| `crates/agentos-types/src/agent.rs` | Add `hardware: HardwarePermission` to `AgentPermission` |
| `crates/agentos-bus/src/message.rs` | Add HAL-related `KernelCommand` variants |
| `crates/agentos-cli/src/commands/hal.rs` | **NEW** — CLI handler |
| `crates/agentos-cli/src/main.rs` | Add `Hal` variant to `Commands` |

---

## Related

- [[Index]] — Back to dashboard
- [[reference/HAL System]] — Existing HAL documentation (spec-level)
- [[02-Ed25519 Tool Signing]] — Tool manifest specifies hardware requirements; HAL enforces them
