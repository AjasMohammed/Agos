use std::collections::HashMap;

use agentos_types::{AgentOSError, PermissionOp, PermissionSet};
use async_trait::async_trait;
use serde_json::Value;

/// Every HAL driver implements this trait.
#[async_trait]
pub trait HalDriver: Send + Sync {
    /// Human-readable driver name (e.g. "system", "process").
    fn name(&self) -> &str;

    /// The permission required to use this driver.
    fn required_permission(&self) -> (&str, PermissionOp);

    /// Execute a typed query and return a JSON result.
    async fn query(&self, params: Value) -> Result<Value, AgentOSError>;
}

/// The Hardware Abstraction Layer orchestrator.
pub struct HardwareAbstractionLayer {
    drivers: HashMap<String, Box<dyn HalDriver>>,
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

    pub fn register(&mut self, driver: Box<dyn HalDriver>) {
        self.drivers.insert(driver.name().to_string(), driver);
    }

    pub async fn query(
        &self,
        driver_name: &str,
        params: Value,
        permission_check: &PermissionSet,
    ) -> Result<Value, AgentOSError> {
        let driver = self
            .drivers
            .get(driver_name)
            .ok_or_else(|| AgentOSError::HalError(format!("Driver '{}' not found", driver_name)))?;

        let (resource, op) = driver.required_permission();

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
                        operation: match op {
                            PermissionOp::Read => "r".to_string(),
                            PermissionOp::Write => "w".to_string(),
                            PermissionOp::Execute => "x".to_string(),
                            PermissionOp::Query => "q".to_string(),
                            PermissionOp::Observe => "o".to_string(),
                        },
                    });
                }
            } else if !permission_check.check(resource, op) {
                return Err(AgentOSError::PermissionDenied {
                    resource: resource.to_string(),
                    operation: match op {
                        PermissionOp::Read => "r".to_string(),
                        PermissionOp::Write => "w".to_string(),
                        PermissionOp::Execute => "x".to_string(),
                        PermissionOp::Query => "q".to_string(),
                        PermissionOp::Observe => "o".to_string(),
                    },
                });
            }
        } else if !permission_check.check(resource, op) {
            return Err(AgentOSError::PermissionDenied {
                resource: resource.to_string(),
                operation: match op {
                    PermissionOp::Read => "r".to_string(),
                    PermissionOp::Write => "w".to_string(),
                    PermissionOp::Execute => "x".to_string(),
                    PermissionOp::Query => "q".to_string(),
                    PermissionOp::Observe => "o".to_string(),
                },
            });
        }

        driver.query(params).await
    }
}
