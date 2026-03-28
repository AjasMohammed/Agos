use crate::kernel::Kernel;
use agentos_bus::KernelResponse;
use agentos_types::*;

impl Kernel {
    pub(crate) async fn cmd_list_tools(&self) -> KernelResponse {
        let registry = self.tool_registry.read().await;
        let tools: Vec<ToolManifest> = registry
            .list_all()
            .into_iter()
            .map(|t| t.manifest.clone())
            .collect();
        KernelResponse::ToolList(tools)
    }

    pub(crate) async fn cmd_install_tool(&self, manifest_path: String) -> KernelResponse {
        let path = std::path::Path::new(&manifest_path);
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                return KernelResponse::Error {
                    message: format!("Cannot read manifest '{}': {}", manifest_path, e),
                }
            }
        };
        let manifest = match toml::from_str::<ToolManifest>(&content) {
            Ok(m) => m,
            Err(e) => {
                return KernelResponse::Error {
                    message: format!("Invalid manifest: {}", e),
                }
            }
        };
        match self.tool_registry.write().await.register(manifest) {
            Ok(_) => KernelResponse::Success { data: None },
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    /// Hot-load a tool from a manifest file that has already been written to disk.
    /// Returns the assigned ToolID on success so the caller knows it was registered.
    pub(crate) async fn cmd_tool_load(&self, manifest_path: String) -> KernelResponse {
        let path = std::path::Path::new(&manifest_path);
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                return KernelResponse::Error {
                    message: format!("Cannot read manifest '{}': {}", manifest_path, e),
                }
            }
        };
        let manifest = match toml::from_str::<ToolManifest>(&content) {
            Ok(m) => m,
            Err(e) => {
                return KernelResponse::Error {
                    message: format!("Invalid manifest: {}", e),
                }
            }
        };
        let tool_name = manifest.manifest.name.clone();
        match self.tool_registry.write().await.register(manifest) {
            Ok(id) => {
                tracing::info!(tool_name = %tool_name, tool_id = %id, "Tool hot-loaded");
                KernelResponse::Success {
                    data: Some(serde_json::json!({
                        "tool_id": id.to_string(),
                        "tool_name": tool_name,
                    })),
                }
            }
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    pub(crate) async fn cmd_remove_tool(&self, tool_name: String) -> KernelResponse {
        match self.tool_registry.write().await.remove(&tool_name) {
            Ok(_) => KernelResponse::Success { data: None },
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }
}
