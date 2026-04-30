use crate::hal::HalDriver;
use crate::types::{JournalEntry, JournalResult, UnitEntry, UnitStatus, UnitsResult};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::Value;
#[cfg(target_os = "linux")]
use std::sync::LazyLock;
#[cfg(target_os = "linux")]
use std::time::Duration;

#[cfg(target_os = "linux")]
use zbus::{Connection, Proxy};

/// Validate systemd unit names. Rejects shell metacharacters, spaces, `..`,
/// leading `-`. Compiled once. Combined with `Command::arg()` (argv, not shell)
/// this makes argv injection structurally impossible.
#[cfg(target_os = "linux")]
static UNIT_NAME_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"^[A-Za-z0-9@._\-]+\.(service|socket|timer|target|path|mount|swap|slice|scope)$",
    )
    .expect("static unit-name regex must compile")
});

/// Hard cap on journalctl runtime to prevent hangs on stuck cursors / huge units.
#[cfg(target_os = "linux")]
const JOURNALCTL_TIMEOUT: Duration = Duration::from_secs(30);

pub struct ServicesDriver;

impl Default for ServicesDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl ServicesDriver {
    pub fn new() -> Self {
        Self
    }

    #[cfg(target_os = "linux")]
    async fn get_system_conn(&self) -> Result<Connection, AgentOSError> {
        Connection::system().await.map_err(|e| {
            AgentOSError::HalError(format!("Failed to connect to system D-Bus: {}", e))
        })
    }

    #[cfg(target_os = "linux")]
    pub async fn list_units(&self, params: Value) -> Result<UnitsResult, AgentOSError> {
        let conn = self.get_system_conn().await?;
        let state_filter = params
            .get("state_filter")
            .and_then(|v| v.as_str())
            .unwrap_or("all");
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(100) as usize;

        let proxy = Proxy::new(
            &conn,
            "org.freedesktop.systemd1",
            "/org/freedesktop/systemd1",
            "org.freedesktop.systemd1.Manager",
        )
        .await
        .map_err(|e| AgentOSError::HalError(e.to_string()))?;

        // ListUnits returns (name, description, load_state, active_state, sub_state, followed, path, job_id, job_type, job_path)
        type UnitTuple = (
            String,
            String,
            String,
            String,
            String,
            String,
            zbus::zvariant::OwnedObjectPath,
            u32,
            String,
            zbus::zvariant::OwnedObjectPath,
        );
        let units_raw: Vec<UnitTuple> = proxy
            .call("ListUnits", &())
            .await
            .map_err(|e| AgentOSError::HalError(e.to_string()))?;

        let mut units = Vec::new();
        for (name, description, load_state, active_state, sub_state, _, _, _, _, _) in units_raw {
            if state_filter != "all" && active_state != state_filter {
                continue;
            }

            units.push(UnitEntry {
                name,
                description,
                load_state,
                active_state,
                sub_state,
            });
        }

        let total_matched = units.len();
        units.truncate(limit);
        let returned = units.len();

        Ok(UnitsResult {
            units,
            total_matched,
            returned,
        })
    }

    #[cfg(target_os = "linux")]
    pub async fn get_unit_status(&self, name: &str) -> Result<UnitStatus, AgentOSError> {
        let conn = self.get_system_conn().await?;

        let manager_proxy = Proxy::new(
            &conn,
            "org.freedesktop.systemd1",
            "/org/freedesktop/systemd1",
            "org.freedesktop.systemd1.Manager",
        )
        .await
        .map_err(|e| AgentOSError::HalError(e.to_string()))?;

        let path: zbus::zvariant::OwnedObjectPath =
            manager_proxy
                .call("GetUnit", &(name,))
                .await
                .map_err(|e| AgentOSError::HalError(format!("Unit {} not found: {}", name, e)))?;

        let unit_proxy = Proxy::new(
            &conn,
            "org.freedesktop.systemd1",
            path.clone(),
            "org.freedesktop.systemd1.Unit",
        )
        .await
        .map_err(|e| AgentOSError::HalError(e.to_string()))?;

        let active_state: String = unit_proxy
            .get_property("ActiveState")
            .await
            .unwrap_or_else(|_| "unknown".into());
        let sub_state: String = unit_proxy
            .get_property("SubState")
            .await
            .unwrap_or_else(|_| "unknown".into());

        // NRestarts is a Service property, not Unit
        let mut n_restarts = 0;
        let mut main_pid = None;
        let mut memory_current = None;

        if name.ends_with(".service") {
            let service_proxy = Proxy::new(
                &conn,
                "org.freedesktop.systemd1",
                path,
                "org.freedesktop.systemd1.Service",
            )
            .await
            .map_err(|e| AgentOSError::HalError(e.to_string()))?;

            n_restarts = service_proxy
                .get_property::<u32>("NRestarts")
                .await
                .unwrap_or(0);
            main_pid = service_proxy
                .get_property::<u32>("MainPID")
                .await
                .ok()
                .filter(|&p| p > 0);
            memory_current = service_proxy
                .get_property::<u64>("MemoryCurrent")
                .await
                .ok()
                .filter(|&m| m != u64::MAX);
        }

        let active_enter_timestamp: Option<chrono::DateTime<chrono::Utc>> = unit_proxy
            .get_property::<u64>("ActiveEnterTimestamp")
            .await
            .ok()
            .and_then(|ts| {
                if ts > 0 {
                    chrono::DateTime::from_timestamp(
                        (ts / 1_000_000) as i64,
                        ((ts % 1_000_000) * 1000) as u32,
                    )
                } else {
                    None
                }
            });

        Ok(UnitStatus {
            name: name.to_string(),
            active_state,
            sub_state,
            main_pid,
            memory_current_bytes: memory_current,
            active_enter_timestamp,
            n_restarts,
        })
    }

    #[cfg(target_os = "linux")]
    pub async fn get_logs(&self, name: &str, lines: usize) -> Result<JournalResult, AgentOSError> {
        if !UNIT_NAME_RE.is_match(name) {
            return Err(AgentOSError::HalError(format!(
                "Invalid unit name: {}",
                name
            )));
        }

        let output_fut = tokio::process::Command::new("journalctl")
            .arg("-u")
            .arg(name)
            .arg("-n")
            .arg(lines.to_string())
            .arg("--output=json")
            .arg("--no-pager")
            .output();

        let output = tokio::time::timeout(JOURNALCTL_TIMEOUT, output_fut)
            .await
            .map_err(|_| {
                AgentOSError::HalError(format!(
                    "journalctl timed out after {}s",
                    JOURNALCTL_TIMEOUT.as_secs()
                ))
            })?
            .map_err(|e| AgentOSError::HalError(format!("Failed to run journalctl: {}", e)))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut entries = Vec::new();

        for line in stdout.lines() {
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                let msg = v
                    .get("MESSAGE")
                    .and_then(|m| m.as_str())
                    .unwrap_or("")
                    .to_string();
                let priority = v
                    .get("PRIORITY")
                    .and_then(|p| p.as_str())
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(6);
                let ts_us = v
                    .get("__REALTIME_TIMESTAMP")
                    .and_then(|t| t.as_str())
                    .and_then(|s| s.parse::<u64>().ok());

                // Skip rows with malformed/missing timestamps rather than tagging
                // them as `now` and lying to the caller.
                let timestamp = match ts_us.and_then(|us| {
                    chrono::DateTime::from_timestamp(
                        (us / 1_000_000) as i64,
                        ((us % 1_000_000) * 1000) as u32,
                    )
                }) {
                    Some(t) => t,
                    None => continue,
                };

                entries.push(JournalEntry {
                    timestamp,
                    priority,
                    message: msg,
                });
            }
        }

        Ok(JournalResult {
            name: name.to_string(),
            entries,
        })
    }

    #[cfg(not(target_os = "linux"))]
    pub async fn list_units(&self, _params: Value) -> Result<UnitsResult, AgentOSError> {
        Err(AgentOSError::HalError(
            "system-services not supported on this platform".into(),
        ))
    }

    #[cfg(not(target_os = "linux"))]
    pub async fn get_unit_status(&self, _name: &str) -> Result<UnitStatus, AgentOSError> {
        Err(AgentOSError::HalError(
            "system-services not supported on this platform".into(),
        ))
    }

    #[cfg(not(target_os = "linux"))]
    pub async fn get_logs(
        &self,
        _name: &str,
        _lines: usize,
    ) -> Result<JournalResult, AgentOSError> {
        Err(AgentOSError::HalError(
            "system-services not supported on this platform".into(),
        ))
    }
}

#[async_trait]
impl HalDriver for ServicesDriver {
    fn name(&self) -> &str {
        "services"
    }

    fn required_permission(&self) -> (&str, PermissionOp) {
        ("system.services", PermissionOp::Read)
    }

    async fn query(&self, params: Value) -> Result<Value, AgentOSError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("list");
        match action {
            "list" => {
                let res = self.list_units(params).await?;
                Ok(serde_json::to_value(res).map_err(|e| AgentOSError::HalError(e.to_string()))?)
            }
            "status" => {
                let name = params.get("name").and_then(|v| v.as_str()).ok_or_else(|| {
                    AgentOSError::HalError("action 'status' requires 'name'".into())
                })?;
                let res = self.get_unit_status(name).await?;
                Ok(serde_json::to_value(res).map_err(|e| AgentOSError::HalError(e.to_string()))?)
            }
            "logs" => {
                let name = params.get("name").and_then(|v| v.as_str()).ok_or_else(|| {
                    AgentOSError::HalError("action 'logs' requires 'name'".into())
                })?;
                let lines = params
                    .get("log_lines")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(50) as usize;
                let res = self.get_logs(name, lines).await?;
                Ok(serde_json::to_value(res).map_err(|e| AgentOSError::HalError(e.to_string()))?)
            }
            other => Err(AgentOSError::HalError(format!("Unknown action: {}", other))),
        }
    }
}
