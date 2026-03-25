---
title: HAL Device Registry Enforcement
tags:
  - hal
  - security
  - kernel
  - plan
  - phase-1
date: 2026-03-24
status: planned
effort: 1.5d
priority: high
---

# Phase 1 — HAL Device Registry Enforcement

> Wire `HardwareRegistry::check_access()` into `HardwareAbstractionLayer::query()` so device quarantine is enforced at runtime, not just tracked in a registry no one consults.

---

## Why This Phase

`HardwareAbstractionLayer` has GPU, storage, sensor, and network drivers that access real hardware. `HardwareRegistry` tracks device approval status (Quarantined / Approved / Denied) and has a working `check_access()` method with tests. The `HalApproveDevice`/`HalDenyDevice` CLI + bus + kernel handlers all work. But nothing connects `HardwareAbstractionLayer::query()` to `HardwareRegistry`.

**The quarantine system is write-only.** Operators can approve or deny devices, but those decisions are never consulted when a tool actually accesses hardware. Any agent with `hardware.gpu` permission can read GPU info regardless of quarantine status.

---

## Current → Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `HardwareAbstractionLayer` fields | `drivers: HashMap<String, Box<dyn HalDriver>>` | + `registry: Option<Arc<HardwareRegistry>>` |
| `query()` signature | `(driver_name, params, permission_check: &PermissionSet)` | + `agent_id: Option<&AgentID>` |
| Device discovery | Never quarantines new devices | Auto-quarantines on first `query()` for device-mapped drivers |
| Access enforcement | Only checks `PermissionSet` | Also calls `registry.check_access(device_key, agent_id)` |
| Drivers with device keys | None | `gpu`, `storage`, `sensor` return a device key; `system`, `process`, `network`, `log_reader` return `None` |
| `HardwareInfoTool.execute()` | Passes no agent_id | Passes `ctx.agent_id` |

---

## Detailed Subtasks

### Subtask 1.1 — Add `device_key()` to `HalDriver` trait

**File:** `crates/agentos-hal/src/hal.rs`

Add an optional method to the `HalDriver` trait that returns the device ID for a given call. Drivers that don't map to physical devices return `None`.

```rust
pub trait HalDriver: Send + Sync {
    fn name(&self) -> &str;
    fn required_permission(&self) -> (&str, PermissionOp);
    async fn query(&self, params: Value) -> Result<Value, AgentOSError>;

    /// Returns the device registry key for this call (e.g. "gpu:0", "storage:/dev/sda").
    /// Return `None` for non-device drivers (system, process, network, log_reader).
    /// Used by `HardwareAbstractionLayer` to call `check_access()` before dispatch.
    fn device_key(&self, _params: &Value) -> Option<String> {
        None
    }
}
```

This is a non-breaking default — existing driver impls don't need to change unless they map to a physical device.

---

### Subtask 1.2 — Implement `device_key()` in device-mapped drivers

**File:** `crates/agentos-hal/src/drivers/gpu.rs`

```rust
fn device_key(&self, params: &Value) -> Option<String> {
    // If caller specifies a card index, use it; otherwise default to "gpu:0"
    let idx = params.get("card_index").and_then(|v| v.as_u64()).unwrap_or(0);
    Some(format!("gpu:{}", idx))
}
```

**File:** `crates/agentos-hal/src/drivers/storage.rs`

```rust
fn device_key(&self, params: &Value) -> Option<String> {
    // Key by path if specified, otherwise "storage:default"
    params.get("path")
        .and_then(|v| v.as_str())
        .map(|p| format!("storage:{}", p))
        .or_else(|| Some("storage:default".to_string()))
}
```

**File:** `crates/agentos-hal/src/drivers/sensor.rs`

```rust
fn device_key(&self, params: &Value) -> Option<String> {
    params.get("sensor_id")
        .and_then(|v| v.as_str())
        .map(|id| format!("sensor:{}", id))
        .or_else(|| Some("sensor:default".to_string()))
}
```

`system`, `process`, `network`, and `log_reader` drivers keep the default `None` — no device-level quarantine needed for them.

---

### Subtask 1.3 — Add `registry` field and `with_registry()` to `HardwareAbstractionLayer`

**File:** `crates/agentos-hal/src/hal.rs`

```rust
use crate::registry::HardwareRegistry;
use agentos_types::AgentID;
use std::sync::Arc;

pub struct HardwareAbstractionLayer {
    drivers: HashMap<String, Box<dyn HalDriver>>,
    /// Optional device registry for quarantine enforcement.
    /// When `None`, device-level quarantine checks are skipped (useful in tests).
    registry: Option<Arc<HardwareRegistry>>,
}

impl HardwareAbstractionLayer {
    pub fn new() -> Self {
        Self { drivers: HashMap::new(), registry: None }
    }

    pub fn new_with_defaults() -> Self {
        let mut hal = Self::new();
        hal.register(Box::new(crate::drivers::system::SystemDriver::new()));
        hal.register(Box::new(crate::drivers::process::ProcessDriver::new()));
        hal.register(Box::new(crate::drivers::network::NetworkDriver::new()));
        hal
    }

    /// Attach a `HardwareRegistry` for device-level quarantine enforcement.
    /// Call this during kernel boot after constructing the HAL.
    pub fn with_registry(mut self, registry: Arc<HardwareRegistry>) -> Self {
        self.registry = Some(registry);
        self
    }

    pub fn register(&mut self, driver: Box<dyn HalDriver>) {
        self.drivers.insert(driver.name().to_string(), driver);
    }

    pub async fn query(
        &self,
        driver_name: &str,
        params: Value,
        permission_check: &PermissionSet,
        agent_id: Option<&AgentID>,        // NEW
    ) -> Result<Value, AgentOSError> {
        let driver = self.drivers.get(driver_name)
            .ok_or_else(|| AgentOSError::HalError(format!("Driver '{}' not found", driver_name)))?;

        // --- existing permission check (unchanged) ---
        // [existing permission check code here]

        // --- NEW: device quarantine enforcement ---
        if let (Some(registry), Some(agent_id), Some(device_key)) =
            (&self.registry, agent_id, driver.device_key(&params))
        {
            // Auto-register unknown devices into quarantine on first contact.
            let device_type = format!("{}-device", driver_name);
            registry.quarantine_device(&device_key, &device_type); // idempotent

            // Block access if device is quarantined or denied.
            registry.check_access(&device_key, agent_id).map_err(|e| {
                AgentOSError::HalError(format!("Device access denied: {}", e))
            })?;
        }

        driver.query(params).await
    }
}
```

---

### Subtask 1.4 — Update `query()` callsites

**File:** `crates/agentos-tools/src/hardware_info.rs`

Change the `hal.query()` call to pass the agent's ID:

```rust
// Before:
hal.query("system", serde_json::json!({}), &perms).await

// After:
hal.query("system", serde_json::json!({}), &perms, Some(&context.agent_id)).await
```

**File:** Any other callsite discovered via `cargo build` compile errors after the signature change.

---

### Subtask 1.5 — Wire registry into HAL during kernel boot

**File:** `crates/agentos-kernel/src/kernel.rs`

Find where `HardwareAbstractionLayer::new_with_defaults()` is called and attach the registry:

```rust
// Before (approximate):
let hal = HardwareAbstractionLayer::new_with_defaults();

// After:
let hal = HardwareAbstractionLayer::new_with_defaults()
    .with_registry(Arc::clone(&hardware_registry));
```

Search for `HardwareAbstractionLayer` in `kernel.rs` to find the exact location.

---

### Subtask 1.6 — Add audit events for HAL device enforcement

**File:** `crates/agentos-hal/src/hal.rs`

The `HardwareAbstractionLayer` doesn't have access to the audit log. Instead, return a richer error that the kernel's tool execution path can log, or add a callback hook. The simplest approach: the error return from `check_access()` already produces an `AgentOSError::PermissionDenied` which propagates up to the tool executor's audit path. No additional logging needed in the HAL itself.

However, for auto-quarantine (new device first contact), add a note in the `quarantine_device()` return value doc: callers should log `AuditEventType::HalDeviceQuarantined` when `quarantine_device()` returns `true`.

If the kernel's HAL query path wants richer logging, it can inspect the `quarantine_device()` return from the registry and emit an audit entry. This is optional for Phase 1.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-hal/src/hal.rs` | Add `device_key()` to trait; add `registry` field; update `query()` signature; add quarantine enforcement block |
| `crates/agentos-hal/src/drivers/gpu.rs` | Implement `device_key()` |
| `crates/agentos-hal/src/drivers/storage.rs` | Implement `device_key()` |
| `crates/agentos-hal/src/drivers/sensor.rs` | Implement `device_key()` |
| `crates/agentos-tools/src/hardware_info.rs` | Pass `agent_id` to `hal.query()` |
| `crates/agentos-kernel/src/kernel.rs` | Call `.with_registry()` when constructing HAL |

---

## Dependencies

None — this phase is self-contained. No other phase must complete first.

---

## Test Plan

### Test 1 — New device is auto-quarantined on first `query()` call

```rust
#[tokio::test]
async fn test_hal_new_device_auto_quarantined() {
    let registry = Arc::new(HardwareRegistry::new());
    let hal = HardwareAbstractionLayer::new_with_defaults()
        .with_registry(Arc::clone(&registry));

    let agent = AgentID::new();
    let perms = make_gpu_perms();

    // First call: device doesn't exist yet → should be quarantined → access denied
    let result = hal.query("gpu", json!({}), &perms, Some(&agent)).await;
    assert!(result.is_err(), "First contact with unknown device must be denied");

    // Confirm it's now in quarantine
    let quarantined = registry.list_quarantined();
    assert_eq!(quarantined.len(), 1);
    assert_eq!(quarantined[0].device_id, "gpu:0");
}
```

### Test 2 — Approved device allows access

```rust
#[tokio::test]
async fn test_hal_approved_device_allows_access() {
    let registry = Arc::new(HardwareRegistry::new());
    let hal = HardwareAbstractionLayer::new_with_defaults()
        .with_registry(Arc::clone(&registry));

    let agent = AgentID::new();
    let perms = make_gpu_perms();

    // Pre-quarantine and approve
    registry.quarantine_device("gpu:0", "test-gpu");
    registry.approve_for_agent("gpu:0", &agent);

    // Access should succeed
    let result = hal.query("gpu", json!({}), &perms, Some(&agent)).await;
    assert!(result.is_ok());
}
```

### Test 3 — Denied device blocks access

```rust
#[tokio::test]
async fn test_hal_denied_device_blocks_access() {
    let registry = Arc::new(HardwareRegistry::new());
    let hal = HardwareAbstractionLayer::new_with_defaults()
        .with_registry(Arc::clone(&registry));

    let agent = AgentID::new();
    let perms = make_gpu_perms();

    registry.quarantine_device("gpu:0", "test-gpu");
    registry.deny_device("gpu:0");

    let result = hal.query("gpu", json!({}), &perms, Some(&agent)).await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("denied") || err.contains("Device access denied"));
}
```

### Test 4 — Non-device drivers (system) bypass registry check

```rust
#[tokio::test]
async fn test_hal_system_driver_skips_registry() {
    let registry = Arc::new(HardwareRegistry::new());
    let hal = HardwareAbstractionLayer::new_with_defaults()
        .with_registry(Arc::clone(&registry));

    let agent = AgentID::new();
    let mut perms = PermissionSet::new();
    perms.grant("hardware.system".to_string(), true, false, false, None);

    // system driver returns device_key() = None → registry check skipped
    // Should succeed without any quarantine entry
    let result = hal.query("system", json!({}), &perms, Some(&agent)).await;
    assert!(result.is_ok());
    assert!(registry.list_quarantined().is_empty());
}
```

### Test 5 — No registry attached: enforcement skipped (test compatibility)

```rust
#[tokio::test]
async fn test_hal_no_registry_skips_device_check() {
    // Existing test pattern: no registry attached
    let hal = HardwareAbstractionLayer::new_with_defaults();
    let agent = AgentID::new();
    let mut perms = PermissionSet::new();
    perms.grant("hardware.gpu".to_string(), true, false, false, None);

    // Should not panic or error due to missing registry
    let result = hal.query("gpu", json!({}), &perms, Some(&agent)).await;
    // Success or HalError due to no GPU hardware, not registry denial
    // The important assertion: no panic
}
```

---

## Verification

```bash
# Build must pass with no warnings related to changed function signatures
cargo build -p agentos-hal -p agentos-tools -p agentos-kernel

# All tests must pass
cargo test -p agentos-hal
cargo test -p agentos-tools
cargo test -p agentos-kernel

# Clippy must pass
cargo clippy -p agentos-hal -p agentos-tools -p agentos-kernel -- -D warnings

# Format check
cargo fmt --all -- --check

# Manual smoke test: start kernel, register a GPU device, attempt access without approval
agentctl kernel start &
agentctl hal list                  # should show empty
agentctl hal register gpu:0 nvidia-test
agentctl hal list                  # should show gpu:0 as Quarantined
# Any agent attempting hardware-info (gpu) should now get denied
agentctl hal approve gpu:0 --agent <agent-name>
agentctl hal list                  # should show gpu:0 as Approved
# Agent should now succeed
```

---

## Related

- [[V3 Completion Plan]] — Master plan
- [[02-mcp-adapter]] — Phase 2 (independent)
