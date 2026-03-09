use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::hal::HalDriver;

/// Storage driver that reads block device information from sysfs.
pub struct StorageDriver;

impl Default for StorageDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl StorageDriver {
    pub fn new() -> Self {
        Self
    }

    fn list_block_devices(&self) -> Vec<Value> {
        let mut devices = Vec::new();

        if let Ok(entries) = std::fs::read_dir("/sys/block/") {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                // Skip loop and ram devices
                if name.starts_with("loop") || name.starts_with("ram") {
                    continue;
                }

                let size_sectors = std::fs::read_to_string(entry.path().join("size"))
                    .ok()
                    .and_then(|s| s.trim().parse::<u64>().ok())
                    .unwrap_or(0);
                let size_gb = (size_sectors * 512) / (1024 * 1024 * 1024);

                let removable = std::fs::read_to_string(entry.path().join("removable"))
                    .ok()
                    .and_then(|s| s.trim().parse::<u8>().ok())
                    .map(|v| v == 1)
                    .unwrap_or(false);

                let ro = std::fs::read_to_string(entry.path().join("ro"))
                    .ok()
                    .and_then(|s| s.trim().parse::<u8>().ok())
                    .map(|v| v == 1)
                    .unwrap_or(false);

                let model = std::fs::read_to_string(entry.path().join("device/model"))
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();

                devices.push(json!({
                    "name": name,
                    "size_gb": size_gb,
                    "removable": removable,
                    "read_only": ro,
                    "model": model,
                }));
            }
        }

        devices
    }
}

#[async_trait]
impl HalDriver for StorageDriver {
    fn name(&self) -> &str {
        "storage"
    }

    fn required_permission(&self) -> (&str, PermissionOp) {
        ("hardware.storage", PermissionOp::Read)
    }

    async fn query(&self, params: Value) -> Result<Value, AgentOSError> {
        let action = params
            .get("action")
            .and_then(|a| a.as_str())
            .unwrap_or("list");

        match action {
            "list" => {
                let devices = self.list_block_devices();
                Ok(json!({ "devices": devices }))
            }
            other => Err(AgentOSError::HalError(format!(
                "Unknown storage action: {}",
                other
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_storage_list() {
        let driver = StorageDriver::new();
        let result = driver.query(json!({ "action": "list" })).await.unwrap();
        assert!(result["devices"].is_array());
    }
}
