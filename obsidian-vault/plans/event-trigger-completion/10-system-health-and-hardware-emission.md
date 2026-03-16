---
title: "Phase 10 — System Health & Hardware Event Emission"
tags:
  - kernel
  - event-system
  - hal
  - plan
  - v3
date: 2026-03-13
status: planned
effort: 4h
priority: low
---
# Phase 10 — System Health & Hardware Event Emission

> Implement a periodic health monitoring loop that emits CPUSpikeDetected, MemoryPressure, DiskSpaceLow, DiskSpaceCritical, and ProcessCrashed events based on HAL readings.

---

## Why This Phase

System health events enable sysops agents to autonomously monitor and respond to resource pressure. Without these events, resource issues go undetected until they cause task failures. The HAL crate (`agentos-hal`) already provides system metrics — this phase connects those readings to the event bus via a monitoring loop.

---

## Current State

| What | Status |
|------|--------|
| `EventType::CPUSpikeDetected` / `MemoryPressure` / `DiskSpaceLow` / `DiskSpaceCritical` / `ProcessCrashed` | Defined in `agentos-types/src/event.rs` |
| `agentos-hal` — system metrics (CPU, RAM, disk) | Working — `HardwareAbstractionLayer` provides readings |
| `health.rs` in kernel — periodic health checks | Exists — runs periodic checks, but doesn't emit events |
| GPU metrics in `agentos-hal/src/drivers/gpu.rs` | Working — can read VRAM usage |
| `EventType::GPUAvailable` / `GPUMemoryPressure` / `DeviceConnected` / `DeviceDisconnected` | Defined in types |
| **Event emission from health checks** | **None** |

---

## Target State

- `health.rs` (or a new `health_monitor.rs`) runs a periodic loop (configurable interval, default 30s)
- Each iteration reads system metrics from HAL
- Compares against configurable thresholds from `config/default.toml`
- Emits events when thresholds are exceeded
- Respects throttle defaults (e.g., `CPUSpikeDetected` max once per 10m)
- GPU events emitted when GPU metrics cross thresholds

---

## Subtasks

### 1. Add health monitoring thresholds to config

**File:** `config/default.toml`

```toml
[health_monitor]
enabled = true
check_interval_secs = 30

[health_monitor.thresholds]
cpu_warning_percent = 85
memory_warning_percent = 80
disk_warning_percent = 85
disk_critical_percent = 95
gpu_vram_warning_percent = 90
```

**File:** `crates/agentos-kernel/src/config.rs`

Add the corresponding config struct:

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct HealthMonitorConfig {
    pub enabled: bool,
    pub check_interval_secs: u64,
    pub thresholds: HealthThresholds,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HealthThresholds {
    pub cpu_warning_percent: f32,
    pub memory_warning_percent: f32,
    pub disk_warning_percent: f32,
    pub disk_critical_percent: f32,
    pub gpu_vram_warning_percent: f32,
}
```

### 2. Implement the health monitoring loop

**File:** `crates/agentos-kernel/src/health.rs` (extend existing, or create `health_monitor.rs`)

The loop runs as a supervised task in the kernel run loop (similar to how EventDispatcher runs):

```rust
pub async fn run_health_monitor(
    kernel: Arc<Kernel>,
    cancellation: CancellationToken,
) {
    let config = kernel.config.health_monitor.clone();
    if !config.enabled {
        return;
    }

    let interval = Duration::from_secs(config.check_interval_secs);
    let thresholds = config.thresholds;

    loop {
        tokio::select! {
            _ = cancellation.cancelled() => break,
            _ = tokio::time::sleep(interval) => {
                check_system_health(&kernel, &thresholds).await;
            }
        }
    }
}

async fn check_system_health(kernel: &Kernel, thresholds: &HealthThresholds) {
    // 1. Read CPU usage from HAL
    let cpu = kernel.hal.read().await.get_cpu_usage().await;
    if cpu > thresholds.cpu_warning_percent {
        kernel.emit_event(
            EventType::CPUSpikeDetected,
            EventSource::HardwareAbstractionLayer,
            EventSeverity::Warning,
            serde_json::json!({
                "cpu_percent": cpu,
                "threshold": thresholds.cpu_warning_percent,
            }),
            0,
        ).await;
    }

    // 2. Read memory usage from HAL
    let mem = kernel.hal.read().await.get_memory_usage().await;
    if mem > thresholds.memory_warning_percent {
        kernel.emit_event(
            EventType::MemoryPressure,
            EventSource::HardwareAbstractionLayer,
            EventSeverity::Warning,
            serde_json::json!({
                "memory_percent": mem,
                "threshold": thresholds.memory_warning_percent,
            }),
            0,
        ).await;
    }

    // 3. Read disk usage from HAL
    let disk = kernel.hal.read().await.get_disk_usage().await;
    if disk > thresholds.disk_critical_percent {
        kernel.emit_event(
            EventType::DiskSpaceCritical,
            EventSource::HardwareAbstractionLayer,
            EventSeverity::Critical,
            serde_json::json!({
                "disk_percent": disk,
                "threshold": thresholds.disk_critical_percent,
            }),
            0,
        ).await;
    } else if disk > thresholds.disk_warning_percent {
        kernel.emit_event(
            EventType::DiskSpaceLow,
            EventSource::HardwareAbstractionLayer,
            EventSeverity::Warning,
            serde_json::json!({
                "disk_percent": disk,
                "threshold": thresholds.disk_warning_percent,
            }),
            0,
        ).await;
    }

    // 4. Check GPU if available
    if let Ok(gpu) = kernel.hal.read().await.get_gpu_info().await {
        if gpu.vram_usage_percent > thresholds.gpu_vram_warning_percent {
            kernel.emit_event(
                EventType::GPUMemoryPressure,
                EventSource::HardwareAbstractionLayer,
                EventSeverity::Warning,
                serde_json::json!({
                    "gpu_vram_percent": gpu.vram_usage_percent,
                    "threshold": thresholds.gpu_vram_warning_percent,
                }),
                0,
            ).await;
        }
    }
}
```

### 3. Spawn the health monitor in `run_loop.rs`

**File:** `crates/agentos-kernel/src/run_loop.rs`

Add a 6th supervised task for the health monitor:

```rust
// Alongside the EventDispatcher task:
let health_handle = {
    let kernel = Arc::clone(&kernel);
    let cancel = cancellation.clone();
    tokio::spawn(async move {
        run_health_monitor(kernel, cancel).await;
    })
};
```

### 4. Add `ProcessCrashed` emission (optional, stretch goal)

**File:** `crates/agentos-kernel/src/health.rs`

If the HAL or kernel tracks monitored processes (e.g., child processes from `shell-exec` tool), emit `ProcessCrashed` when a monitored PID exits unexpectedly. This may require a process monitoring subsystem that doesn't exist yet — if so, defer to a future phase and document as a known gap.

---

## Files Changed

| File | Change |
|------|--------|
| `config/default.toml` | Add `[health_monitor]` section with thresholds |
| `crates/agentos-kernel/src/config.rs` | Add `HealthMonitorConfig` and `HealthThresholds` structs |
| `crates/agentos-kernel/src/health.rs` | Implement `run_health_monitor()` and `check_system_health()` |
| `crates/agentos-kernel/src/run_loop.rs` | Spawn health monitor as supervised task |

---

## Dependencies

None — can be done in parallel with all other phases. The HAL crate already provides system metrics.

---

## Test Plan

1. **Threshold exceeded test:** Mock HAL to return 90% CPU, verify `CPUSpikeDetected` event is emitted.

2. **Below threshold test:** Mock HAL to return 50% CPU, verify no event.

3. **Disk critical vs warning test:** Mock 96% disk → `DiskSpaceCritical`. Mock 88% disk → `DiskSpaceLow`. Mock 50% disk → no event.

4. **Config disabled test:** Set `health_monitor.enabled = false`, verify no monitoring loop runs.

5. **Throttle integration test:** Emit `CPUSpikeDetected`, create a subscription with `max_once_per:10m` throttle, verify the agent is not triggered again within 10 minutes even if CPU stays high.

---

## Verification

```bash
cargo build -p agentos-kernel
cargo test -p agentos-kernel

grep -n "CPUSpikeDetected" crates/agentos-kernel/src/health.rs
grep -n "MemoryPressure" crates/agentos-kernel/src/health.rs
grep -n "DiskSpaceLow\|DiskSpaceCritical" crates/agentos-kernel/src/health.rs
grep -n "health_monitor" config/default.toml
```

---

## Related

- [[Event Trigger Completion Plan]] — Master plan
- [[09-remaining-trigger-prompts]] — Phase 09 includes the CPUSpikeDetected prompt
- [[agentos-event-trigger-system]] — Original spec §3 (SystemHealth, HardwareEvents categories)
