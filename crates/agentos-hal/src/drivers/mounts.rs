use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::Value;

use crate::hal::HalDriver;
use crate::types::{MountEntry, MountsResult};

pub struct MountsDriver;

impl Default for MountsDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl MountsDriver {
    pub fn new() -> Self {
        Self
    }

    #[cfg(target_os = "linux")]
    pub fn list_mounts(&self, params: Value) -> Result<MountsResult, AgentOSError> {
        let device_filter = params.get("device_contains").and_then(|v| v.as_str());
        let mount_filter = params.get("mount_contains").and_then(|v| v.as_str());
        let fs_filter = params.get("fs_type").and_then(|v| v.as_str());
        let writable_only = params
            .get("writable_only")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let mut mounts = Vec::new();

        let info = match procfs::process::Process::myself().and_then(|p| p.mountinfo()) {
            Ok(i) => i,
            Err(e) => {
                tracing::debug!(error = %e, "system-mounts: procfs mountinfo unavailable");
                return Ok(MountsResult {
                    total_matched: 0,
                    returned: 0,
                    mounts,
                });
            }
        };

        for m in info {
            if let Some(f) = device_filter {
                if !m.mount_source.as_deref().unwrap_or("").contains(f) {
                    continue;
                }
            }
            if let Some(f) = mount_filter {
                if !m.mount_point.to_string_lossy().contains(f) {
                    continue;
                }
            }
            if let Some(f) = fs_filter {
                if m.fs_type != f {
                    continue;
                }
            }

            // Check exact "ro" key, not substring (avoids matching e.g. "errors=remount-ro").
            let writable = !m.mount_options.contains_key("ro");
            if writable_only && !writable {
                continue;
            }

            let mut opts: Vec<String> = m
                .mount_options
                .iter()
                .map(|(k, v)| match v {
                    Some(val) => format!("{k}={val}"),
                    None => k.clone(),
                })
                .collect();
            opts.sort();
            let options = opts.join(",");

            let mut total_bytes: u64 = 0;
            let mut available_bytes: u64 = 0;
            let mut use_percent: f32 = 0.0;

            if let Ok(stats) = nix::sys::statvfs::statvfs(&m.mount_point) {
                total_bytes = stats.blocks().saturating_mul(stats.block_size());
                available_bytes = stats.blocks_available().saturating_mul(stats.block_size());
                if total_bytes > 0 {
                    let used = total_bytes.saturating_sub(available_bytes);
                    use_percent = (used as f32 / total_bytes as f32) * 100.0;
                }
            }

            mounts.push(MountEntry {
                device: m.mount_source.as_deref().unwrap_or("").to_string(),
                mount_point: m.mount_point.to_string_lossy().to_string(),
                fs_type: m.fs_type,
                options,
                writable,
                total_bytes,
                available_bytes,
                use_percent,
            });
        }

        let returned = mounts.len();
        Ok(MountsResult {
            total_matched: returned,
            returned,
            mounts,
        })
    }

    #[cfg(not(target_os = "linux"))]
    pub fn list_mounts(&self, _params: Value) -> Result<MountsResult, AgentOSError> {
        Err(AgentOSError::HalError(
            "system-mounts not supported on this platform".into(),
        ))
    }
}

#[async_trait]
impl HalDriver for MountsDriver {
    fn name(&self) -> &str {
        "mounts"
    }

    fn required_permission(&self) -> (&str, PermissionOp) {
        ("system.mounts", PermissionOp::Read)
    }

    async fn query(&self, params: Value) -> Result<Value, AgentOSError> {
        let result = self.list_mounts(params)?;
        Ok(serde_json::to_value(result).map_err(|e| AgentOSError::HalError(e.to_string()))?)
    }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_list_mounts_includes_root() {
        let driver = MountsDriver::new();
        let res = driver.list_mounts(serde_json::json!({})).unwrap();
        assert!(res.mounts.iter().any(|m| m.mount_point == "/"));
    }
}
