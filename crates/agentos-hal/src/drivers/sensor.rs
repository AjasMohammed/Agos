use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::hal::HalDriver;

/// Reads thermal sensors from sysfs thermal zones and hwmon devices.
pub struct SensorDriver;

impl Default for SensorDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl SensorDriver {
    pub fn new() -> Self {
        Self
    }

    fn read_temperatures(&self) -> Vec<Value> {
        let mut readings = Vec::new();

        // Read from /sys/class/thermal/thermal_zone*/temp
        if let Ok(entries) = std::fs::read_dir("/sys/class/thermal/") {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !name.starts_with("thermal_zone") {
                    continue;
                }
                let temp_path = entry.path().join("temp");
                let type_path = entry.path().join("type");

                let zone_type = std::fs::read_to_string(&type_path)
                    .map(|s| s.trim().to_string())
                    .unwrap_or_else(|_| "unknown".to_string());

                if let Ok(temp_str) = std::fs::read_to_string(&temp_path) {
                    if let Ok(millidegrees) = temp_str.trim().parse::<f64>() {
                        readings.push(json!({
                            "zone": name,
                            "type": zone_type,
                            "celsius": millidegrees / 1000.0,
                        }));
                    }
                }
            }
        }

        readings
    }
}

#[async_trait]
impl HalDriver for SensorDriver {
    fn name(&self) -> &str {
        "sensor"
    }

    fn required_permission(&self) -> (&str, PermissionOp) {
        ("hardware.sensor", PermissionOp::Read)
    }

    async fn query(&self, params: Value) -> Result<Value, AgentOSError> {
        let action = params
            .get("action")
            .and_then(|a| a.as_str())
            .unwrap_or("read_temperature");

        match action {
            "read_temperature" => {
                let readings = self.read_temperatures();
                Ok(json!({ "temperatures": readings }))
            }
            other => Err(AgentOSError::HalError(format!(
                "Unknown sensor action: {}",
                other
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_sensor_read_temperature() {
        let driver = SensorDriver::new();
        let result = driver
            .query(json!({ "action": "read_temperature" }))
            .await
            .unwrap();
        // Should succeed even if no sensors found (returns empty list)
        assert!(result["temperatures"].is_array());
    }
}
