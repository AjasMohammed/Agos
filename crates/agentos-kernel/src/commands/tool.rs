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
        match toml::from_str::<ToolManifest>(&content) {
            Ok(manifest) => {
                self.tool_registry.write().await.register(manifest);
                KernelResponse::Success { data: None }
            }
            Err(e) => KernelResponse::Error {
                message: format!("Invalid manifest: {}", e),
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
