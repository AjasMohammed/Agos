---
title: "Phase 8: Peripheral Tool Manifests and Agent Tools"
tags:
  - hal
  - hardware
  - tools
  - phase-8
date: 2026-03-26
status: planned
effort: 1.5d
priority: high
---

# Phase 8: Peripheral Tool Manifests and Agent Tools

> Create tool wrappers and TOML manifests for all 7 peripheral drivers, so agents can invoke them through the standard intent/tool system.

---

## Why This Phase

HAL drivers are internal infrastructure — agents interact with tools, not drivers directly. Each peripheral needs:
1. A **tool wrapper** in `crates/agentos-tools/src/` that calls the HAL driver
2. A **TOML manifest** in `tools/core/` for tool discovery and registration
3. Proper **permission declarations** matching the driver's security tier

## Current State → Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Peripheral tools | 0 (only `hardware-info` and `network-monitor`) | 7 new tools, one per peripheral driver |
| Tool manifests | None for peripherals | 7 TOML files in `tools/core/` |
| Agent discoverability | Agents cannot find peripheral capabilities | All peripheral tools discoverable via `agentctl tool list` |

## Detailed Subtasks

### 1. Create tool wrappers

Each tool follows the existing pattern (see `crates/agentos-tools/src/hardware_info.rs`):

**File pattern:** `crates/agentos-tools/src/<peripheral>.rs`

| Tool file | HAL Driver | Actions | Permission |
|-----------|-----------|---------|------------|
| `usb_storage.rs` | `UsbStorageDriver` | mount, unmount, eject, list | `hardware.usb-storage:x` |
| `printer.rs` | `PrinterDriver` | list, print, status, cancel | `hardware.printer:x` |
| `webcam.rs` | `WebcamDriver` | list, capture, burst | `hardware.webcam:x` |
| `audio.rs` | `AudioDriver` | list, capture, playback, volume | `hardware.audio.capture:x`, `hardware.audio.playback:x` |
| `bluetooth.rs` | `BluetoothDriver` | scan, pair, connect, disconnect, gatt_read, gatt_write | `hardware.bluetooth:x` |
| `display.rs` | `DisplayDriver` | list, set_mode, set_position, enable, disable, test | `hardware.display:x` |
| `raw_usb.rs` | `RawUsbDriver` | list, read, write, control | `hardware.raw-usb:x` |

**Example tool wrapper (printer):**

```rust
use agentos_types::{AgentOSError, ToolOutput};
use serde_json::Value;

pub async fn execute(
    params: Value,
    hal: &HardwareAbstractionLayer,
    agent_id: &AgentID,
    permissions: &PermissionSet,
) -> Result<ToolOutput, AgentOSError> {
    let result = hal.query("printer", params, permissions, Some(agent_id)).await?;

    Ok(ToolOutput {
        content: serde_json::to_string_pretty(&result)
            .unwrap_or_else(|_| result.to_string()),
        metadata: Some(result),
    })
}
```

### 2. Create TOML manifests

**Location:** `tools/core/`

**Example: `tools/core/printer.toml`**
```toml
[tool]
name = "printer"
version = "0.1.0"
description = "Manage printers and submit print jobs via CUPS/IPP"
trust_tier = "core"

[tool.permissions]
required = ["hardware.printer:x"]

[tool.schema]
type = "object"
properties.action = { type = "string", enum = ["list", "print", "status", "cancel"] }
properties.printer = { type = "string", description = "Printer name" }
properties.document_path = { type = "string", description = "Path to document to print" }
properties.format = { type = "string", default = "application/pdf" }
properties.copies = { type = "integer", default = 1 }
properties.job_id = { type = "integer", description = "Job ID for status/cancel" }
required = ["action"]
```

**All manifests to create:**

| Manifest | Permission | Trust |
|----------|-----------|-------|
| `tools/core/usb-storage.toml` | `hardware.usb-storage:x` | core |
| `tools/core/printer.toml` | `hardware.printer:x` | core |
| `tools/core/webcam.toml` | `hardware.webcam:x` | core |
| `tools/core/audio.toml` | `hardware.audio.capture:x`, `hardware.audio.playback:x` | core |
| `tools/core/bluetooth.toml` | `hardware.bluetooth:x` | core |
| `tools/core/display-config.toml` | `hardware.display:x` | core |
| `tools/core/raw-usb.toml` | `hardware.raw-usb:x` | core |

### 3. Register tools in `ToolRegistry`

**File:** `crates/agentos-kernel/src/tool_registry.rs`

Add conditional registration in the kernel boot path:

```rust
#[cfg(feature = "usb-storage")]
registry.register(load_manifest("tools/core/usb-storage.toml")?)?;

#[cfg(feature = "printer")]
registry.register(load_manifest("tools/core/printer.toml")?)?;

// ... etc for each peripheral
```

### 4. Add CLI discovery

**File:** `crates/agentos-cli/src/commands/tool.rs`

Ensure `agentctl tool list` shows peripheral tools when their features are enabled. No code change needed if tool discovery already reads from `tools/core/` — just verify.

### 5. Update default agent permissions

**File:** `crates/agentos-kernel/src/config.rs` or default agent permission setup

Peripheral permissions should NOT be granted by default. Agents must explicitly request them, and the operator must approve. Only add them to the agent's `PermissionSet` when the agent's manifest declares them.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-tools/src/usb_storage.rs` | **New** — USB storage tool wrapper |
| `crates/agentos-tools/src/printer.rs` | **New** — Printer tool wrapper |
| `crates/agentos-tools/src/webcam.rs` | **New** — Webcam tool wrapper |
| `crates/agentos-tools/src/audio.rs` | **New** — Audio tool wrapper |
| `crates/agentos-tools/src/bluetooth.rs` | **New** — Bluetooth tool wrapper |
| `crates/agentos-tools/src/display.rs` | **New** — Display config tool wrapper |
| `crates/agentos-tools/src/raw_usb.rs` | **New** — Raw USB tool wrapper |
| `tools/core/usb-storage.toml` | **New** — USB storage manifest |
| `tools/core/printer.toml` | **New** — Printer manifest |
| `tools/core/webcam.toml` | **New** — Webcam manifest |
| `tools/core/audio.toml` | **New** — Audio manifest |
| `tools/core/bluetooth.toml` | **New** — Bluetooth manifest |
| `tools/core/display-config.toml` | **New** — Display config manifest |
| `tools/core/raw-usb.toml` | **New** — Raw USB manifest |
| `crates/agentos-tools/src/lib.rs` | Export new tool modules |
| `crates/agentos-kernel/src/tool_registry.rs` | Conditional registration |

## Dependencies

- **Requires:** All of Phases 1-7 (drivers must exist before tools wrap them)
- **Blocks:** None — this is the final phase

## Test Plan

1. **Manifest validation:** Load each TOML manifest, verify schema parses correctly.
2. **Tool registration:** Boot kernel with all features, verify all 7 peripheral tools appear in `agentctl tool list`.
3. **Permission enforcement:** Attempt to call printer tool without `hardware.printer:x` permission — verify denied.
4. **End-to-end (per driver):** Call each tool with a valid action and verify it dispatches to the correct HAL driver.

## Verification

```bash
# Build with all peripheral features
cargo build --workspace --features all-peripherals

# Run all tests
cargo test --workspace --features all-peripherals

# Verify tool manifests are valid
for f in tools/core/usb-storage.toml tools/core/printer.toml tools/core/webcam.toml \
         tools/core/audio.toml tools/core/bluetooth.toml tools/core/display-config.toml \
         tools/core/raw-usb.toml; do
    echo "Checking $f..."
    cargo run -p agentos-cli -- tool verify "$f" 2>&1 || echo "WARN: $f"
done

# Clippy
cargo clippy --workspace --features all-peripherals -- -D warnings
```

## Related

- [[Peripheral Device Ecosystem Plan]] — master plan
- All phase files [[01-usb-storage-driver]] through [[07-raw-usb-driver]]
