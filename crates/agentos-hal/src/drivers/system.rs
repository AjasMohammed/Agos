use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Mutex;
use sysinfo::{Disks, System};

use crate::hal::HalDriver;
use crate::types::{DiskInfo, SystemSnapshot};

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
            sys: Mutex::new(System::new_all()),
        }
    }

    pub fn snapshot(&self) -> Result<SystemSnapshot, AgentOSError> {
        let mut sys = self.sys.lock().unwrap();
        sys.refresh_all();
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
}
