use crate::config::HealthThresholds;
use crate::kernel::Kernel;
use agentos_audit::AuditLog;
use agentos_types::{EventSeverity, EventSource, EventType, PermissionEntry, PermissionSet};
use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Minimum interval before a *still-active* condition re-announces itself.
///
/// Every health event in this file is a level condition ("disk is above 85%"),
/// not a discrete occurrence, so it is edge-triggered: it fires when the
/// condition becomes true and stays silent while it holds. This interval is the
/// safety valve so a condition that never recovers is not forgotten forever —
/// it re-alerts at most once every 6 hours. Before edge-triggering, a 10-minute
/// debounce meant a full disk re-fired 144 times a day, and each re-fire spawned
/// an agent task downstream.
const REALERT_INTERVAL_SECS: i64 = 21_600; // 6 hours

/// Run a periodic health monitoring loop that emits system health events.
///
/// Reads CPU, memory, disk, and GPU metrics from the HAL and emits typed
/// events when thresholds are exceeded. Emissions are edge-triggered: an event
/// fires when its condition first becomes true, stays silent while the
/// condition holds (re-alerting at most once per [`REALERT_INTERVAL_SECS`]),
/// and re-arms silently when the condition clears.
/// Emission timestamps are persisted to the audit SQLite database and the
/// latch set is re-seeded from them at boot (see
/// [`seed_active_from_persisted`]), so a restart does not re-announce a
/// condition that was already latched.
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
    // Load persisted emission timestamps so the re-alert window survives restarts.
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

    // Conditions currently latched as true, re-seeded from the persisted
    // emission timestamps so a restart does not re-announce a held condition.
    let mut active: HashSet<String> = seed_active_from_persisted(&last_emitted, Utc::now());
    // GPUs already announced this boot. "A GPU exists" is a static fact, not a
    // condition — announce once, never re-alert, never persist.
    let mut announced_gpus: HashSet<String> = HashSet::new();

    loop {
        tokio::select! {
            _ = cancellation.cancelled() => break,
            _ = tokio::time::sleep(interval) => {
                check_system_health(
                    &kernel,
                    &thresholds,
                    &permissions,
                    &mut active,
                    &mut announced_gpus,
                    &mut last_emitted,
                    &audit,
                )
                .await;
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

/// Re-latch, at boot, every condition whose last emission is still inside the
/// re-alert window.
///
/// The latch set is in-memory, so without this the first tick after a restart
/// takes the *unlatched* branch and emits regardless of the persisted
/// timestamp: under systemd `Restart=always` a full disk yields one
/// `DiskSpaceLow` — and one downstream agent task — per boot. An emission older
/// than the window would have re-alerted anyway, so it is left unlatched.
/// Anything that recovered while the kernel was down is cleared by the first
/// tick's `false` branch.
fn seed_active_from_persisted(
    last_emitted: &HashMap<String, DateTime<Utc>>,
    now: DateTime<Utc>,
) -> HashSet<String> {
    let realert = chrono::Duration::seconds(REALERT_INTERVAL_SECS);
    last_emitted
        .iter()
        .filter(|(_, emitted_at)| now - **emitted_at < realert)
        .map(|(key, _)| key.clone())
        .collect()
}

/// Edge-trigger a level condition. Returns true only when the event should be emitted.
///
/// * condition true, not latched → latch it and emit.
/// * condition true, already latched → emit only if [`REALERT_INTERVAL_SECS`] elapsed.
/// * condition false → clear the latch (silently — there is no recovery event) so
///   the next transition to true emits again.
///
/// MUST be called on every tick for every known key, with the *current* value of
/// the condition, or a recovery is never observed and the latch sticks forever.
///
/// For single-instance events, pass the event type name (e.g., "CPUSpikeDetected").
/// For per-device events, pass a compound key (e.g., "GPUMemoryPressure:rtx4090").
/// ponytail: a key that is never passed again — an unplugged GPU, a removed
/// mount, or a legacy aggregate key re-seeded from an old debounce row — stays
/// latched. Bounded by device count, so not worth a reaper.
fn should_emit_condition(
    active: &mut HashSet<String>,
    last_emitted: &mut HashMap<String, DateTime<Utc>>,
    key: &str,
    condition: bool,
    audit: &AuditLog,
) -> bool {
    if !condition {
        active.remove(key);
        return false;
    }

    let now = Utc::now();
    if !active.insert(key.to_string()) {
        // Already latched — only the re-alert valve can let this through.
        if let Some(last) = last_emitted.get(key) {
            if now - *last < chrono::Duration::seconds(REALERT_INTERVAL_SECS) {
                return false;
            }
        }
    }

    last_emitted.insert(key.to_string(), now);
    if let Err(e) = audit.save_health_debounce(key, now) {
        tracing::warn!(
            error = %e,
            key = %key,
            "Failed to persist health emission timestamp"
        );
    }
    true
}

#[allow(clippy::too_many_arguments)]
async fn check_system_health(
    kernel: &Kernel,
    thresholds: &HealthThresholds,
    permissions: &PermissionSet,
    active: &mut HashSet<String>,
    announced_gpus: &mut HashSet<String>,
    last_emitted: &mut HashMap<String, DateTime<Utc>>,
    audit: &AuditLog,
) {
    // ── 1. System snapshot: CPU / memory / disk ─────────────────────────────
    let system_snapshot = kernel
        .hal
        .query("system", serde_json::Value::Null, permissions, None, None)
        .await;
    match &system_snapshot {
        Ok(snapshot) => {
            let mut cpu_metric = None;
            let mut memory_metric = None;
            let mut disk_metric = None;
            // CPU
            if let Some(cpu) = snapshot.get("cpu_usage_percent").and_then(|v| v.as_f64()) {
                cpu_metric = Some(cpu);
                let cpu = cpu as f32;
                if should_emit_condition(
                    active,
                    last_emitted,
                    "CPUSpikeDetected",
                    cpu > thresholds.cpu_warning_percent,
                    audit,
                ) {
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
                memory_metric = Some(mem_percent as f64);
                if should_emit_condition(
                    active,
                    last_emitted,
                    "MemoryPressure",
                    mem_percent > thresholds.memory_warning_percent,
                    audit,
                ) {
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
                // Set when at least one mount *newly* latched this tick.
                let mut emit_critical = false;
                let mut emit_low = false;

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
                    disk_metric = Some(
                        disk_metric
                            .map(|current: f64| current.max(used_percent as f64))
                            .unwrap_or(used_percent as f64),
                    );

                    let is_critical = used_percent > thresholds.disk_critical_percent;
                    let is_low = !is_critical && used_percent > thresholds.disk_warning_percent;

                    // Latch per mount, not per tier: with one aggregate key, `/` at
                    // 96% hid `/var` crossing the same threshold hours later until
                    // the 6h re-alert valve opened. Both keys are evaluated for
                    // every mount on every tick, so a mount that recovers — or
                    // moves between tiers — clears the latch it no longer holds.
                    if should_emit_condition(
                        active,
                        last_emitted,
                        &format!("DiskSpaceCritical:{}", mount),
                        is_critical,
                        audit,
                    ) {
                        emit_critical = true;
                    }
                    if should_emit_condition(
                        active,
                        last_emitted,
                        &format!("DiskSpaceLow:{}", mount),
                        is_low,
                        audit,
                    ) {
                        emit_low = true;
                    }

                    if is_critical {
                        critical_mounts.push(serde_json::json!({
                            "mount_point": mount,
                            "disk_percent": used_percent,
                            "threshold": thresholds.disk_critical_percent,
                        }));
                    } else if is_low {
                        low_mounts.push(serde_json::json!({
                            "mount_point": mount,
                            "disk_percent": used_percent,
                            "threshold": thresholds.disk_warning_percent,
                        }));
                    }
                }

                // One aggregate event per tier, fired when any mount newly latched,
                // with a payload listing every mount currently in that tier — one
                // event still says everything, but no crossing waits on the valve.
                if emit_critical {
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
                if emit_low {
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

            kernel
                .otel
                .record_health_snapshot(cpu_metric, memory_metric, disk_metric);
        }
        Err(e) => {
            tracing::warn!(error = %e, "Health monitor: failed to query system HAL driver");
        }
    }

    // ── 2. GPU VRAM — optional, silently skipped when no GPU / no VRAM data ─
    if let Ok(gpu_json) = kernel
        .hal
        .query(
            "gpu",
            serde_json::json!({"action": "list"}),
            permissions,
            None,
            None,
        )
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
                if should_emit_condition(
                    active,
                    last_emitted,
                    &format!("GPUMemoryPressure:{}", name),
                    vram_percent > thresholds.gpu_vram_warning_percent,
                    audit,
                ) {
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

                // "A GPU exists" is a static fact, not a condition: announce each GPU
                // once per boot. HashSet::insert returns false if already announced.
                if announced_gpus.insert(name.to_string()) {
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
            None,
            None,
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
                // A downed interface stays down: level condition, not a one-shot
                // notification. Coming back up clears the latch.
                if should_emit_condition(
                    active,
                    last_emitted,
                    &format!("NetworkInterfaceDown:{}", name),
                    !is_up,
                    audit,
                ) {
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
                if should_emit_condition(
                    active,
                    last_emitted,
                    "ContainerResourceQuotaExceeded",
                    usage_pct > 95.0,
                    audit,
                ) {
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
        .query(
            "sensor",
            serde_json::json!({"action": "list"}),
            permissions,
            None,
            None,
        )
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
                    if should_emit_condition(
                        active,
                        last_emitted,
                        &format!("SensorReadingThresholdExceeded:{}", name),
                        value > thresh,
                        audit,
                    ) {
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

    /// Open a throwaway audit log; the NamedTempFile must stay alive for the
    /// lifetime of the returned log, so hand both back.
    fn test_audit() -> (tempfile::NamedTempFile, AuditLog) {
        let tmp = tempfile::NamedTempFile::new().expect("temp file");
        let audit = AuditLog::open(tmp.path()).expect("open audit log");
        (tmp, audit)
    }

    /// (a) A condition that stays true inside the re-alert window emits exactly once.
    /// This is the whole point of edge-triggering: the old 10-minute debounce
    /// re-fired a full disk 144 times a day, spawning an agent task each time.
    #[test]
    fn holding_condition_emits_once_within_realert_window() {
        let (_tmp, audit) = test_audit();
        let mut active: HashSet<String> = HashSet::new();
        let mut last: HashMap<String, DateTime<Utc>> = HashMap::new();

        assert!(should_emit_condition(
            &mut active,
            &mut last,
            "DiskSpaceLow",
            true,
            &audit
        ));
        for _ in 0..10 {
            assert!(
                !should_emit_condition(&mut active, &mut last, "DiskSpaceLow", true, &audit),
                "a still-true condition must not re-emit inside the re-alert window"
            );
        }
        // A different key latches independently.
        assert!(should_emit_condition(
            &mut active,
            &mut last,
            "DiskSpaceCritical",
            true,
            &audit
        ));
    }

    /// (b) Recovery clears the latch, so the next transition to true emits again.
    #[test]
    fn recovery_rearms_the_condition() {
        let (_tmp, audit) = test_audit();
        let mut active: HashSet<String> = HashSet::new();
        let mut last: HashMap<String, DateTime<Utc>> = HashMap::new();

        assert!(should_emit_condition(
            &mut active,
            &mut last,
            "MemoryPressure",
            true,
            &audit
        ));
        // Condition clears — no recovery event, but the latch must drop.
        assert!(!should_emit_condition(
            &mut active,
            &mut last,
            "MemoryPressure",
            false,
            &audit
        ));
        assert!(!active.contains("MemoryPressure"));
        // Re-occurrence emits immediately, without waiting out the re-alert window.
        assert!(should_emit_condition(
            &mut active,
            &mut last,
            "MemoryPressure",
            true,
            &audit
        ));
    }

    /// (c) A condition that never recovers still re-alerts once the interval passes.
    #[test]
    fn stuck_condition_realerts_after_interval() {
        let (_tmp, audit) = test_audit();
        let mut active: HashSet<String> = HashSet::new();
        let mut last: HashMap<String, DateTime<Utc>> = HashMap::new();

        assert!(should_emit_condition(
            &mut active,
            &mut last,
            "DiskSpaceLow",
            true,
            &audit
        ));
        assert!(!should_emit_condition(
            &mut active,
            &mut last,
            "DiskSpaceLow",
            true,
            &audit
        ));

        // Backdate the last emission past the re-alert interval.
        last.insert(
            "DiskSpaceLow".to_string(),
            Utc::now() - chrono::Duration::seconds(REALERT_INTERVAL_SECS + 1),
        );
        assert!(
            should_emit_condition(&mut active, &mut last, "DiskSpaceLow", true, &audit),
            "a condition stuck past the re-alert interval must announce itself again"
        );
        // ...and the valve closes again right after.
        assert!(!should_emit_condition(
            &mut active,
            &mut last,
            "DiskSpaceLow",
            true,
            &audit
        ));
    }

    /// Emission timestamps round-trip through the audit DB, and the latch set is
    /// re-seeded from them at boot: a condition that was already latched when the
    /// process died stays silent. Before the seeding, systemd `Restart=always`
    /// turned a full disk into one event — and one agent task — per restart.
    #[test]
    fn emission_timestamp_persists_across_reload() {
        let (_tmp, audit) = test_audit();
        let mut active: HashSet<String> = HashSet::new();
        let mut last: HashMap<String, DateTime<Utc>> = HashMap::new();

        assert!(should_emit_condition(
            &mut active,
            &mut last,
            "DiskSpaceLow",
            true,
            &audit
        ));

        let (loaded, skipped) = audit.load_health_debounce().expect("reload debounce state");
        assert!(skipped.is_empty(), "no rows should have bad timestamps");
        assert!(
            loaded.contains_key("DiskSpaceLow"),
            "persisted key must survive DB round-trip"
        );

        // Fresh boot: the latch is re-seeded from the persisted emission, so a
        // condition that is still true stays silent.
        let mut reloaded = loaded;
        let mut fresh_active = seed_active_from_persisted(&reloaded, Utc::now());
        assert!(fresh_active.contains("DiskSpaceLow"));
        assert!(
            !should_emit_condition(
                &mut fresh_active,
                &mut reloaded,
                "DiskSpaceLow",
                true,
                &audit
            ),
            "a condition already latched before the restart must not re-announce"
        );
    }

    /// (d) The boot seed only covers emissions inside the re-alert window: an
    /// older one would have re-alerted anyway, and a condition that recovered
    /// while the kernel was down unlatches on the first tick.
    #[test]
    fn stale_persisted_emission_still_reannounces_after_restart() {
        let (_tmp, audit) = test_audit();
        let now = Utc::now();
        let mut last: HashMap<String, DateTime<Utc>> = HashMap::new();
        last.insert(
            "DiskSpaceLow".to_string(),
            now - chrono::Duration::minutes(5),
        );
        last.insert(
            "DiskSpaceCritical".to_string(),
            now - chrono::Duration::seconds(REALERT_INTERVAL_SECS + 1),
        );

        let mut active = seed_active_from_persisted(&last, now);
        assert!(
            active.contains("DiskSpaceLow"),
            "recent emission re-latches"
        );
        assert!(
            !active.contains("DiskSpaceCritical"),
            "an emission past the re-alert window must not re-latch"
        );
        assert!(should_emit_condition(
            &mut active,
            &mut last,
            "DiskSpaceCritical",
            true,
            &audit
        ));

        // Recovered during downtime: the first tick clears the seeded latch, so
        // the next occurrence emits immediately.
        assert!(!should_emit_condition(
            &mut active,
            &mut last,
            "DiskSpaceLow",
            false,
            &audit
        ));
        assert!(should_emit_condition(
            &mut active,
            &mut last,
            "DiskSpaceLow",
            true,
            &audit
        ));
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
