---
title: "Phase 1: USB Storage Driver (UDisks2)"
tags:
  - hal
  - hardware
  - usb-storage
  - phase-1
date: 2026-03-26
status: completed
effort: 2d
priority: high
---

# Phase 1: USB Storage Driver (UDisks2)

> Add mount/unmount/eject capabilities for USB drives via UDisks2 D-Bus API, enabling agents to read/write files on removable storage.

---

## Completed

- Implemented a feature-gated `UsbStorageDriver` using `zbus` and UDisks2 for `list`, `mount`, `unmount`, and `eject`.
- Restricted operations to USB-backed drives by checking UDisks2 drive metadata before sensitive actions.
- Wired the driver into HAL defaults and kernel boot behind the `usb-storage` feature flag.
- Added HAL event-sink support so successful USB mount/unmount/eject actions emit `DeviceMounted`, `DeviceUnmounted`, and `DeviceEjected` events into the kernel event/audit pipeline.
- Verified with:
  - `cargo test -p agentos-hal --features usb-storage`
  - `cargo build -p agentos-hal --no-default-features`
  - `cargo build -p agentos-kernel --features usb-storage`
  - `cargo clippy -p agentos-hal --features usb-storage -- -D warnings`

> Note: a live UDisks2 integration test environment was not available in this workspace, so end-to-end runtime validation against a real daemon remains a follow-up verification step.

## Why This Phase

USB drives are the most common external storage peripheral. The existing `StorageDriver` in `crates/agentos-hal/src/drivers/storage.rs` can only **list** block devices from `/sys/block/`. It cannot mount, unmount, or eject them. This phase adds a new `UsbStorageDriver` that talks to UDisks2 over D-Bus to perform actual storage operations.

## Current State → Target State

| Aspect | Current | Target |
|--------|---------|--------|
| USB detection | `StorageDriver` lists block devices (name, size, removable flag) | Unchanged — detection stays in `StorageDriver` |
| Mount/unmount | Not possible | New `UsbStorageDriver` mounts via UDisks2 D-Bus |
| Eject/power-off | Not possible | `UsbStorageDriver` calls `Drive.PowerOff()` |
| Security | Quarantine gate exists but no mount action to gate | Mount requires device approval + `hardware.usb-storage:x` permission |
| Feature flag | N/A | `usb-storage` feature in `agentos-hal/Cargo.toml` |

## Detailed Subtasks

### 1. Add `zbus` dependency (feature-gated)

**File:** `crates/agentos-hal/Cargo.toml`

```toml
[dependencies]
zbus = { version = "5", optional = true, default-features = false, features = ["tokio"] }

[features]
usb-storage = ["dep:zbus"]
```

Also add to workspace `Cargo.toml`:
```toml
[workspace.dependencies]
zbus = { version = "5", default-features = false, features = ["tokio"] }
```

### 2. Create `UsbStorageDriver` module

**File:** `crates/agentos-hal/src/drivers/usb_storage.rs` (new file)

```rust
#[cfg(feature = "usb-storage")]
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::{json, Value};
use crate::hal::HalDriver;

pub struct UsbStorageDriver;

impl UsbStorageDriver {
    pub fn new() -> Self { Self }
}

#[async_trait]
impl HalDriver for UsbStorageDriver {
    fn name(&self) -> &str { "usb-storage" }

    fn required_permission(&self) -> (&str, PermissionOp) {
        ("hardware.usb-storage", PermissionOp::Execute)
    }

    fn device_key(&self, params: &Value) -> Option<String> {
        params.get("device")
            .and_then(|v| v.as_str())
            .map(|d| {
                let sanitized: String = d.chars()
                    .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
                    .collect();
                format!("usb-storage:{}", sanitized)
            })
    }

    async fn query(&self, params: Value) -> Result<Value, AgentOSError> {
        let action = params.get("action")
            .and_then(|a| a.as_str())
            .unwrap_or("list");

        match action {
            "mount" => self.mount_device(&params).await,
            "unmount" => self.unmount_device(&params).await,
            "eject" => self.eject_device(&params).await,
            "list" => self.list_filesystems().await,
            other => Err(AgentOSError::HalError(
                format!("Unknown usb-storage action: {}", other)
            )),
        }
    }
}
```

### 3. Implement D-Bus operations

Each method connects to the system D-Bus via `zbus::Connection::system().await?` and calls UDisks2:

**Mount:**
```rust
async fn mount_device(&self, params: &Value) -> Result<Value, AgentOSError> {
    let device = params.get("device").and_then(|v| v.as_str())
        .ok_or_else(|| AgentOSError::HalError("Missing 'device' param".into()))?;

    // Validate device name (no path traversal)
    if device.contains("..") || device.contains('/') {
        return Err(AgentOSError::HalError("Invalid device name".into()));
    }

    let conn = zbus::Connection::system().await
        .map_err(|e| AgentOSError::HalError(format!("D-Bus connect failed: {e}")))?;

    let object_path = format!("/org/freedesktop/UDisks2/block_devices/{}", device);

    // Call Filesystem.Mount with safe options
    let mount_options: HashMap<String, zbus::zvariant::Value> = [
        ("nosuid".to_string(), zbus::zvariant::Value::from("true")),
        ("noexec".to_string(), zbus::zvariant::Value::from("true")),
        ("nodev".to_string(), zbus::zvariant::Value::from("true")),
    ].into();

    // Call Mount method, receive mount path string
    let reply: String = conn.call_method(
        Some("org.freedesktop.UDisks2"),
        &object_path,
        Some("org.freedesktop.UDisks2.Filesystem"),
        "Mount",
        &(mount_options,),
    ).await
    .map_err(|e| AgentOSError::HalError(format!("Mount failed: {e}")))?
    .body().deserialize()?;

    Ok(json!({
        "mounted": true,
        "mount_path": reply,
        "device": device,
        "options": ["nosuid", "noexec", "nodev"]
    }))
}
```

**Unmount:** Call `Filesystem.Unmount({})` on the same object path.

**Eject:** Call `Drive.PowerOff({})` on the parent drive object.

**List:** Call `ObjectManager.GetManagedObjects()` and filter for objects implementing `org.freedesktop.UDisks2.Filesystem`.

### 4. Register driver conditionally

**File:** `crates/agentos-hal/src/drivers/mod.rs`

Add:
```rust
#[cfg(feature = "usb-storage")]
pub mod usb_storage;
```

**File:** `crates/agentos-hal/src/hal.rs` — update `new_with_defaults()`:
```rust
#[cfg(feature = "usb-storage")]
hal.register(Box::new(crate::drivers::usb_storage::UsbStorageDriver::new()));
```

### 5. Add audit events

**File:** `crates/agentos-types/src/lib.rs` (or `event.rs`)

Add to the event enum:
```rust
DeviceMounted { agent_id: AgentID, device_key: String, mount_path: String },
DeviceUnmounted { agent_id: AgentID, device_key: String },
DeviceEjected { agent_id: AgentID, device_key: String },
```

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-hal/Cargo.toml` | Add `zbus` optional dep + `usb-storage` feature |
| `Cargo.toml` (workspace) | Add `zbus` to workspace deps |
| `crates/agentos-hal/src/drivers/usb_storage.rs` | **New** — `UsbStorageDriver` implementation |
| `crates/agentos-hal/src/drivers/mod.rs` | Conditionally include `usb_storage` module |
| `crates/agentos-hal/src/hal.rs` | Register driver in `new_with_defaults()` |
| `crates/agentos-types/src/lib.rs` | Add `DeviceMounted`, `DeviceUnmounted`, `DeviceEjected` events |

## Dependencies

- **Requires:** Device quarantine gate (Phase 1 of v3-completion — already completed)
- **Blocks:** Phase 8 (tool manifests)

## Test Plan

1. **Unit test — device key generation:** Verify `device_key("sdb1")` → `"usb-storage:sdb1"`, rejects `../etc` and `/dev/sda`.
2. **Unit test — action dispatch:** Verify unknown actions return `HalError`.
3. **Integration test (requires D-Bus):** If UDisks2 is available, mount a test image (loopback), verify mount path returned, unmount, verify cleanup.
4. **Feature flag test:** Build with `--no-default-features` — verify `usb_storage` module is excluded.
5. **Permission test:** Verify driver requires `hardware.usb-storage:x` permission.

## Verification

```bash
# Build with feature
cargo build -p agentos-hal --features usb-storage

# Build without feature (should compile, driver excluded)
cargo build -p agentos-hal --no-default-features

# Run tests
cargo test -p agentos-hal --features usb-storage

# Clippy
cargo clippy -p agentos-hal --features usb-storage -- -D warnings
```

## Related

- [[Peripheral Device Ecosystem Plan]] — master plan
- [[Peripheral Device Ecosystem Research]] — UDisks2 protocol details (section 2.2)
- [[Peripheral Device Data Flow]] — mount flow diagram (section 2)
