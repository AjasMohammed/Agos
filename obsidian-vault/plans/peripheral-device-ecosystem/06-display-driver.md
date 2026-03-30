---
title: "Phase 6: Display Driver (Wayland/X11)"
tags:
  - hal
  - hardware
  - display
  - phase-6
date: 2026-03-26
status: planned
effort: 1.5d
priority: medium
---

# Phase 6: Display Driver (Wayland Output Management)

> Add display/monitor detection, resolution configuration, and safe output management via Wayland protocols (with GNOME D-Bus fallback).

---

## Why This Phase

Display management lets agents adapt their environment (set resolution for screen capture), detect multi-monitor setups, and configure outputs for digital signage or kiosk deployments. The risk is low-medium but requires a safety mechanism: auto-revert if a bad config is applied.

## Current State → Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Monitor detection | `GpuDriver` sees GPU cards but ignores connected monitors | `DisplayDriver` enumerates outputs with resolutions |
| Resolution change | Not available | Apply mode changes via `wlr-output-management` protocol |
| Safety | N/A | `test()` before `apply()`; auto-revert after 15s without confirmation |
| Feature flag | N/A | `display` feature in `agentos-hal/Cargo.toml` |

## Detailed Subtasks

### 1. Add Wayland crate dependencies (feature-gated)

**File:** `crates/agentos-hal/Cargo.toml`

```toml
[dependencies]
wayland-client = { version = "0.31", optional = true }
wayland-protocols-wlr = { version = "0.3", optional = true }
# For GNOME fallback
# zbus is already available if usb-storage feature is enabled
# Otherwise add it here too

[features]
display = ["dep:wayland-client", "dep:wayland-protocols-wlr"]
```

### 2. Create `DisplayDriver` module

**File:** `crates/agentos-hal/src/drivers/display.rs` (new file)

Actions:
- `list` — enumerate connected outputs (monitors) with current mode, available modes, position, scale
- `set_mode` — change resolution/refresh rate for an output
- `set_position` — reposition an output in a multi-monitor setup
- `enable` / `disable` — toggle an output
- `test` — validate a configuration without applying

### 3. Implement via `wlr-output-management-unstable-v1`

```rust
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols_wlr::output_management::v1::client::{
    zwlr_output_manager_v1, zwlr_output_head_v1, zwlr_output_mode_v1,
    zwlr_output_configuration_v1, zwlr_output_configuration_head_v1,
};

pub struct DisplayDriver {
    auto_revert_timeout: Duration,
}

impl DisplayDriver {
    pub fn new() -> Self {
        Self {
            auto_revert_timeout: Duration::from_secs(15),
        }
    }
}
```

**List outputs:**
1. Connect to Wayland compositor
2. Bind `zwlr_output_manager_v1` global
3. Receive `head` events (one per output) with modes, positions, scale
4. Return structured JSON with all output info

**Set mode (with auto-revert):**
1. Save current configuration as "rollback snapshot"
2. `create_configuration(serial)`
3. `enable_head(head)` → `set_mode(mode)`
4. Call `test()` first — if fails, return error
5. Call `apply()`
6. Start 15s timer; if not confirmed via `confirm` action, re-apply rollback snapshot
7. Emit `DisplayConfigApplied` audit event

### 4. GNOME fallback (D-Bus)

For GNOME (Mutter), use `org.gnome.Mutter.DisplayConfig` D-Bus API via `zbus`:

```rust
#[cfg(feature = "display")]
async fn list_outputs_gnome(&self) -> Result<Value, AgentOSError> {
    let conn = zbus::Connection::session().await?;
    // Call GetCurrentState on org.gnome.Mutter.DisplayConfig
    // Parse monitors, modes, logical monitors
}
```

Detect compositor at runtime: check `$XDG_CURRENT_DESKTOP` or probe for `zwlr_output_manager_v1` global.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-hal/Cargo.toml` | Add `wayland-client`, `wayland-protocols-wlr` optional deps + `display` feature |
| `crates/agentos-hal/src/drivers/display.rs` | **New** — `DisplayDriver` with auto-revert |
| `crates/agentos-hal/src/drivers/mod.rs` | Conditionally include `display` module |
| `crates/agentos-hal/src/hal.rs` | Register driver in `new_with_defaults()` |
| `crates/agentos-types/src/lib.rs` | Add `DisplayConfigApplied`, `DisplayConfigReverted` events |

## Dependencies

- **Requires:** None (independent)
- **Blocks:** Phase 8 (tool manifests)
- **System dep:** Running Wayland compositor (Sway, Hyprland for wlr; GNOME for Mutter path)

## Test Plan

1. **Unit test — auto-revert timer:** Mock a config apply, verify rollback fires after 15s without confirmation.
2. **Unit test — mode validation:** Verify invalid resolution (e.g., 99999x99999) is rejected by `test()`.
3. **Integration test (requires Wayland):** If compositor is running, list outputs, verify at least one returned.
4. **Feature flag test:** Build without `display` feature — module excluded.

## Verification

```bash
cargo build -p agentos-hal --features display
cargo test -p agentos-hal --features display
cargo clippy -p agentos-hal --features display -- -D warnings
```

## Related

- [[Peripheral Device Ecosystem Plan]] — master plan
- [[Peripheral Device Ecosystem Research]] — Wayland output management details (section 2.6)
