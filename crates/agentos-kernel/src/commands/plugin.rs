use crate::kernel::Kernel;
use crate::plugin_registry::PluginStatus;
use agentos_bus::KernelResponse;

impl Kernel {
    pub(crate) async fn cmd_list_plugins(&self) -> KernelResponse {
        let plugins = self.plugin_registry.list().await;
        let data: Vec<serde_json::Value> = plugins
            .iter()
            .map(|e| {
                let status = match &e.status {
                    PluginStatus::Discovered => "discovered",
                    PluginStatus::Active => "active",
                    PluginStatus::Disabled => "disabled",
                    PluginStatus::Blocked { .. } => "blocked",
                };
                let block_reason = match &e.status {
                    PluginStatus::Blocked { reason } => Some(reason.as_str()),
                    _ => None,
                };
                serde_json::json!({
                    "id": e.manifest.id,
                    "display_name": e.manifest.display_name,
                    "version": e.manifest.version,
                    "description": e.manifest.description,
                    "trust_tier": format!("{:?}", e.manifest.trust_tier),
                    "status": status,
                    "block_reason": block_reason,
                    "path": e.manifest_path.display().to_string(),
                })
            })
            .collect();

        KernelResponse::Success {
            data: Some(serde_json::json!({ "plugins": data })),
        }
    }

    pub(crate) async fn cmd_enable_plugin(&self, plugin_id: String) -> KernelResponse {
        match self.plugin_registry.activate(&plugin_id).await {
            Ok(()) => KernelResponse::Success {
                data: Some(serde_json::json!({ "plugin_id": plugin_id, "status": "active" })),
            },
            Err(e) => KernelResponse::Error {
                message: format!("Failed to enable plugin '{}': {}", plugin_id, e),
            },
        }
    }

    pub(crate) async fn cmd_disable_plugin(&self, plugin_id: String) -> KernelResponse {
        match self.plugin_registry.deactivate(&plugin_id).await {
            Ok(()) => KernelResponse::Success {
                data: Some(serde_json::json!({ "plugin_id": plugin_id, "status": "disabled" })),
            },
            Err(e) => KernelResponse::Error {
                message: format!("Failed to disable plugin '{}': {}", plugin_id, e),
            },
        }
    }
}
