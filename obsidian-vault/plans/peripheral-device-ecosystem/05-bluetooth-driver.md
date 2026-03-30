---
title: "Phase 5: Bluetooth Driver (BlueZ D-Bus)"
tags:
  - hal
  - hardware
  - bluetooth
  - phase-5
date: 2026-03-26
status: planned
effort: 2.5d
priority: medium
---

# Phase 5: Bluetooth Driver (BlueZ D-Bus)

> Add Bluetooth device discovery, pairing, connection, and BLE GATT communication via the BlueZ D-Bus API.

---

## Why This Phase

Bluetooth enables interaction with wireless peripherals: speakers, keyboards, IoT sensors, health monitors, BLE beacons, and serial devices. The BlueZ stack exposes everything over D-Bus — no root needed with proper group membership.

## Current State → Target State

| Aspect | Current | Target |
|--------|---------|--------|
| BT adapter detection | Not available | `BluetoothDriver` discovers adapters via BlueZ D-Bus |
| Device scanning | Not available | Time-limited discovery with `StartDiscovery()` |
| Pairing | Not available | Pair via `Device1.Pair()` with escalation for passkey |
| BLE GATT | Not available | Read/write GATT characteristics |
| Feature flag | N/A | `bluetooth` feature in `agentos-hal/Cargo.toml` |

## Detailed Subtasks

### 1. Add `bluer` dependency (feature-gated)

**File:** `crates/agentos-hal/Cargo.toml`

```toml
[dependencies]
bluer = { version = "0.17", optional = true, features = ["full"] }

[features]
bluetooth = ["dep:bluer"]
```

`bluer` is the official Rust BlueZ bindings maintained under the BlueZ GitHub org. It covers adapters, discovery, pairing, GATT client/server, L2CAP, and RFCOMM.

### 2. Create `BluetoothDriver` module

**File:** `crates/agentos-hal/src/drivers/bluetooth.rs` (new file)

Actions:
- `list_adapters` — enumerate Bluetooth adapters (hci0, hci1, ...)
- `scan` — start time-limited discovery, return found devices
- `pair` — pair with a device (requires escalation for passkey confirmation)
- `connect` — connect to an already-paired device
- `disconnect` — disconnect a device
- `gatt_read` — read a GATT characteristic by UUID
- `gatt_write` — write a GATT characteristic by UUID

### 3. Implement scanning with time limit

```rust
async fn scan_devices(&self, params: &Value) -> Result<Value, AgentOSError> {
    let duration = params.get("duration_seconds")
        .and_then(|v| v.as_u64()).unwrap_or(10);

    // Cap scan duration to prevent indefinite MAC exposure
    let duration = duration.min(30);

    let session = bluer::Session::new().await
        .map_err(|e| AgentOSError::HalError(format!("BlueZ session: {e}")))?;
    let adapter = session.default_adapter().await
        .map_err(|e| AgentOSError::HalError(format!("No BT adapter: {e}")))?;

    adapter.set_powered(true).await.ok();

    let discover = adapter.discover_devices().await
        .map_err(|e| AgentOSError::HalError(format!("Start discovery: {e}")))?;

    let mut devices = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(duration);

    tokio::pin!(discover);
    loop {
        tokio::select! {
            event = discover.next() => {
                if let Some(bluer::AdapterEvent::DeviceAdded(addr)) = event {
                    if let Ok(device) = adapter.device(addr).await {
                        devices.push(json!({
                            "address": addr.to_string(),
                            "name": device.name().await.ok().flatten().unwrap_or_default(),
                            "rssi": device.rssi().await.ok().flatten(),
                            "paired": device.is_paired().await.unwrap_or(false),
                            "connected": device.is_connected().await.unwrap_or(false),
                        }));
                    }
                }
            }
            _ = tokio::time::sleep_until(deadline) => break,
        }
    }

    Ok(json!({ "devices": devices, "scan_duration_seconds": duration }))
}
```

### 4. Implement pairing with escalation

Pairing is security-sensitive — it requires operator approval:

```rust
async fn pair_device(&self, params: &Value) -> Result<Value, AgentOSError> {
    let address_str = params.get("address").and_then(|v| v.as_str())
        .ok_or_else(|| AgentOSError::HalError("Missing 'address' param".into()))?;

    let addr: bluer::Address = address_str.parse()
        .map_err(|e| AgentOSError::HalError(format!("Invalid BT address: {e}")))?;

    // Pairing requires escalation (handled by kernel before reaching here)
    // The kernel creates a PendingEscalation with the device name + address

    let session = bluer::Session::new().await?;
    let adapter = session.default_adapter().await?;
    let device = adapter.device(addr).await?;

    device.pair().await
        .map_err(|e| AgentOSError::HalError(format!("Pair failed: {e}")))?;

    Ok(json!({
        "paired": true,
        "address": address_str,
        "name": device.name().await.ok().flatten(),
    }))
}
```

### 5. Implement GATT read/write

```rust
async fn gatt_read(&self, params: &Value) -> Result<Value, AgentOSError> {
    // Connect to device, find service by UUID, find characteristic by UUID, read
    // Return base64-encoded bytes
}

async fn gatt_write(&self, params: &Value) -> Result<Value, AgentOSError> {
    // Connect to device, find service, find characteristic, write bytes
}
```

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-hal/Cargo.toml` | Add `bluer` optional dep + `bluetooth` feature |
| `crates/agentos-hal/src/drivers/bluetooth.rs` | **New** — `BluetoothDriver` |
| `crates/agentos-hal/src/drivers/mod.rs` | Conditionally include `bluetooth` module |
| `crates/agentos-hal/src/hal.rs` | Register driver in `new_with_defaults()` |
| `crates/agentos-types/src/lib.rs` | Add `BluetoothScanStarted`, `BluetoothPairRequested`, `BluetoothConnected` events |

## Dependencies

- **Requires:** None (independent)
- **Blocks:** Phase 8 (tool manifests)
- **System dep:** `bluetoothd` running, user in `bluetooth` group

## Test Plan

1. **Unit test — scan duration capping:** Verify duration >30s is capped to 30s.
2. **Unit test — address validation:** Verify malformed BT addresses are rejected.
3. **Unit test — device key:** Verify `device_key` returns `"bluetooth:XX:XX:XX:XX:XX:XX"`.
4. **Integration test (requires BT adapter):** Scan for 5s, verify device list returned.
5. **Feature flag test:** Build without `bluetooth` feature — module excluded.

## Verification

```bash
cargo build -p agentos-hal --features bluetooth
cargo test -p agentos-hal --features bluetooth
cargo clippy -p agentos-hal --features bluetooth -- -D warnings
```

## Related

- [[Peripheral Device Ecosystem Plan]] — master plan
- [[Peripheral Device Ecosystem Research]] — BlueZ D-Bus details (section 2.4)
- [[Peripheral Device Data Flow]] — Bluetooth pairing flow (section 5)
