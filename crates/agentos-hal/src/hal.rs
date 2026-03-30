use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use agentos_types::{AgentID, AgentOSError, PermissionOp, PermissionSet, TaskID};
use async_trait::async_trait;
use serde_json::Value;
use sysinfo::Networks;

use crate::registry::{DeviceStatus, HardwareRegistry};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredDevice {
    pub id: String,
    pub device_type: String,
}

/// Inspect the current host and return the hardware devices that should be
/// registered in the HAL registry during kernel boot.
pub fn discover_available_devices() -> Vec<DiscoveredDevice> {
    let mut devices = vec![
        DiscoveredDevice {
            id: "cpu:system".to_string(),
            device_type: "cpu".to_string(),
        },
        DiscoveredDevice {
            id: "memory:system".to_string(),
            device_type: "memory".to_string(),
        },
    ];

    let networks = Networks::new_with_refreshed_list();
    for (name, _) in &networks {
        devices.push(DiscoveredDevice {
            id: format!("network:{name}"),
            device_type: "network-interface".to_string(),
        });
    }

    if let Ok(entries) = std::fs::read_dir("/sys/block/") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("loop") || name.starts_with("ram") {
                continue;
            }

            devices.push(DiscoveredDevice {
                id: format!("storage:{name}"),
                device_type: "block-device".to_string(),
            });
        }
    }

    if let Ok(entries) = std::fs::read_dir("/sys/class/drm/") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with("card") || name.contains('-') {
                continue;
            }

            let id = name
                .strip_prefix("card")
                .map(|suffix| format!("gpu:{suffix}"))
                .unwrap_or_else(|| format!("gpu:{name}"));
            devices.push(DiscoveredDevice {
                id,
                device_type: "gpu".to_string(),
            });
        }
    }

    if let Ok(entries) = std::fs::read_dir("/sys/class/thermal/") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with("thermal_zone") {
                continue;
            }

            devices.push(DiscoveredDevice {
                id: format!("sensor:{name}"),
                device_type: "thermal-sensor".to_string(),
            });
        }
    }

    devices
}

/// Every HAL driver implements this trait.
#[async_trait]
pub trait HalDriver: Send + Sync {
    /// Human-readable driver name (e.g. "system", "process").
    fn name(&self) -> &str;

    /// The permission required to use this driver.
    fn required_permission(&self) -> (&str, PermissionOp);

    /// Execute a typed query and return a JSON result.
    async fn query(&self, params: Value) -> Result<Value, AgentOSError>;

    /// Returns the device registry key for this call (e.g. `"gpu:0"`, `"storage:/dev/sda"`).
    ///
    /// Return `None` for non-device drivers (system, process, network, log_reader) —
    /// those do not map to physical devices requiring per-device quarantine enforcement.
    /// The default implementation returns `None` so existing drivers need not change.
    fn device_key(&self, _params: &Value) -> Option<String> {
        None
    }
}

/// Optional observer for HAL driver actions that should surface in the kernel event stream.
#[async_trait]
pub trait HalEventSink: Send + Sync {
    async fn emit_driver_event(
        &self,
        driver_name: &str,
        params: &Value,
        result: &Value,
        agent_id: Option<&AgentID>,
    ) -> Result<(), AgentOSError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HalOperation {
    Read,
    Write,
    Execute,
    Query,
    Observe,
}

impl fmt::Display for HalOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Execute => "execute",
            Self::Query => "query",
            Self::Observe => "observe",
        };
        f.write_str(label)
    }
}

impl From<PermissionOp> for HalOperation {
    fn from(value: PermissionOp) -> Self {
        match value {
            PermissionOp::Read => Self::Read,
            PermissionOp::Write => Self::Write,
            PermissionOp::Execute => Self::Execute,
            PermissionOp::Query => Self::Query,
            PermissionOp::Observe => Self::Observe,
        }
    }
}

#[async_trait]
pub trait DeviceAccessGate: Send + Sync {
    async fn check(
        &self,
        agent_id: &AgentID,
        task_id: &TaskID,
        device_id: &str,
        device_type: &str,
        operation: HalOperation,
    ) -> Result<(), AgentOSError>;
}

/// The Hardware Abstraction Layer orchestrator.
pub struct HardwareAbstractionLayer {
    drivers: HashMap<String, Box<dyn HalDriver>>,
    /// Optional device registry for lightweight tests and compatibility.
    registry: Option<Arc<HardwareRegistry>>,
    device_access_gate: Option<Arc<dyn DeviceAccessGate>>,
    event_sink: Option<Arc<dyn HalEventSink>>,
}

impl Default for HardwareAbstractionLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl HardwareAbstractionLayer {
    pub fn new() -> Self {
        Self {
            drivers: HashMap::new(),
            registry: None,
            device_access_gate: None,
            event_sink: None,
        }
    }

    pub fn new_with_defaults() -> Self {
        let mut hal = Self::new();
        hal.register(Box::new(crate::drivers::system::SystemDriver::new()));
        hal.register(Box::new(crate::drivers::process::ProcessDriver::new()));
        hal.register(Box::new(crate::drivers::network::NetworkDriver::new()));
        hal.register(Box::new(crate::drivers::storage::StorageDriver::new()));
        #[cfg(feature = "usb-storage")]
        hal.register(Box::new(
            crate::drivers::usb_storage::UsbStorageDriver::new(),
        ));
        // Note: log_reader requires paths, initialized differently usually, but we can provide defaults or leave it for Kernel.
        hal
    }

    /// Attach a `HardwareRegistry` for device-level quarantine enforcement.
    ///
    /// Call this during kernel boot after constructing the HAL. Without a registry,
    /// device-mapped drivers (gpu, storage, sensor) skip quarantine checks and only
    /// apply `PermissionSet` validation — which is unsafe in production.
    pub fn with_registry(mut self, registry: Arc<HardwareRegistry>) -> Self {
        self.registry = Some(registry);
        self
    }

    pub fn with_device_access_gate(
        mut self,
        device_access_gate: Arc<dyn DeviceAccessGate>,
    ) -> Self {
        self.device_access_gate = Some(device_access_gate);
        self
    }

    /// Attach an optional event sink for driver actions that should be surfaced
    /// to the kernel's event and audit pipeline.
    pub fn with_event_sink(mut self, event_sink: Arc<dyn HalEventSink>) -> Self {
        self.event_sink = Some(event_sink);
        self
    }

    pub fn register(&mut self, driver: Box<dyn HalDriver>) {
        self.drivers.insert(driver.name().to_string(), driver);
    }

    pub async fn query(
        &self,
        driver_name: &str,
        params: Value,
        permission_check: &PermissionSet,
        agent_id: Option<&AgentID>,
        task_id: Option<&TaskID>,
    ) -> Result<Value, AgentOSError> {
        let driver = self
            .drivers
            .get(driver_name)
            .ok_or_else(|| AgentOSError::HalError(format!("Driver '{}' not found", driver_name)))?;

        let (resource, op) = driver.required_permission();

        // --- Permission check (unchanged logic) ---
        // Special logic for process kill
        if driver_name == "process" {
            if let Some(action) = params.get("action").and_then(|a| a.as_str()) {
                if action == "kill" {
                    if !permission_check.check("process.kill", PermissionOp::Execute) {
                        return Err(AgentOSError::PermissionDenied {
                            resource: "process.kill".to_string(),
                            operation: "x".to_string(),
                        });
                    }
                } else if action == "list" {
                    if !permission_check.check("process.list", PermissionOp::Read) {
                        return Err(AgentOSError::PermissionDenied {
                            resource: "process.list".to_string(),
                            operation: "r".to_string(),
                        });
                    }
                } else if !permission_check.check(resource, op) {
                    return Err(AgentOSError::PermissionDenied {
                        resource: resource.to_string(),
                        operation: op_str(op).to_string(),
                    });
                }
            } else if !permission_check.check(resource, op) {
                return Err(AgentOSError::PermissionDenied {
                    resource: resource.to_string(),
                    operation: op_str(op).to_string(),
                });
            }
        } else if !permission_check.check(resource, op) {
            return Err(AgentOSError::PermissionDenied {
                resource: resource.to_string(),
                operation: op_str(op).to_string(),
            });
        }

        // --- Device quarantine enforcement ---
        //
        // Only engaged when:
        //   1. A registry is attached (production kernel path).
        //   2. An agent_id was supplied (identifies who is making the request).
        //   3. The driver returns a device_key for this call (physical-device drivers only).
        //
        // On first contact the device is auto-quarantined. Access is denied until an
        // operator approves it via `agentctl hal approve`. If already approved for this
        // agent, the call proceeds to `driver.query()`.
        if let Some(device_key) = driver.device_key(&params) {
            let device_type = format!("{}-device", driver_name);

            if let (Some(device_access_gate), Some(agent_id), Some(task_id)) =
                (&self.device_access_gate, agent_id, task_id)
            {
                device_access_gate
                    .check(
                        agent_id,
                        task_id,
                        &device_key,
                        &device_type,
                        HalOperation::from(op),
                    )
                    .await?;
            } else if let (Some(registry), Some(agent_id)) = (&self.registry, agent_id) {
                if registry.get_device_status(&device_key).is_none() {
                    registry.register_pending_device(&device_key, &device_type);
                }

                match registry.get_device_status(&device_key) {
                    Some(DeviceStatus::Approved) => registry.check_access(&device_key, agent_id)?,
                    Some(DeviceStatus::Pending) => {
                        return Err(AgentOSError::DeviceAccessPending {
                            device_id: device_key,
                            escalation_id: "pending".to_string(),
                        });
                    }
                    Some(DeviceStatus::Quarantined) => {
                        return Err(AgentOSError::DeviceQuarantined(device_key));
                    }
                    None => {
                        return Err(AgentOSError::HalError(format!(
                            "Device '{}' could not be registered in the hardware registry",
                            device_key
                        )));
                    }
                }
            }
        }

        let params_for_sink = params.clone();
        let result = driver.query(params).await?;

        if let Some(event_sink) = &self.event_sink {
            if let Err(err) = event_sink
                .emit_driver_event(driver_name, &params_for_sink, &result, agent_id)
                .await
            {
                tracing::warn!(
                    driver = %driver_name,
                    error = %err,
                    "HAL event sink failed; driver result returned without event emission"
                );
            }
        }

        Ok(result)
    }
}

/// Convert a `PermissionOp` to its single-character string representation.
#[inline]
fn op_str(op: PermissionOp) -> &'static str {
    match op {
        PermissionOp::Read => "r",
        PermissionOp::Write => "w",
        PermissionOp::Execute => "x",
        PermissionOp::Query => "q",
        PermissionOp::Observe => "o",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    struct MockDriver;

    #[async_trait]
    impl HalDriver for MockDriver {
        fn name(&self) -> &str {
            "mock"
        }

        fn required_permission(&self) -> (&str, PermissionOp) {
            ("hardware.mock", PermissionOp::Read)
        }

        fn device_key(&self, params: &Value) -> Option<String> {
            params
                .get("device_id")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        }

        async fn query(&self, _params: Value) -> Result<Value, AgentOSError> {
            Ok(json!({ "ok": true }))
        }
    }

    struct RecordingGate {
        calls: Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl DeviceAccessGate for RecordingGate {
        async fn check(
            &self,
            _agent_id: &AgentID,
            _task_id: &TaskID,
            device_id: &str,
            _device_type: &str,
            operation: HalOperation,
        ) -> Result<(), AgentOSError> {
            self.calls
                .lock()
                .unwrap()
                .push((device_id.to_string(), operation.to_string()));
            Ok(())
        }
    }

    #[test]
    fn discover_available_devices_includes_core_system_entries() {
        let devices = discover_available_devices();

        assert!(devices.iter().any(|device| device.id == "cpu:system"));
        assert!(devices.iter().any(|device| device.id == "memory:system"));
    }

    #[tokio::test]
    async fn device_access_gate_runs_for_device_scoped_queries() {
        let gate = Arc::new(RecordingGate {
            calls: Mutex::new(Vec::new()),
        });
        let mut hal = HardwareAbstractionLayer::new();
        hal.register(Box::new(MockDriver));
        let hal = hal.with_device_access_gate(gate.clone());

        let mut perms = PermissionSet::new();
        perms.grant("hardware.mock".to_string(), true, false, false, None);

        hal.query(
            "mock",
            json!({ "device_id": "gpu:0" }),
            &perms,
            Some(&AgentID::new()),
            Some(&TaskID::new()),
        )
        .await
        .expect("device-scoped query should succeed");

        let calls = gate.calls.lock().unwrap();
        assert_eq!(
            calls.as_slice(),
            &[("gpu:0".to_string(), "read".to_string())]
        );
    }
}
