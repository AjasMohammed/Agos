use agentos_tools::loader::{load_all_manifests, LoadedManifest};
use agentos_types::*;
use std::collections::HashMap;
use std::path::Path;

pub struct ToolRegistry {
    tools: HashMap<ToolID, RegisteredTool>,
    name_index: HashMap<String, ToolID>,
    /// Keeps LoadedManifest (with manifest_dir) so WASM tools can resolve wasm_path at boot.
    pub loaded: Vec<LoadedManifest>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            name_index: HashMap::new(),
            loaded: Vec::new(),
        }
    }

    /// Load all tool manifests from the core and user tool directories.
    pub fn load_from_dirs(core_dir: &Path, user_dir: &Path) -> Result<Self, AgentOSError> {
        let mut registry = Self::new();

        for dir in [core_dir, user_dir] {
            if !dir.exists() {
                continue;
            }
            let manifests = load_all_manifests(dir)?;
            for loaded in manifests {
                registry.register(loaded.manifest.clone());
                registry.loaded.push(loaded);
            }
        }

        Ok(registry)
    }

    /// Register a single tool from its manifest.
    pub fn register(&mut self, manifest: ToolManifest) -> ToolID {
        let tool_id = ToolID::new();
        let name = manifest.manifest.name.clone();
        let tool = RegisteredTool {
            id: tool_id,
            manifest,
            status: ToolStatus::Available,
        };
        self.name_index.insert(name, tool_id);
        self.tools.insert(tool_id, tool);
        tool_id
    }

    pub fn get_by_name(&self, name: &str) -> Option<&RegisteredTool> {
        self.name_index.get(name).and_then(|id| self.tools.get(id))
    }

    pub fn get_by_id(&self, id: &ToolID) -> Option<&RegisteredTool> {
        self.tools.get(id)
    }

    pub fn list_all(&self) -> Vec<&RegisteredTool> {
        self.tools.values().collect()
    }

    pub fn remove(&mut self, name: &str) -> Result<(), AgentOSError> {
        if let Some(id) = self.name_index.remove(name) {
            self.tools.remove(&id);
            Ok(())
        } else {
            Err(AgentOSError::ToolNotFound(name.to_string()))
        }
    }

    /// Get the list of all tools formatted for the system prompt.
    pub fn tools_for_prompt(&self) -> String {
        let mut lines = Vec::new();
        for tool in self.tools.values() {
            lines.push(format!(
                "- {} : {}",
                tool.manifest.manifest.name, tool.manifest.manifest.description
            ));
        }
        if lines.is_empty() {
            "No tools available.".to_string()
        } else {
            lines.join("\n")
        }
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}
