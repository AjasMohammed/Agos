use agentos_tools::loader::{load_all_manifests, LoadedManifest};
use agentos_tools::signing::{verify_manifest_with_crl, RevocationList};
use agentos_types::*;
use std::collections::HashMap;
use std::path::Path;
use tokio::sync::mpsc;

/// Lightweight notification sent by ToolRegistry to the kernel.
/// The kernel converts these into properly signed EventMessages with audit trail.
#[derive(Debug, Clone)]
pub enum ToolLifecycleEvent {
    Installed {
        tool_id: ToolID,
        tool_name: String,
        trust_tier: String,
        description: String,
    },
    Removed {
        tool_id: ToolID,
        tool_name: String,
    },
}

pub struct ToolRegistry {
    tools: HashMap<ToolID, RegisteredTool>,
    name_index: HashMap<String, ToolID>,
    /// Keeps LoadedManifest (with manifest_dir) so WASM tools can resolve wasm_path at boot.
    pub loaded: Vec<LoadedManifest>,
    /// Certificate revocation list — tools signed by revoked keys are rejected.
    crl: RevocationList,
    /// Optional channel for notifying the kernel of tool lifecycle changes.
    lifecycle_sender: Option<mpsc::Sender<ToolLifecycleEvent>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            name_index: HashMap::new(),
            loaded: Vec::new(),
            crl: RevocationList::new(),
            lifecycle_sender: None,
        }
    }

    /// Create a new registry with a pre-loaded CRL.
    pub fn with_crl(crl: RevocationList) -> Self {
        Self {
            tools: HashMap::new(),
            name_index: HashMap::new(),
            loaded: Vec::new(),
            crl,
            lifecycle_sender: None,
        }
    }

    /// Set the lifecycle notification sender. The kernel uses this to receive
    /// tool install/remove notifications and convert them into signed events.
    pub fn set_lifecycle_sender(&mut self, sender: mpsc::Sender<ToolLifecycleEvent>) {
        self.lifecycle_sender = Some(sender);
    }

    /// Load all tool manifests from the core and user tool directories.
    pub fn load_from_dirs(core_dir: &Path, user_dir: &Path) -> Result<Self, AgentOSError> {
        Self::load_from_dirs_with_crl(core_dir, user_dir, RevocationList::new())
    }

    /// Load all tool manifests with CRL enforcement.
    pub fn load_from_dirs_with_crl(
        core_dir: &Path,
        user_dir: &Path,
        crl: RevocationList,
    ) -> Result<Self, AgentOSError> {
        let mut registry = Self::with_crl(crl);

        for dir in [core_dir, user_dir] {
            if !dir.exists() {
                continue;
            }
            let manifests = load_all_manifests(dir)?;
            for loaded in manifests {
                registry.register(loaded.manifest.clone())?;
                registry.loaded.push(loaded);
            }
        }

        Ok(registry)
    }

    /// Register a single tool from its manifest, enforcing trust tier and CRL policy.
    ///
    /// Returns an error if the manifest is `Blocked`, the author key is revoked,
    /// or if a `Community`/`Verified` manifest has a missing or invalid Ed25519 signature.
    pub fn register(&mut self, manifest: ToolManifest) -> Result<ToolID, AgentOSError> {
        verify_manifest_with_crl(&manifest, &self.crl)?;

        let tool_id = ToolID::new();
        let name = manifest.manifest.name.clone();
        let trust_tier = format!("{:?}", manifest.manifest.trust_tier);
        let description = manifest.manifest.description.clone();
        let tool = RegisteredTool {
            id: tool_id,
            manifest,
            status: ToolStatus::Available,
        };
        self.name_index.insert(name.clone(), tool_id);
        self.tools.insert(tool_id, tool);

        if let Some(ref sender) = self.lifecycle_sender {
            if let Err(e) = sender.try_send(ToolLifecycleEvent::Installed {
                tool_id,
                tool_name: name.clone(),
                trust_tier,
                description,
            }) {
                tracing::warn!(error = %e, tool_name = %name, "Failed to send ToolInstalled notification");
            }
        }

        Ok(tool_id)
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
            self.loaded.retain(|lm| lm.manifest.manifest.name != name);

            if let Some(ref sender) = self.lifecycle_sender {
                if let Err(e) = sender.try_send(ToolLifecycleEvent::Removed {
                    tool_id: id,
                    tool_name: name.to_string(),
                }) {
                    tracing::warn!(error = %e, tool_name = %name, "Failed to send ToolRemoved notification");
                }
            }

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

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::tool::{ToolCapabilities, ToolInfo, ToolOutputs, ToolSchema};
    use tokio::sync::mpsc;

    fn make_core_manifest(name: &str) -> ToolManifest {
        ToolManifest {
            manifest: ToolInfo {
                name: name.to_string(),
                version: "0.1.0".to_string(),
                description: format!("Test tool {}", name),
                author: "test".to_string(),
                checksum: None,
                author_pubkey: None,
                signature: None,
                trust_tier: TrustTier::Core,
            },
            capabilities_required: ToolCapabilities {
                permissions: vec![],
            },
            capabilities_provided: ToolOutputs { outputs: vec![] },
            intent_schema: ToolSchema {
                input: "TestInput".to_string(),
                output: "TestOutput".to_string(),
            },
            input_schema: None,
            sandbox: ToolSandbox {
                network: false,
                fs_write: false,
                gpu: false,
                max_memory_mb: 64,
                max_cpu_ms: 5000,
                syscalls: vec![],
            },
            executor: ToolExecutor::default(),
        }
    }

    #[test]
    fn register_without_sender_succeeds() {
        let mut registry = ToolRegistry::new();
        let manifest = make_core_manifest("test-tool");
        assert!(registry.register(manifest).is_ok());
        assert!(registry.get_by_name("test-tool").is_some());
    }

    #[test]
    fn remove_without_sender_succeeds() {
        let mut registry = ToolRegistry::new();
        registry.register(make_core_manifest("test-tool")).unwrap();
        assert!(registry.remove("test-tool").is_ok());
        assert!(registry.get_by_name("test-tool").is_none());
    }

    #[test]
    fn register_sends_installed_notification() {
        let (tx, mut rx) = mpsc::channel(64);
        let mut registry = ToolRegistry::new();
        registry.set_lifecycle_sender(tx);
        let tool_id = registry.register(make_core_manifest("my-tool")).unwrap();
        let event = rx
            .try_recv()
            .expect("should receive Installed notification");
        match event {
            ToolLifecycleEvent::Installed {
                tool_id: id,
                tool_name,
                trust_tier,
                description,
            } => {
                assert_eq!(id, tool_id);
                assert_eq!(tool_name, "my-tool");
                assert_eq!(trust_tier, "Core");
                assert_eq!(description, "Test tool my-tool");
            }
            _ => panic!("Expected Installed variant"),
        }
    }

    #[test]
    fn remove_sends_removed_notification() {
        let (tx, mut rx) = mpsc::channel(64);
        let mut registry = ToolRegistry::new();
        registry.set_lifecycle_sender(tx);
        let tool_id = registry.register(make_core_manifest("rm-tool")).unwrap();
        let _ = rx.try_recv(); // consume Installed
        registry.remove("rm-tool").unwrap();
        let event = rx.try_recv().expect("should receive Removed notification");
        match event {
            ToolLifecycleEvent::Removed {
                tool_id: id,
                tool_name,
            } => {
                assert_eq!(id, tool_id);
                assert_eq!(tool_name, "rm-tool");
            }
            _ => panic!("Expected Removed variant"),
        }
    }

    #[test]
    fn remove_nonexistent_tool_returns_error() {
        let mut registry = ToolRegistry::new();
        assert!(registry.remove("nonexistent").is_err());
    }

    #[test]
    fn remove_prunes_loaded_vec() {
        let mut registry = ToolRegistry::new();
        registry.register(make_core_manifest("tool-a")).unwrap();
        registry.register(make_core_manifest("tool-b")).unwrap();
        // Simulate loaded entries (normally populated by load_from_dirs)
        registry.loaded.push(agentos_tools::loader::LoadedManifest {
            manifest: make_core_manifest("tool-a"),
            manifest_dir: std::path::PathBuf::from("/tmp/a"),
        });
        registry.loaded.push(agentos_tools::loader::LoadedManifest {
            manifest: make_core_manifest("tool-b"),
            manifest_dir: std::path::PathBuf::from("/tmp/b"),
        });
        assert_eq!(registry.loaded.len(), 2);

        registry.remove("tool-a").unwrap();
        assert_eq!(registry.loaded.len(), 1);
        assert_eq!(registry.loaded[0].manifest.manifest.name, "tool-b");
    }
}
