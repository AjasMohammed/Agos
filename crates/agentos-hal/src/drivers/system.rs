use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::Value;
use std::path::Path;
use std::sync::Mutex;
use sysinfo::{CpuRefreshKind, Disks, MemoryRefreshKind, RefreshKind, System};

use crate::hal::HalDriver;
use crate::types::{DiskInfo, SystemSnapshot};

/// `snapshot` reads memory, swap and CPU only. `refresh_all` would also walk
/// every process and thread in /proc (with environ) on each API call.
fn system_refresh() -> RefreshKind {
    RefreshKind::nothing()
        .with_memory(MemoryRefreshKind::everything())
        .with_cpu(CpuRefreshKind::everything())
}

pub struct SystemDriver {
    sys: Mutex<System>,
}

impl Default for SystemDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemDriver {
    pub fn new() -> Self {
        Self {
            sys: Mutex::new(System::new_with_specifics(system_refresh())),
        }
    }

    /// Charge and charging state of the first system battery, straight from
    /// sysfs — `sysinfo` does not expose power supplies.
    ///
    /// Anything unreadable degrades to `None`: a desktop has no battery, and
    /// that is not an error. The two halves are independent — a driver that
    /// exports `capacity` but not `status` (some ACPI quirk laptops) yields a
    /// charge with no state, and vice versa.
    ///
    /// A host with several batteries reports only the first, where `upower`
    /// and `acpi` would aggregate. Slice batteries are rare enough that the
    /// simpler reading is worth more than the accurate sum.
    fn read_battery() -> (Option<u8>, Option<String>) {
        Self::read_battery_from(Path::new("/sys/class/power_supply/"))
    }

    /// Root-parameterised so the peripheral and ordering rules below can be
    /// tested against a tempdir — on a build machine with no battery the real
    /// path exercises nothing at all.
    ///
    /// Peripherals advertise `type=Battery` too (a wireless mouse, a headset).
    /// Most mark themselves `scope=Device`, but a real system battery often
    /// exports no `scope` file whatsoever, so "no scope" cannot mean "not a
    /// system battery". `BAT*`-named supplies are therefore preferred outright
    /// and everything else is a fallback, rather than leaning on the accident
    /// that peripheral drivers happen to use lowercase names that sort later.
    fn read_battery_from(root: &Path) -> (Option<u8>, Option<String>) {
        let Ok(entries) = std::fs::read_dir(root) else {
            return (None, None);
        };
        let mut supplies: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        // read_dir order is arbitrary: BAT* first, then by name so BAT0 beats BAT1.
        supplies.sort_by_key(|p| {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            (!name.starts_with("BAT"), name)
        });

        for path in supplies {
            let field = |name: &str| {
                std::fs::read_to_string(path.join(name))
                    .ok()
                    .map(|s| s.trim().to_string())
            };
            if field("type").as_deref() != Some("Battery") {
                continue;
            }
            if field("scope").as_deref() == Some("Device") {
                continue;
            }
            // Parsed as i32, not u8: a driver reporting the ACPI "unknown"
            // sentinel (-1) or a bogus 255 should still report its charging
            // state, rather than failing the parse and falling through to
            // whatever supply comes next.
            let percent = field("capacity")
                .and_then(|c| c.parse::<i32>().ok())
                .filter(|p| (0..=100).contains(p))
                .map(|p| p as u8);
            let status = field("status");
            if percent.is_none() && status.is_none() {
                continue; // a `type=Battery` directory with nothing readable in it
            }
            return (percent, status);
        }
        (None, None)
    }

    pub fn snapshot(&self) -> Result<SystemSnapshot, AgentOSError> {
        let mut sys = self.sys.lock().unwrap();
        sys.refresh_specifics(system_refresh());
        let disks = Disks::new_with_refreshed_list();

        let cpu_usage_percent = sys.global_cpu_usage();
        let cpu_core_count = sys.cpus().len();

        let memory_total_mb = sys.total_memory() / 1024 / 1024;
        let memory_used_mb = sys.used_memory() / 1024 / 1024;
        let memory_available_mb = sys.available_memory() / 1024 / 1024;

        let swap_total_mb = sys.total_swap() / 1024 / 1024;
        let swap_used_mb = sys.used_swap() / 1024 / 1024;

        let uptime_seconds = System::uptime();
        let os_name = System::name().unwrap_or_else(|| "Unknown".to_string());
        let os_version = System::os_version().unwrap_or_else(|| "Unknown".to_string());
        let hostname = System::host_name().unwrap_or_else(|| "Unknown".to_string());

        let load_average = {
            let load = System::load_average();
            (load.one, load.five, load.fifteen)
        };

        let (battery_percent, battery_status) = Self::read_battery();

        let mut disk_usage = Vec::new();
        for disk in &disks {
            disk_usage.push(DiskInfo {
                name: disk.name().to_string_lossy().to_string(),
                mount_point: disk.mount_point().to_string_lossy().to_string(),
                total_space_bytes: disk.total_space(),
                available_space_bytes: disk.available_space(),
                file_system: String::from_utf8_lossy(disk.file_system().as_encoded_bytes())
                    .to_string(),
            });
        }

        Ok(SystemSnapshot {
            cpu_usage_percent,
            cpu_core_count,
            memory_total_mb,
            memory_used_mb,
            memory_available_mb,
            swap_total_mb,
            swap_used_mb,
            uptime_seconds,
            os_name,
            os_version,
            hostname,
            load_average,
            disk_usage,
            battery_percent,
            battery_status,
        })
    }
}

#[async_trait]
impl HalDriver for SystemDriver {
    fn name(&self) -> &str {
        "system"
    }

    fn required_permission(&self) -> (&str, PermissionOp) {
        ("hardware.system", PermissionOp::Read)
    }

    async fn query(&self, _params: Value) -> Result<Value, AgentOSError> {
        let snapshot = self.snapshot()?;
        Ok(serde_json::to_value(snapshot).map_err(|e| AgentOSError::HalError(e.to_string()))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_system_snapshot_has_required_fields() {
        let driver = SystemDriver::new();
        let snapshot: SystemSnapshot = driver.snapshot().unwrap();
        assert!(snapshot.cpu_core_count > 0);
        assert!(snapshot.memory_total_mb > 0);
    }

    /// Runs on desktops (no battery) and laptops alike, so the only invariant
    /// that holds everywhere is the range. `status` is deliberately not
    /// asserted: a driver may export `capacity` without it.
    #[test]
    fn battery_is_absent_or_a_valid_percentage() {
        let (percent, _status) = SystemDriver::read_battery();
        if let Some(p) = percent {
            assert!(p <= 100, "battery_percent out of range: {p}");
        }
    }

    fn supply(root: &Path, name: &str, fields: &[(&str, &str)]) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        for (file, value) in fields {
            std::fs::write(dir.join(file), format!("{value}\n")).unwrap();
        }
    }

    /// The whole point of the type/scope/name rules: a wireless mouse also
    /// reports `type=Battery`, and picking it would have the laptop announce
    /// its mouse's charge as its own.
    #[test]
    fn peripheral_batteries_lose_to_the_system_battery() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        supply(root, "ACAD", &[("type", "Mains")]);
        // Sorts before BAT1 by name, and exports no `scope` — only the `BAT`
        // preference keeps it from winning.
        supply(
            root,
            "AAA_mouse",
            &[("type", "Battery"), ("capacity", "12"), ("status", "Full")],
        );
        supply(
            root,
            "hidpp_battery_0",
            &[
                ("type", "Battery"),
                ("scope", "Device"),
                ("capacity", "5"),
                ("status", "Discharging"),
            ],
        );
        supply(
            root,
            "BAT1",
            &[
                ("type", "Battery"),
                ("capacity", "89"),
                ("status", "Charging"),
            ],
        );

        let (percent, status) = SystemDriver::read_battery_from(root);
        assert_eq!(percent, Some(89));
        assert_eq!(status.as_deref(), Some("Charging"));
    }

    #[test]
    fn battery_absent_or_unreadable_is_none_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            SystemDriver::read_battery_from(&tmp.path().join("nonexistent")),
            (None, None)
        );
        // An empty dir, and a Mains-only host (a desktop), both report nothing.
        supply(tmp.path(), "ACAD", &[("type", "Mains"), ("online", "1")]);
        assert_eq!(SystemDriver::read_battery_from(tmp.path()), (None, None));
    }

    /// `capacity` and `status` are independent: an ACPI quirk driver exporting
    /// one without the other must still report the half it has.
    #[test]
    fn half_readable_battery_reports_the_half_it_has() {
        let tmp = tempfile::tempdir().unwrap();
        supply(
            tmp.path(),
            "BAT0",
            &[("type", "Battery"), ("capacity", "42")],
        );
        assert_eq!(
            SystemDriver::read_battery_from(tmp.path()),
            (Some(42), None)
        );

        let tmp2 = tempfile::tempdir().unwrap();
        supply(
            tmp2.path(),
            "BAT0",
            &[("type", "Battery"), ("status", "Discharging")],
        );
        let (percent, status) = SystemDriver::read_battery_from(tmp2.path());
        assert_eq!(percent, None);
        assert_eq!(status.as_deref(), Some("Discharging"));
    }

    /// -1 is the ACPI "unknown" sentinel; without the range filter it would
    /// either fail the parse or wrap into a plausible-looking percentage.
    #[test]
    fn out_of_range_capacity_is_dropped_but_status_survives() {
        for bogus in ["-1", "255", "not-a-number"] {
            let tmp = tempfile::tempdir().unwrap();
            supply(
                tmp.path(),
                "BAT0",
                &[("type", "Battery"), ("capacity", bogus), ("status", "Full")],
            );
            let (percent, status) = SystemDriver::read_battery_from(tmp.path());
            assert_eq!(percent, None, "capacity {bogus} should not be reported");
            assert_eq!(status.as_deref(), Some("Full"));
        }
    }
}
