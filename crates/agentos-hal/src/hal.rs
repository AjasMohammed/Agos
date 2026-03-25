use std::collections::HashMap;
use std::sync::Arc;

use agentos_types::{AgentID, AgentOSError, PermissionOp, PermissionSet};
use async_trait::async_trait;
use serde_json::Value;

use crate::registry::HardwareRegistry;

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

/// The Hardware Abstraction Layer orchestrator.
pub struct HardwareAbstractionLayer {
    drivers: HashMap<String, Box<dyn HalDriver>>,
    /// Optional device registry for quarantine enforcement.
    ///
    /// When `None`, per-device quarantine checks are skipped. This is the correct
    /// mode for tests that construct the HAL without a full kernel context.
    /// Production usage always attaches a registry via `with_registry()`.
    registry: Option<Arc<HardwareRegistry>>,
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
        }
    }

    pub fn new_with_defaults() -> Self {
        let mut hal = Self::new();
        hal.register(Box::new(crate::drivers::system::SystemDriver::new()));
        hal.register(Box::new(crate::drivers::process::ProcessDriver::new()));
        hal.register(Box::new(crate::drivers::network::NetworkDriver::new()));
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

    pub fn register(&mut self, driver: Box<dyn HalDriver>) {
        self.drivers.insert(driver.name().to_string(), driver);
    }

    pub async fn query(
        &self,
        driver_name: &str,
        params: Value,
        permission_check: &PermissionSet,
        agent_id: Option<&AgentID>,
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
        if let (Some(registry), Some(agent_id), Some(device_key)) =
            (&self.registry, agent_id, driver.device_key(&params))
        {
            let device_type = format!("{}-device", driver_name);
            let is_new = registry.quarantine_device(&device_key, &device_type);
            if is_new {
                tracing::warn!(
                    device_id = %device_key,
                    driver = %driver_name,
                    agent_id = %agent_id,
                    "New hardware device auto-quarantined on first access — operator approval required"
                );
            }
            registry.check_access(&device_key, agent_id).map_err(|_| {
                AgentOSError::PermissionDenied {
                    resource: device_key.clone(),
                    operation: "device_access".to_string(),
                }
            })?;
        }

        driver.query(params).await
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
