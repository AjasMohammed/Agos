---
title: "Phase 9: Device Twin & Safety Engine"
tags:
  - plan
  - real-world
  - hardware
  - iot
  - safety
  - phase-9
date: 2026-04-08
status: complete
effort: 2.5d
priority: low
---

# Phase 9: Device Twin & Safety Engine

> Add a Device Twin state model (desired vs. reported state) and a declarative Safety Engine that evaluates operator-defined rules in Rust before any physical actuator command executes.

---

## Why This Phase

Agents can now discover IoT devices (Phase 8), but letting an LLM directly set physical state is dangerous. Hallucinated commands could:

- Turn on a heater when temperature is already 80C
- Unlock a door at 3AM
- Activate industrial equipment without safety preconditions

The Device Twin pattern (proven by Azure IoT Hub, AWS IoT Shadow) decouples agent intent from physical action:
1. Agent writes to **desired state** ("I want the light on")
2. Safety Engine evaluates rules ("is it safe to turn the light on?")
3. If safe, the OS sends the command to the physical device
4. Device reports back **reported state** ("the light is now on")

The Safety Engine is pure Rust — no LLM in the loop. Operator-defined rules are the final authority.

---

## Current State

- `HardwareRegistry` tracks devices with `DeviceStatus` (Pending/Approved/Quarantined) and per-agent grants
- `DeviceAccessGate` trait checks permissions before hardware access
- HAL drivers execute queries directly — no desired/reported state model
- No safety rule evaluation exists
- No declarative rule config format

## Target State

- `DeviceTwin` struct: `desired_state`, `reported_state`, `last_reported_at`, `last_desired_at`
- `TwinRegistry` (SQLite-backed) extending `HardwareRegistry`
- `SafetyEngine` — evaluates declarative rules from `config/hardware_limits.toml`
- `SafetyRule` — condition expressions evaluated against twin state
- Safety middleware in `hardware.set_state` flow: desired → rules → execute → reported
- Audit events: `DesiredStateSet`, `SafetyRuleViolation`, `ReportedStateUpdated`
- Agent tools: `hardware.set-desired-state`, `hardware.get-twin`

---

## Detailed Subtasks

### 1. Device Twin model

**File:** `crates/agentos-hal/src/twin.rs` (new)

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceTwin {
    pub device_id: String,
    pub device_type: String,
    pub desired_state: Value,                // what the agent wants
    pub reported_state: Value,               // what the sensor reports
    pub desired_at: Option<DateTime<Utc>>,
    pub reported_at: Option<DateTime<Utc>>,
    pub desired_by: Option<AgentID>,         // which agent set desired state
    pub reconciled: bool,                    // desired == reported
}

pub struct TwinRegistry {
    db: rusqlite::Connection,
    twins: RwLock<HashMap<String, DeviceTwin>>,
}

impl TwinRegistry {
    pub fn new(db_path: &Path) -> Result<Self, AgentOSError>;

    /// Set the desired state for a device (agent-initiated)
    pub fn set_desired(
        &self,
        device_id: &str,
        state: Value,
        agent_id: &AgentID,
    ) -> Result<(), AgentOSError>;

    /// Update the reported state (from sensor/MQTT callback)
    pub fn update_reported(
        &self,
        device_id: &str,
        state: Value,
    ) -> Result<(), AgentOSError>;

    /// Get the full twin for a device
    pub fn get_twin(&self, device_id: &str) -> Option<DeviceTwin>;

    /// List all twins
    pub fn list_twins(&self) -> Vec<DeviceTwin>;

    /// List unreconciled twins (desired != reported)
    pub fn list_unreconciled(&self) -> Vec<DeviceTwin>;
}
```

SQLite schema:
```sql
CREATE TABLE IF NOT EXISTS device_twins (
    device_id TEXT PRIMARY KEY,
    device_type TEXT NOT NULL,
    desired_state TEXT NOT NULL DEFAULT '{}',
    reported_state TEXT NOT NULL DEFAULT '{}',
    desired_at TEXT,
    reported_at TEXT,
    desired_by TEXT,
    reconciled INTEGER NOT NULL DEFAULT 1
);
```

### 2. Safety rule format

**File:** `config/hardware_limits.toml` (new)

Operator-defined declarative rules:

```toml
# Each rule has a condition that must be true for the action to proceed.
# If the condition evaluates to false, the action is blocked.

[[rules]]
name = "thermal_protection"
description = "Block heater activation when temperature exceeds threshold"
device_type = "heater"
action = "set_state"
# Condition: the target device's desired state AND the referenced sensor's state
condition = """
  reported("thermal_sensor_1").temperature < 75.0
"""
error_message = "Cannot activate heater: ambient temperature too high ({reported.thermal_sensor_1.temperature}C >= 75C)"

[[rules]]
name = "night_lock"
description = "Prevent door unlocking between 11PM and 6AM without escalation"
device_type = "door_lock"
action = "unlock"
condition = """
  hour_of_day() >= 6 AND hour_of_day() < 23
"""
error_message = "Door unlock blocked by night lock policy (11PM-6AM)"
escalation = true   # if blocked, create escalation for operator approval

[[rules]]
name = "power_budget"
description = "Total active power draw must not exceed 3000W"
device_type = "*"
action = "set_state"
condition = """
  sum_reported("power_meter_*", "watts") + desired_watts < 3000
"""
error_message = "Action would exceed 3000W power budget"
```

### 3. Safety rule engine

**File:** `crates/agentos-hal/src/safety.rs` (new)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafetyRule {
    pub name: String,
    pub description: String,
    pub device_type: String,            // "*" matches all
    pub action: String,                 // "set_state", "unlock", "*"
    pub condition: SafetyCondition,
    pub error_message: String,
    pub escalation: bool,               // create escalation if blocked
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SafetyCondition {
    /// Compare a reported value: reported("device_id").field < threshold
    ReportedThreshold {
        device_id: String,
        field: String,
        op: CompareOp,
        threshold: f64,
    },
    /// Time-of-day window
    TimeWindow {
        start_hour: u32,
        end_hour: u32,
    },
    /// Aggregate: sum of reported field across matching devices
    AggregateThreshold {
        device_pattern: String,         // glob pattern
        field: String,
        op: CompareOp,
        threshold: f64,
    },
    /// Boolean AND/OR of sub-conditions
    And(Vec<SafetyCondition>),
    Or(Vec<SafetyCondition>),
    Not(Box<SafetyCondition>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CompareOp { Lt, Lte, Gt, Gte, Eq, Neq }

pub struct SafetyEngine {
    rules: Vec<SafetyRule>,
    twin_registry: Arc<TwinRegistry>,
}

impl SafetyEngine {
    pub fn from_config(path: &Path, twin_registry: Arc<TwinRegistry>) -> Result<Self, AgentOSError>;

    /// Evaluate all applicable rules for a desired state change.
    /// Returns Ok(()) if all pass, or Err with the first violated rule's message.
    pub fn evaluate(
        &self,
        device_id: &str,
        device_type: &str,
        action: &str,
        desired_state: &Value,
    ) -> Result<(), SafetyViolation>;

    /// Reload rules from config (supports hot-reload)
    pub fn reload(&mut self, path: &Path) -> Result<(), AgentOSError>;
}

#[derive(Debug)]
pub struct SafetyViolation {
    pub rule_name: String,
    pub message: String,
    pub requires_escalation: bool,
}
```

### 4. Integrate safety into HAL flow

**File:** `crates/agentos-hal/src/hal.rs`

Add safety engine as an optional field on `HardwareAbstractionLayer`:

```rust
pub struct HardwareAbstractionLayer {
    // ... existing fields ...
    pub safety_engine: Option<Arc<SafetyEngine>>,
    pub twin_registry: Option<Arc<TwinRegistry>>,
}
```

Override the `query` path for IoT drivers: when `action=set_state`:
1. Write to twin's `desired_state`
2. Evaluate safety rules
3. If safe → dispatch to driver (MQTT publish, HA service call)
4. If violated → return error (and create escalation if configured)
5. On success → update twin's `reported_state` from driver response

### 5. DeviceAccessGate implementation with safety

**File:** `crates/agentos-hal/src/safety_gate.rs` (new)

Implement `DeviceAccessGate` that combines the existing permission check with safety rule evaluation:

```rust
pub struct SafetyAwareGate {
    permission_gate: Arc<dyn DeviceAccessGate>,  // existing gate
    safety_engine: Arc<SafetyEngine>,
}

#[async_trait]
impl DeviceAccessGate for SafetyAwareGate {
    async fn check(&self, agent_id, task_id, device_id, device_type, operation) -> Result<(), AgentOSError> {
        // 1. Check permissions via inner gate
        self.permission_gate.check(agent_id, task_id, device_id, device_type, operation).await?;
        // 2. For write operations, evaluate safety rules
        if operation == HalOperation::Write {
            // Safety check happens later with the actual desired state
            // This gate only does permission checks; safety is in the query path
        }
        Ok(())
    }
}
```

### 6. Agent tools

**File:** `tools/core/hardware-set-desired.toml` (new)

```toml
[manifest]
name = "hardware-set-desired"
version = "1.0.0"
description = "Set the desired state for an IoT device (subject to safety rules)"
author = "agentos-core"
trust_tier = "core"

[capabilities_required]
permissions = ["hardware.iot:w"]

[sandbox]
network = false
fs_write = false
max_memory_mb = 32
max_cpu_ms = 5000
```

**File:** `tools/core/hardware-get-twin.toml` (new)

```toml
[manifest]
name = "hardware-get-twin"
version = "1.0.0"
description = "Get the device twin (desired + reported state) for an IoT device"
author = "agentos-core"
trust_tier = "core"

[capabilities_required]
permissions = ["hardware.iot:r"]
```

### 7. Context injection

When an agent has IoT device access, inject the safety constraints into its system prompt:

```
You have access to IoT devices. The following safety rules are in effect:
- thermal_protection: Heater cannot be activated when temperature >= 75C
- night_lock: Door unlock blocked between 11PM-6AM (requires escalation)
- power_budget: Total power draw must stay below 3000W

These rules are enforced by the OS kernel. If you request an action that violates
a rule, you will receive an error. Plan your actions accordingly.
```

This is informational only — the actual enforcement is in Rust.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-hal/src/twin.rs` | **New** — `DeviceTwin`, `TwinRegistry` (SQLite-backed) |
| `crates/agentos-hal/src/safety.rs` | **New** — `SafetyEngine`, `SafetyRule`, `SafetyCondition` |
| `crates/agentos-hal/src/safety_gate.rs` | **New** — `SafetyAwareGate` implementing `DeviceAccessGate` |
| `crates/agentos-hal/src/hal.rs` | Add `safety_engine` and `twin_registry` fields; integrate into query flow |
| `crates/agentos-hal/src/lib.rs` | Re-export new modules |
| `crates/agentos-hal/Cargo.toml` | Add `rusqlite` dependency (for twin storage) |
| `config/hardware_limits.toml` | **New** — Operator-defined safety rules |
| `tools/core/hardware-set-desired.toml` | **New** — Tool manifest |
| `tools/core/hardware-get-twin.toml` | **New** — Tool manifest |
| `crates/agentos-kernel/src/kernel.rs` | Initialize `TwinRegistry` and `SafetyEngine` at boot |

---

## Dependencies

- **Requires:** Phase 8 (IoT Protocol Drivers)
- **Blocks:** None (end of Subsystem D)

---

## Test Plan

1. **Unit: twin set/get** — Set desired state, verify stored; update reported, verify stored
2. **Unit: reconciliation flag** — Set desired != reported → `reconciled=false`; set desired == reported → `reconciled=true`
3. **Unit: safety rule parsing** — Parse `hardware_limits.toml`, verify rules deserialize correctly
4. **Unit: threshold condition** — `reported("sensor").temp < 75` with temp=70 → pass; temp=80 → fail
5. **Unit: time window** — `hour_of_day >= 6 AND < 23` at 14:00 → pass; at 02:00 → fail
6. **Unit: AND/OR/NOT** — Compound conditions evaluate correctly
7. **Integration: safety blocking** — Set desired state that violates a rule → verify `SafetyViolation` error returned
8. **Integration: safety passing** — Set desired state that satisfies all rules → verify command dispatched
9. **Integration: escalation** — Violate a rule with `escalation=true` → verify escalation created in kernel
10. **Security: rules are operator-only** — Verify agents cannot modify `hardware_limits.toml` or safety rules at runtime
11. **Security: no bypass** — Verify there is no code path that skips safety evaluation for IoT write operations

---

## Verification

```bash
cargo build -p agentos-hal --features mqtt,homeassistant
cargo test -p agentos-hal
cargo clippy -p agentos-hal -- -D warnings
cargo fmt -p agentos-hal -- --check
```
