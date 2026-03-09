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

    pub fn list_processes(&self) -> Result<Vec<ProcessEntry>, AgentOSError> {
        let mut sys = self.sys.lock().unwrap();
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

        let mut processes = Vec::new();
        for (pid, process) in sys.processes() {
            let start_time = chrono::Utc
                .timestamp_opt(process.start_time() as i64, 0)
                .single()
                .unwrap_or_else(chrono::Utc::now);

            processes.push(ProcessEntry {
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
            });
        }

        Ok(processes)
    }

    pub fn kill_process(&self, target_pid: u32) -> Result<(), AgentOSError> {
        let mut sys = self.sys.lock().unwrap();
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
                let procs = self.list_processes()?;
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
        let procs = driver.list_processes().unwrap();
        let self_pid = std::process::id();
        assert!(procs.iter().any(|p| p.pid == self_pid));
    }
}
