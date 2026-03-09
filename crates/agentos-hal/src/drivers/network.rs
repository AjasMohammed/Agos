use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::Value;
use sysinfo::Networks;

use crate::hal::HalDriver;
use crate::types::NetworkInterface;

pub struct NetworkDriver;

impl Default for NetworkDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl NetworkDriver {
    pub fn new() -> Self {
        Self
    }

    pub fn get_networks(&self) -> Result<Vec<NetworkInterface>, AgentOSError> {
        let networks = Networks::new_with_refreshed_list();

        let mut interfaces = Vec::new();
        for (name, data) in &networks {
            interfaces.push(NetworkInterface {
                name: name.clone(),
                ip_addresses: vec![], // sysinfo natively doesn't provide IPs easily, rely on names / stats
                bytes_received: data.total_received(),
                bytes_sent: data.total_transmitted(),
                packets_received: data.total_packets_received(),
                packets_sent: data.total_packets_transmitted(),
                errors_in: data.total_errors_on_received(),
                errors_out: data.total_errors_on_transmitted(),
            });
        }

        Ok(interfaces)
    }
}

#[async_trait]
impl HalDriver for NetworkDriver {
    fn name(&self) -> &str {
        "network"
    }

    fn required_permission(&self) -> (&str, PermissionOp) {
        ("network.logs", PermissionOp::Read)
    }

    async fn query(&self, _params: Value) -> Result<Value, AgentOSError> {
        let networks = self.get_networks()?;
        Ok(serde_json::to_value(networks).map_err(|e| AgentOSError::HalError(e.to_string()))?)
    }
}
