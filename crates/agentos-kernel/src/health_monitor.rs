use crate::config::HealthThresholds;
use crate::kernel::Kernel;
use agentos_audit::AuditLog;
use agentos_types::{EventSeverity, EventSource, EventType, PermissionEntry, PermissionSet};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Minimum interval between emissions of the same event key (debounce).
const DEBOUNCE_INTERVAL_SECS: i64 = 600; // 10 minutes

/// Run a periodic health monitoring loop that emits system health events.
///
/// Reads CPU, memory, disk, and GPU metrics from the HAL and emits typed
/// events when thresholds are exceeded. Debounces emissions so each event
/// type fires at most once per 10 minutes, even if the threshold stays exceeded.
/// Debounce state is persisted to the audit SQLite database so the window
/// survives kernel restarts.
pub async fn run_health_monitor(kernel: Arc<Kernel>, cancellation: CancellationToken) {
    let config = kernel.config.health_monitor.clone();
    if !config.enabled {
        tracing::debug!("Health monitor disabled; running watchdog-only loop");
        // The health monitor is disabled, but we must still ping the systemd
        // watchdog so the process is not killed by WatchdogSec.  Use the same
        // interval as the enabled path so the cadence is predictable.
        let interval = Duration::from_secs(config.check_interval_secs.max(1));
        loop {
            tokio::select! {
                _ = cancellation.cancelled() => break,
                _ = tokio::time::sleep(interval) => {
                    crate::sd_notify::notify_watchdog();
                }
            }
        }
        return;
    }

    // Clamp to at least 1 second to prevent a busy-spin if misconfigured.
    let interval = Duration::from_secs(config.check_interval_secs.max(1));
    let thresholds = config.thresholds;
    // Build permissions once; they are static for the lifetime of the monitor.
    let permissions = hal_read_permissions();
    // Clone the audit Arc so check_system_health can borrow kernel and audit independently.
    let audit = Arc::clone(&kernel.audit);
    // Load persisted debounce state so the 10-minute window survives restarts.
    let mut last_emitted: HashMap<String, DateTime<Utc>> = match audit.load_health_debounce() {
        Ok((map, skipped)) => {
            for key in &skipped {
                tracing::warn!(
                    key = %key,
                    "Health debounce: skipping row with unparseable timestamp; \
                     debounce window for this key will reset"
                );
            }
            map
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "Failed to load health debounce state; starting fresh"
            );
            HashMap::new()
        }
    };

    loop {
        tokio::select! {
            _ = cancellation.cancelled() => break,
            _ = tokio::time::sleep(interval) => {
                check_system_health(&kernel, &thresholds, &permissions, &mut last_emitted, &audit).await;
                // Ping the systemd watchdog after each successful cycle so systemd
                // can distinguish hangs from crashes.  No-op outside systemd.
                crate::sd_notify::notify_watchdog();
            }
        }
    }
}

/// Build a minimal read-only PermissionSet that allows the kernel to query
/// the system, GPU, network, and sensor HAL drivers internally.
fn hal_read_permissions() -> PermissionSet {
    let mut ps = PermissionSet::new();
    for resource in &[
        "hardware.system",
        "hardware.gpu",
        "hardware.network",
        "hardware.sensor",
    ] {
        ps.entries.push(PermissionEntry {
            resource: resource.to_string(),
            read: true,
            write: false,
            execute: false,
            query: false,
            observe: false,
            expires_at: None,
        });
    }
    ps
}

/// Check whether enough time has elapsed since the last emission of the given key.
/// If so, update the in-memory timestamp, persist it to the audit DB, and return true;
/// otherwise return false.
/// For single-instance events, pass the event type name (e.g., "CPUSpikeDetected").
/// For per-device events, pass a compound key (e.g., "GPUMemoryPressure:rtx4090").
fn should_emit(
    last_emitted: &mut HashMap<String, DateTime<Utc>>,
    key: &str,
    audit: &AuditLog,
) -> bool {
    let now = Utc::now();
    let debounce = chrono::Duration::seconds(DEBOUNCE_INTERVAL_SECS);
    match last_emitted.get(key) {
        Some(last) if now - *last < debounce => false,
        _ => {
            last_emitted.insert(key.to_string(), now);
            if let Err(e) = audit.save_health_debounce(key, now) {
                tracing::warn!(
                    error = %e,
                    key = %key,
                    "Failed to persist health debounce state"
                );
            }
            true
        }
    }
}

async fn check_system_health(
    kernel: &Kernel,
    thresholds: &HealthThresholds,
    permissions: &PermissionSet,
    last_emitted: &mut HashMap<String, DateTime<Utc>>,
    audit: &AuditLog,
) {
    // ── 1. System snapshot: CPU / memory / disk ─────────────────────────────
    let system_snapshot = kernel
        .hal
        .query("system", serde_json::Value::Null, permissions)
        .await;
    match &system_snapshot {
        Ok(snapshot) => {
            // CPU
            if let Some(cpu) = snapshot.get("cpu_usage_percent").and_then(|v| v.as_f64()) {
                let cpu = cpu as f32;
                if cpu > thresholds.cpu_warning_percent
                    && should_emit(last_emitted, "CPUSpikeDetected", audit)
                {
                    kernel
                        .emit_event(
                            EventType::CPUSpikeDetected,
                            EventSource::HardwareAbstractionLayer,
                            EventSeverity::Warning,
                            serde_json::json!({
                                "cpu_percent": cpu,
                                "threshold": thresholds.cpu_warning_percent,
                            }),
                            0,
                        )
                        .await;
                }
            }

            // Memory — compute percent from total / used fields
            let mem_total = snapshot
                .get("memory_total_mb")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let mem_used = snapshot
                .get("memory_used_mb")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            if mem_total > 0 {
                let mem_percent = (mem_used as f32 / mem_total as f32) * 100.0;
                if mem_percent > thresholds.memory_warning_percent
                    && should_emit(last_emitted, "MemoryPressure", audit)
                {
                    kernel
                        .emit_event(
                            EventType::MemoryPressure,
                            EventSource::HardwareAbstractionLayer,
                            EventSeverity::Warning,
                            serde_json::json!({
                                "memory_percent": mem_percent,
                                "memory_used_mb": mem_used,
                                "memory_total_mb": mem_total,
                                "threshold": thresholds.memory_warning_percent,
                            }),
                            0,
                        )
                        .await;
                }
            }

            // Disk — collect all affected mounts first, then emit one aggregated event
            // per tier. This prevents N events (one per mount) from flooding the audit log
            // each cycle. Use saturating_sub to guard against filesystems where
            // available_space_bytes can exceed total_space_bytes (btrfs, ZFS, NFS with
            // compression), which would otherwise cause u64 underflow.
            if let Some(disks) = snapshot.get("disk_usage").and_then(|d| d.as_array()) {
                let mut critical_mounts: Vec<serde_json::Value> = Vec::new();
                let mut low_mounts: Vec<serde_json::Value> = Vec::new();

                for disk in disks {
                    let total = disk
                        .get("total_space_bytes")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let available = disk
                        .get("available_space_bytes")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let mount = disk
                        .get("mount_point")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");

                    if total == 0 {
                        continue;
                    }

                    let used = total.saturating_sub(available);
                    let used_percent = (used as f32 / total as f32) * 100.0;

                    if used_percent > thresholds.disk_critical_percent {
                        critical_mounts.push(serde_json::json!({
                            "mount_point": mount,
                            "disk_percent": used_percent,
                            "threshold": thresholds.disk_critical_percent,
                        }));
                    } else if used_percent > thresholds.disk_warning_percent {
                        low_mounts.push(serde_json::json!({
                            "mount_point": mount,
                            "disk_percent": used_percent,
                            "threshold": thresholds.disk_warning_percent,
                        }));
                    }
                }

                // One event per tier per cycle — debounced on the aggregate key.
                // Trade-off: if all mounts become healthy and then one goes critical
                // again within the same 10-minute window, the re-occurrence is
                // suppressed until the window expires. This is intentional: the
                // aggregate key ("DiskSpaceCritical") debounces the tier as a whole,
                // not individual mounts. The 10-minute window limits audit log noise
                // at the cost of up to 10 minutes' delay for a re-triggered event.
                if !critical_mounts.is_empty()
                    && should_emit(last_emitted, "DiskSpaceCritical", audit)
                {
                    kernel
                        .emit_event(
                            EventType::DiskSpaceCritical,
                            EventSource::HardwareAbstractionLayer,
                            EventSeverity::Critical,
                            serde_json::json!({ "mounts": critical_mounts }),
                            0,
                        )
                        .await;
                }
                if !low_mounts.is_empty() && should_emit(last_emitted, "DiskSpaceLow", audit) {
                    kernel
                        .emit_event(
                            EventType::DiskSpaceLow,
                            EventSource::HardwareAbstractionLayer,
                            EventSeverity::Warning,
                            serde_json::json!({ "mounts": low_mounts }),
                            0,
                        )
                        .await;
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "Health monitor: failed to query system HAL driver");
        }
    }

    // ── 2. GPU VRAM — optional, silently skipped when no GPU / no VRAM data ─
    if let Ok(gpu_json) = kernel
        .hal
        .query("gpu", serde_json::json!({"action": "list"}), permissions)
        .await
    {
        if let Some(devices) = gpu_json.get("devices").and_then(|d| d.as_array()) {
            for device in devices {
                let vram_total = device
                    .get("vram_total_mb")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let vram_used = device
                    .get("vram_used_mb")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let name = device
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");

                if vram_total == 0 {
                    continue;
                }

                let vram_percent = (vram_used as f32 / vram_total as f32) * 100.0;
                if vram_percent > thresholds.gpu_vram_warning_percent
                    && should_emit(last_emitted, &format!("GPUMemoryPressure:{}", name), audit)
                {
                    kernel
                        .emit_event(
                            EventType::GPUMemoryPressure,
                            EventSource::HardwareAbstractionLayer,
                            EventSeverity::Warning,
                            serde_json::json!({
                                "gpu_name": name,
                                "gpu_vram_percent": vram_percent,
                                "vram_used_mb": vram_used,
                                "vram_total_mb": vram_total,
                                "threshold": thresholds.gpu_vram_warning_percent,
                            }),
                            0,
                        )
                        .await;
                }

                // Emit GPUAvailable when a GPU with VRAM is detected — keyed per GPU
                if should_emit(last_emitted, &format!("GPUAvailable:{}", name), audit) {
                    kernel
                        .emit_event(
                            EventType::GPUAvailable,
                            EventSource::HardwareAbstractionLayer,
                            EventSeverity::Info,
                            serde_json::json!({
                                "gpu_name": name,
                                "vram_total_mb": vram_total,
                            }),
                            0,
                        )
                        .await;
                }
            }
        }
    }

    // ── 3. Network interfaces — check for downed interfaces ──────────────────
    if let Ok(net_json) = kernel
        .hal
        .query(
            "network",
            serde_json::json!({"action": "list"}),
            permissions,
        )
        .await
    {
        if let Some(interfaces) = net_json.get("interfaces").and_then(|n| n.as_array()) {
            for iface in interfaces {
                let name = iface
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let is_up = iface.get("is_up").and_then(|v| v.as_bool()).unwrap_or(true);
                if !is_up
                    && should_emit(
                        last_emitted,
                        &format!("NetworkInterfaceDown:{}", name),
                        audit,
                    )
                {
                    kernel
                        .emit_event(
                            EventType::NetworkInterfaceDown,
                            EventSource::HardwareAbstractionLayer,
                            EventSeverity::Warning,
                            serde_json::json!({
                                "interface": name,
                            }),
                            0,
                        )
                        .await;
                }
            }
        }
    }

    // ── 4. Container resource quota — check cgroup memory limits ─────────────
    // Reuse the system snapshot from section 1 instead of querying again.
    if let Ok(snapshot) = &system_snapshot {
        if let Some(cgroup) = snapshot.get("cgroup") {
            let mem_limit = cgroup
                .get("memory_limit_bytes")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let mem_usage = cgroup
                .get("memory_usage_bytes")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            if mem_limit > 0 {
                let usage_pct = (mem_usage as f32 / mem_limit as f32) * 100.0;
                if usage_pct > 95.0
                    && should_emit(last_emitted, "ContainerResourceQuotaExceeded", audit)
                {
                    kernel
                        .emit_event(
                            EventType::ContainerResourceQuotaExceeded,
                            EventSource::HardwareAbstractionLayer,
                            EventSeverity::Critical,
                            serde_json::json!({
                                "resource": "memory",
                                "usage_percent": usage_pct,
                                "limit_bytes": mem_limit,
                                "usage_bytes": mem_usage,
                            }),
                            0,
                        )
                        .await;
                }
            }
        }
    }

    // ── 5. Sensor readings — check for threshold exceedances ─────────────────
    if let Ok(sensor_json) = kernel
        .hal
        .query("sensor", serde_json::json!({"action": "list"}), permissions)
        .await
    {
        if let Some(readings) = sensor_json.get("readings").and_then(|r| r.as_array()) {
            for reading in readings {
                let name = reading
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let value = reading.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let threshold = reading.get("threshold").and_then(|v| v.as_f64());
                if let Some(thresh) = threshold {
                    if value > thresh
                        && should_emit(
                            last_emitted,
                            &format!("SensorReadingThresholdExceeded:{}", name),
                            audit,
                        )
                    {
                        kernel
                            .emit_event(
                                EventType::SensorReadingThresholdExceeded,
                                EventSource::HardwareAbstractionLayer,
                                EventSeverity::Warning,
                                serde_json::json!({
                                    "sensor_name": name,
                                    "value": value,
                                    "threshold": thresh,
                                }),
                                0,
                            )
                            .await;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::PermissionOp;

    #[test]
    fn hal_read_permissions_grants_hardware_system_read() {
        let ps = hal_read_permissions();
        assert!(ps.check("hardware.system", PermissionOp::Read));
        assert!(!ps.check("hardware.system", PermissionOp::Write));
        assert!(ps.check("hardware.gpu", PermissionOp::Read));
        assert!(!ps.check("hardware.gpu", PermissionOp::Write));
        // Must not grant unrelated resources
        assert!(!ps.check("fs:/etc/passwd", PermissionOp::Read));
    }

    #[test]
    fn should_emit_allows_first_call_and_debounces_second() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let audit = AuditLog::open(tmp.path()).unwrap();
        let mut state: HashMap<String, DateTime<Utc>> = HashMap::new();

        // First call — no prior state, must emit.
        assert!(should_emit(&mut state, "DiskSpaceLow", &audit));
        // Immediate second call — debounce window active, must not emit.
        assert!(!should_emit(&mut state, "DiskSpaceLow", &audit));
        // Different key — independent debounce, must emit.
        assert!(should_emit(&mut state, "DiskSpaceCritical", &audit));
    }

    #[test]
    fn debounce_persists_across_reload() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let audit = AuditLog::open(tmp.path()).unwrap();
        let mut state: HashMap<String, DateTime<Utc>> = HashMap::new();

        // Emit once so the timestamp is persisted.
        assert!(should_emit(&mut state, "DiskSpaceLow", &audit));

        // Simulate a restart: load state from DB into a fresh in-memory map.
        let (loaded, skipped) = audit.load_health_debounce().unwrap();
        assert!(skipped.is_empty(), "no rows should have bad timestamps");
        assert!(
            loaded.contains_key("DiskSpaceLow"),
            "persisted key must survive DB round-trip"
        );

        let mut reloaded_state = loaded;
        // The debounce window is still active — must not emit again.
        assert!(!should_emit(&mut reloaded_state, "DiskSpaceLow", &audit));
    }

    #[test]
    fn disk_aggregation_collects_all_mounts() {
        // Verify that the aggregation logic correctly separates mounts by tier.
        // This is a pure data test — it does not require a running kernel.
        let thresholds = HealthThresholds::default(); // warning=85, critical=95

        let total: u64 = 100_000_000_000; // 100 GB
        let make_disk = |used_pct: f32| {
            let used = (total as f32 * used_pct / 100.0) as u64;
            let available = total.saturating_sub(used);
            serde_json::json!({
                "mount_point": format!("/mnt/disk_{}", used_pct as u32),
                "total_space_bytes": total,
                "available_space_bytes": available,
            })
        };

        // 3 mounts in warning tier, 2 in critical tier, 1 healthy
        let disks = vec![
            make_disk(88.0), // warning
            make_disk(91.0), // warning
            make_disk(87.0), // warning
            make_disk(96.0), // critical
            make_disk(99.0), // critical
            make_disk(50.0), // healthy — should not appear
        ];

        let mut critical_mounts: Vec<serde_json::Value> = Vec::new();
        let mut low_mounts: Vec<serde_json::Value> = Vec::new();

        for disk in &disks {
            let total_bytes = disk
                .get("total_space_bytes")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let available = disk
                .get("available_space_bytes")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            if total_bytes == 0 {
                continue;
            }
            let used = total_bytes.saturating_sub(available);
            let used_pct = (used as f32 / total_bytes as f32) * 100.0;
            if used_pct > thresholds.disk_critical_percent {
                critical_mounts.push(disk.clone());
            } else if used_pct > thresholds.disk_warning_percent {
                low_mounts.push(disk.clone());
            }
        }

        assert_eq!(low_mounts.len(), 3, "expected 3 warning-tier mounts");
        assert_eq!(critical_mounts.len(), 2, "expected 2 critical-tier mounts");
    }
}
