---
title: "Phase 7: Raw USB Driver (libusb)"
tags:
  - hal
  - hardware
  - usb
  - security
  - phase-7
date: 2026-03-26
status: planned
effort: 1.5d
priority: medium
---

# Phase 7: Raw USB Driver (nusb)

> Add raw USB device enumeration and bulk/interrupt/control transfers via `nusb` (pure-Rust, async USB library), with the most restrictive security tier.

---

## Why This Phase

Raw USB access enables communication with specialized hardware: microcontrollers (Arduino, STM32), lab equipment, barcode scanners, HID devices, USB-serial adapters, and custom hardware. This is the most powerful — and dangerous — peripheral driver. It bypasses kernel drivers and talks directly to USB endpoints.

## Current State → Target State

| Aspect | Current | Target |
|--------|---------|--------|
| USB device listing | Not available | `RawUsbDriver` enumerates devices by vendor/product ID |
| Data transfers | Not available | Bulk, interrupt, and control transfers to claimed interfaces |
| Security tier | N/A | **Most restrictive** — per-device vendor/product whitelist |
| Kernel driver detach | N/A | **Blocked by default** — requires elevated permission |
| Feature flag | N/A | `raw-usb` feature in `agentos-hal/Cargo.toml` |

## Detailed Subtasks

### 1. Add `nusb` dependency (feature-gated)

**File:** `crates/agentos-hal/Cargo.toml`

```toml
[dependencies]
nusb = { version = "0.2", optional = true }

[features]
raw-usb = ["dep:nusb"]
```

`nusb` is pure-Rust (no C FFI), async-first, and talks directly to the Linux kernel's USB subsystem. Preferred over `rusb` for new projects.

### 2. Create `RawUsbDriver` module

**File:** `crates/agentos-hal/src/drivers/raw_usb.rs` (new file)

Actions:
- `list` — enumerate USB devices (vendor_id, product_id, manufacturer, product, serial)
- `open` — open a device by vendor/product ID and claim an interface
- `read` — bulk or interrupt read from an endpoint
- `write` — bulk or interrupt write to an endpoint
- `control` — control transfer (setup packet + optional data)
- `close` — release interface

### 3. Implement vendor/product whitelist

```rust
pub struct RawUsbDriver {
    /// Allowed (vendor_id, product_id) pairs. If empty, all are blocked.
    whitelist: Arc<RwLock<HashSet<(u16, u16)>>>,
    /// Whether kernel driver detach is allowed
    allow_detach: bool,
}

impl RawUsbDriver {
    pub fn new() -> Self {
        Self {
            whitelist: Arc::new(RwLock::new(HashSet::new())),
            allow_detach: false, // Blocked by default
        }
    }

    pub fn allow_device(&self, vendor_id: u16, product_id: u16) {
        self.whitelist.write().unwrap().insert((vendor_id, product_id));
    }

    fn is_allowed(&self, vendor_id: u16, product_id: u16) -> bool {
        self.whitelist.read().unwrap().contains(&(vendor_id, product_id))
    }
}
```

### 4. Implement USB operations

```rust
async fn list_devices(&self) -> Result<Value, AgentOSError> {
    let devices: Vec<Value> = nusb::list_devices()
        .map_err(|e| AgentOSError::HalError(format!("USB enumerate: {e}")))?
        .map(|info| {
            json!({
                "vendor_id": format!("{:04x}", info.vendor_id()),
                "product_id": format!("{:04x}", info.product_id()),
                "manufacturer": info.manufacturer_string().unwrap_or_default(),
                "product": info.product_string().unwrap_or_default(),
                "serial": info.serial_number().unwrap_or_default(),
                "bus": info.bus_number(),
                "address": info.device_address(),
                "whitelisted": self.is_allowed(info.vendor_id(), info.product_id()),
            })
        })
        .collect();

    Ok(json!({ "devices": devices }))
}

async fn open_and_transfer(&self, params: &Value) -> Result<Value, AgentOSError> {
    let vid: u16 = params.get("vendor_id")...;
    let pid: u16 = params.get("product_id")...;

    // Whitelist check
    if !self.is_allowed(vid, pid) {
        return Err(AgentOSError::PermissionDenied {
            resource: format!("usb:{:04x}:{:04x}", vid, pid),
            operation: "raw_access".to_string(),
        });
    }

    let device = nusb::list_devices()?
        .find(|d| d.vendor_id() == vid && d.product_id() == pid)
        .ok_or_else(|| AgentOSError::HalError("Device not found".into()))?;

    let handle = device.open()
        .map_err(|e| AgentOSError::HalError(format!("USB open: {e}")))?;

    let interface_num = params.get("interface").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
    let iface = handle.claim_interface(interface_num)
        .map_err(|e| AgentOSError::HalError(format!("Claim interface: {e}")))?;

    // Perform transfer based on action
    let action = params.get("transfer_type").and_then(|v| v.as_str()).unwrap_or("bulk_read");
    match action {
        "bulk_read" => { /* iface.bulk_in(endpoint).await */ }
        "bulk_write" => { /* iface.bulk_out(endpoint, data).await */ }
        "interrupt_read" => { /* iface.interrupt_in(endpoint).await */ }
        "control" => { /* handle.control_in/out(setup).await */ }
        _ => return Err(AgentOSError::HalError("Unknown transfer type".into())),
    }
}
```

### 5. Block kernel driver detach

```rust
// In open_and_transfer:
if params.get("detach_kernel_driver").and_then(|v| v.as_bool()).unwrap_or(false) {
    if !self.allow_detach {
        return Err(AgentOSError::PermissionDenied {
            resource: "usb:kernel_driver_detach".to_string(),
            operation: "detach".to_string(),
        });
    }
    // Only if explicitly allowed in driver config
    handle.detach_kernel_driver(interface_num)?;
}
```

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-hal/Cargo.toml` | Add `nusb` optional dep + `raw-usb` feature |
| `crates/agentos-hal/src/drivers/raw_usb.rs` | **New** — `RawUsbDriver` with whitelist |
| `crates/agentos-hal/src/drivers/mod.rs` | Conditionally include `raw_usb` module |
| `crates/agentos-hal/src/hal.rs` | Register driver in `new_with_defaults()` |
| `crates/agentos-types/src/lib.rs` | Add `RawUsbTransfer`, `RawUsbDeviceOpened` events |

## Dependencies

- **Requires:** None (independent)
- **Blocks:** Phase 8 (tool manifests)
- **System dep:** udev rules granting user access to target USB devices

## Test Plan

1. **Unit test — whitelist enforcement:** Add device (0x1234, 0x5678) to whitelist, verify access allowed. Verify non-whitelisted device is rejected.
2. **Unit test — kernel detach blocked:** Verify `detach_kernel_driver: true` returns `PermissionDenied` when `allow_detach` is false.
3. **Unit test — device key:** Verify `device_key` returns `"raw-usb:1234:5678"`.
4. **Integration test (requires USB device):** If a test device is connected, enumerate and verify it appears.
5. **Feature flag test:** Build without `raw-usb` feature — module excluded.

## Verification

```bash
cargo build -p agentos-hal --features raw-usb
cargo test -p agentos-hal --features raw-usb
cargo clippy -p agentos-hal --features raw-usb -- -D warnings
```

## Related

- [[Peripheral Device Ecosystem Plan]] — master plan
- [[Peripheral Device Ecosystem Research]] — libusb/nusb details (section 2.7)
