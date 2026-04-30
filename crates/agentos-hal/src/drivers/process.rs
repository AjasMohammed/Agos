use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Mutex;
use sysinfo::{Pid, System};

use crate::hal::HalDriver;
use crate::types::ProcessEntry;
use chrono::TimeZone;

pub struct ProcessDriver {
    sys: Mutex<System>,
}

impl Default for ProcessDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessDriver {
    pub fn new() -> Self {
        Self {
            sys: Mutex::new(System::new_all()),
        }
    }

    pub fn list_processes(&self, opts: ListOpts) -> Result<ProcessListResult, AgentOSError> {
        let mut sys = self.sys.lock().unwrap_or_else(|e| e.into_inner());
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

        let mut processes = Vec::new();
        for (pid, process) in sys.processes() {
            let start_time = chrono::Utc
                .timestamp_opt(process.start_time() as i64, 0)
                .single()
                .unwrap_or_else(chrono::Utc::now);

            let entry = ProcessEntry {
                pid: pid.as_u32(),
                name: process.name().to_string_lossy().to_string(),
                cpu_usage_percent: process.cpu_usage(),
                memory_mb: process.memory() / 1024 / 1024,
                status: process.status().to_string(),
                parent_pid: process.parent().map(|p| p.as_u32()),
                start_time,
                command: process
                    .cmd()
                    .iter()
                    .map(|s| s.to_string_lossy().to_string())
                    .collect::<Vec<_>>()
                    .join(" "),
            };

            // Apply filters
            if let Some(ref name_filter) = opts.name_contains {
                let filter = name_filter.to_lowercase();
                if !entry.name.to_lowercase().contains(&filter)
                    && !entry.command.to_lowercase().contains(&filter)
                {
                    continue;
                }
            }
            if let Some(min_mem) = opts.min_memory_mb {
                if entry.memory_mb < min_mem {
                    continue;
                }
            }
            if let Some(min_cpu) = opts.min_cpu_percent {
                if entry.cpu_usage_percent < min_cpu {
                    continue;
                }
            }

            processes.push(entry);
        }

        let total_matched = processes.len();

        // Sort
        let sort_by = opts.sort_by.as_deref().unwrap_or("memory");
        let order = opts.order.as_deref().unwrap_or("desc");

        match sort_by {
            "memory" => processes.sort_by(|a, b| a.memory_mb.cmp(&b.memory_mb)),
            "cpu" => processes.sort_by(|a, b| {
                a.cpu_usage_percent
                    .partial_cmp(&b.cpu_usage_percent)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            "pid" => processes.sort_by(|a, b| a.pid.cmp(&b.pid)),
            "name" => processes.sort_by(|a, b| a.name.cmp(&b.name)),
            "start_time" => processes.sort_by(|a, b| a.start_time.cmp(&b.start_time)),
            _ => {
                return Err(AgentOSError::HalError(format!(
                    "invalid sort_by: {}",
                    sort_by
                )))
            }
        }

        if order == "desc" {
            processes.reverse();
        }

        // Limit
        let limit = opts.limit.unwrap_or(50).min(500);
        processes.truncate(limit);

        let returned = processes.len();

        Ok(ProcessListResult {
            processes,
            total_matched,
            returned,
        })
    }

    pub fn kill_process(&self, target_pid: u32) -> Result<(), AgentOSError> {
        // Guard against killing critical system processes or self
        if target_pid == 0 {
            return Err(AgentOSError::HalError(
                "Cannot kill PID 0 (kernel scheduler)".to_string(),
            ));
        }
        if target_pid == 1 {
            return Err(AgentOSError::HalError(
                "Cannot kill PID 1 (init/systemd)".to_string(),
            ));
        }
        if target_pid == std::process::id() {
            return Err(AgentOSError::HalError(
                "Cannot kill the AgentOS kernel process".to_string(),
            ));
        }

        let mut sys = self.sys.lock().unwrap_or_else(|e| e.into_inner());
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

        let pid = Pid::from_u32(target_pid);
        if let Some(process) = sys.process(pid) {
            if process.kill() {
                Ok(())
            } else {
                Err(AgentOSError::HalError(format!(
                    "Failed to kill process {}",
                    target_pid
                )))
            }
        } else {
            Err(AgentOSError::HalError(format!(
                "Process {} not found",
                target_pid
            )))
        }
    }
}

#[async_trait]
impl HalDriver for ProcessDriver {
    fn name(&self) -> &str {
        "process"
    }

    fn required_permission(&self) -> (&str, PermissionOp) {
        ("process.list", PermissionOp::Read)
        // Note: hal.rs directly mediates process.kill:x based on the action
    }

    async fn query(&self, params: Value) -> Result<Value, AgentOSError> {
        let action = params
            .get("action")
            .and_then(|a: &Value| a.as_str())
            .unwrap_or("list");

        match action {
            "list" => {
                let procs =
                    self.list_processes(serde_json::from_value(params).unwrap_or_default())?;
                Ok(serde_json::to_value(procs)
                    .map_err(|e| AgentOSError::HalError(e.to_string()))?)
            }
            "kill" => {
                let pid_u64 = params
                    .get("pid")
                    .and_then(|p: &Value| p.as_u64())
                    .ok_or_else(|| AgentOSError::HalError("Missing 'pid' in params".to_string()))?;

                if pid_u64 > u32::MAX as u64 {
                    return Err(AgentOSError::HalError(format!(
                        "PID {} out of range (max {})",
                        pid_u64,
                        u32::MAX
                    )));
                }

                let pid = pid_u64 as u32;
                self.kill_process(pid)?;
                Ok(
                    serde_json::json!({ "success": true, "message": format!("Process {} killed", pid) }),
                )
            }
            _ => Err(AgentOSError::HalError(format!(
                "Unknown action: {}",
                action
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_list_returns_self() {
        let driver = ProcessDriver::new();
        let procs = driver.list_processes(ListOpts::default()).unwrap();
        let self_pid = std::process::id();
        assert!(procs.processes.iter().any(|p| p.pid == self_pid));
    }

    #[test]
    fn test_list_sorted_by_memory_desc() {
        let driver = ProcessDriver::new();
        let opts = ListOpts {
            sort_by: Some("memory".into()),
            order: Some("desc".into()),
            limit: Some(10),
            ..Default::default()
        };
        let procs = driver.list_processes(opts).unwrap();
        assert!(procs.processes.len() <= 10);
        if procs.processes.len() > 1 {
            for i in 0..procs.processes.len() - 1 {
                assert!(procs.processes[i].memory_mb >= procs.processes[i + 1].memory_mb);
            }
        }
    }

    #[test]
    fn test_list_filtered_by_name_contains() {
        let driver = ProcessDriver::new();
        // Use a name that is likely to exist, like "cargo" or "rustc" or "sh"
        let opts = ListOpts {
            name_contains: Some("cargo".into()),
            ..Default::default()
        };
        let procs = driver.list_processes(opts).unwrap();
        for p in procs.processes {
            assert!(
                p.name.to_lowercase().contains("cargo")
                    || p.command.to_lowercase().contains("cargo")
            );
        }
    }

    #[test]
    fn test_list_limit_capped_at_500() {
        let driver = ProcessDriver::new();
        let opts = ListOpts {
            limit: Some(1000),
            ..Default::default()
        };
        let procs = driver.list_processes(opts).unwrap();
        assert!(procs.processes.len() <= 500);
    }

    #[test]
    fn test_invalid_sort_by_rejected() {
        let driver = ProcessDriver::new();
        let opts = ListOpts {
            sort_by: Some("invalid".into()),
            ..Default::default()
        };
        let res = driver.list_processes(opts);
        assert!(res.is_err());
    }
}

#[derive(Default, Debug, serde::Deserialize)]
pub struct ListOpts {
    pub sort_by: Option<String>,
    pub order: Option<String>,
    pub limit: Option<usize>,
    pub name_contains: Option<String>,
    pub min_memory_mb: Option<u64>,
    pub min_cpu_percent: Option<f32>,
}

#[derive(serde::Serialize)]
pub struct ProcessListResult {
    pub processes: Vec<ProcessEntry>,
    pub total_matched: usize,
    pub returned: usize,
}
