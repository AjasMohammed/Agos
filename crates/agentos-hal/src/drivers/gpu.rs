use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::hal::HalDriver;

/// GPU driver that detects GPUs via sysfs DRM subsystem.
/// For NVIDIA-specific features (VRAM, utilization), an optional `nvidia` feature
/// with the `nvml-wrapper` crate can be added in a future release.
pub struct GpuDriver;

impl Default for GpuDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl GpuDriver {
    pub fn new() -> Self {
        Self
    }

    fn list_gpus(&self) -> Vec<Value> {
        let mut devices = Vec::new();

        // Detect via /sys/class/drm/card*/
        if let Ok(entries) = std::fs::read_dir("/sys/class/drm/") {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                // Only match cardN entries (not card0-HDMI-A-1 etc.)
                if !name.starts_with("card") || name.contains('-') {
                    continue;
                }

                let device_path = entry.path().join("device");

                let vendor = std::fs::read_to_string(device_path.join("vendor"))
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                let device = std::fs::read_to_string(device_path.join("device"))
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();

                let vendor_name = match vendor.as_str() {
                    "0x10de" => "NVIDIA",
                    "0x1002" => "AMD",
                    "0x8086" => "Intel",
                    _ => "Unknown",
                };

                // Try to read VRAM from /sys/class/drm/cardN/device/mem_info_vram_total (AMD)
                let vram_total = std::fs::read_to_string(
                    device_path.join("mem_info_vram_total"),
                )
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok())
                .map(|bytes| bytes / 1_048_576); // Convert to MB

                let vram_used = std::fs::read_to_string(
                    device_path.join("mem_info_vram_used"),
                )
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok())
                .map(|bytes| bytes / 1_048_576);

                let mut gpu = json!({
                    "name": name,
                    "vendor": vendor_name,
                    "vendor_id": vendor,
                    "device_id": device,
                });

                if let Some(total) = vram_total {
                    gpu["vram_total_mb"] = json!(total);
                }
                if let Some(used) = vram_used {
                    gpu["vram_used_mb"] = json!(used);
                }

                devices.push(gpu);
            }
        }

        devices
    }
}

#[async_trait]
impl HalDriver for GpuDriver {
    fn name(&self) -> &str {
        "gpu"
    }

    fn required_permission(&self) -> (&str, PermissionOp) {
        ("hardware.gpu", PermissionOp::Read)
    }

    async fn query(&self, params: Value) -> Result<Value, AgentOSError> {
        let action = params
            .get("action")
            .and_then(|a| a.as_str())
            .unwrap_or("list");

        match action {
            "list" => {
                let devices = self.list_gpus();
                Ok(json!({ "devices": devices }))
            }
            other => Err(AgentOSError::HalError(format!(
                "Unknown GPU action: {}",
                other
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_gpu_list() {
        let driver = GpuDriver::new();
        let result = driver.query(json!({ "action": "list" })).await.unwrap();
        assert!(result["devices"].is_array());
    }
}
