use agentos_tools::loader::{load_all_manifests, LoadedManifest};
use agentos_tools::signing::{verify_manifest_with_crl, RevocationList};
use agentos_types::*;
use std::collections::{HashMap, HashSet};
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
    ChecksumMismatch {
        tool_name: String,
        expected: String,
        actual: String,
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

fn schema_type_for_prompt(field_schema: &serde_json::Value) -> String {
    if let Some(type_value) = field_schema.get("type") {
        if let Some(type_name) = type_value.as_str() {
            return type_name.to_string();
        }
        if let Some(type_arr) = type_value.as_array() {
            let mut names: Vec<String> = type_arr
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect();
            names.sort();
            names.dedup();
            if !names.is_empty() {
                return names.join("|");
            }
        }
    }
    if field_schema.get("enum").is_some() {
        return "enum".to_string();
    }
    if field_schema.get("oneOf").is_some() {
        return "oneOf".to_string();
    }
    if field_schema.get("anyOf").is_some() {
        return "anyOf".to_string();
    }
    "any".to_string()
}

fn compact_input_schema(schema: Option<&serde_json::Value>) -> Option<String> {
    let schema = schema?;
    let obj = schema.as_object()?;

    let required: HashSet<String> = obj
        .get("required")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let mut parts: Vec<String> = Vec::new();
    if let Some(properties) = obj.get("properties").and_then(|v| v.as_object()) {
        let mut names: Vec<&String> = properties.keys().collect();
        names.sort();

        for name in names.iter().take(8) {
            if let Some(field_schema) = properties.get(*name) {
                let type_name = schema_type_for_prompt(field_schema);
                let opt = if required.contains(name.as_str()) {
                    ""
                } else {
                    "?"
                };
                parts.push(format!("{}{}:{}", name, opt, type_name));
            }
        }
        if properties.len() > 8 {
            parts.push(format!("+{} more", properties.len() - 8));
        }
    }

    if parts.is_empty() {
        if let Some(required_arr) = obj.get("required").and_then(|v| v.as_array()) {
            if !required_arr.is_empty() {
                let required_names: Vec<String> = required_arr
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect();
                return Some(format!("required {}", required_names.join(",")));
            }
        }
        return Some("object".to_string());
    }

    Some(format!("{{{}}}", parts.join(", ")))
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
        if let Err(e) = verify_manifest_with_crl(&manifest, &self.crl) {
            if let AgentOSError::ToolSignatureInvalid { .. } = &e {
                if let Some(ref sender) = self.lifecycle_sender {
                    let _ = sender.try_send(ToolLifecycleEvent::ChecksumMismatch {
                        tool_name: manifest.manifest.name.clone(),
                        expected: manifest.manifest.checksum.clone().unwrap_or_default(),
                        actual: e.to_string(),
                    });
                }
            }
            return Err(e);
        }

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
    ///
    /// Each tool is rendered as a multi-line block:
    /// ```text
    /// ## tool-name
    /// Description text
    /// Permissions: perm1, perm2
    /// Input: {field:type, optional?:type}
    /// ```
    /// Blocks are separated by blank lines. Tools without an input schema show a
    /// fallback directing the agent to `agent-manual tool-detail`.
    pub fn tools_for_prompt(&self) -> String {
        let mut sorted_tools: Vec<&RegisteredTool> = self.tools.values().collect();
        sorted_tools.sort_by(|a, b| a.manifest.manifest.name.cmp(&b.manifest.manifest.name));

        if sorted_tools.is_empty() {
            return "No tools available.".to_string();
        }

        let mut sections: Vec<String> = Vec::new();
        for tool in sorted_tools {
            let mut block = Vec::new();
            block.push(format!("## {}", tool.manifest.manifest.name));
            block.push(tool.manifest.manifest.description.clone());

            let perms = &tool.manifest.capabilities_required.permissions;
            if !perms.is_empty() {
                block.push(format!("Permissions: {}", perms.join(", ")));
            }

            let input_line = match compact_input_schema(tool.manifest.input_schema.as_ref()) {
                Some(schema_summary) => format!("Input: {}", schema_summary),
                None => "Input: (see agent-manual tool-detail)".to_string(),
            };
            block.push(input_line);

            sections.push(block.join("\n"));
        }
        sections.join("\n\n")
    }

    /// Return all tools whose required permissions include the given capability prefix.
    ///
    /// Matches against the resource class hierarchy: the prefix must end at a `.` or `:`
    /// boundary (or match the entire permission string exactly). This ensures `"fs"` matches
    /// `"fs.user_data:r"` but not a hypothetical `"fsstats:x"`.
    ///
    /// Comparison is case-insensitive. Results are sorted by tool name. An empty prefix
    /// returns all tools that have at least one permission.
    ///
    /// This is useful for agents asking "which tools can write files?" or
    /// "which tools can access the network?".
    pub fn search_by_capability(&self, capability_prefix: &str) -> Vec<&RegisteredTool> {
        let prefix_lower = capability_prefix.to_lowercase();
        let mut tools: Vec<&RegisteredTool> = self
            .tools
            .values()
            .filter(|t| {
                t.manifest
                    .capabilities_required
                    .permissions
                    .iter()
                    .any(|p| {
                        let p_lower = p.to_lowercase();
                        if p_lower == prefix_lower {
                            return true;
                        }
                        if p_lower.starts_with(&prefix_lower) {
                            // Require a segment boundary after the prefix
                            matches!(
                                p_lower.as_bytes().get(prefix_lower.len()),
                                Some(b'.' | b':')
                            )
                        } else {
                            false
                        }
                    })
            })
            .collect();
        tools.sort_by(|a, b| a.manifest.manifest.name.cmp(&b.manifest.manifest.name));
        tools
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

    fn make_community_manifest_bad_sig(name: &str) -> ToolManifest {
        ToolManifest {
            manifest: ToolInfo {
                name: name.to_string(),
                version: "0.1.0".to_string(),
                description: format!("Test community tool {}", name),
                author: "test".to_string(),
                checksum: Some("deadbeef".to_string()),
                author_pubkey: Some("notavalidpubkey".to_string()),
                signature: Some("notavalidsig".to_string()),
                trust_tier: TrustTier::Community,
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

    #[test]
    fn register_sends_checksum_mismatch_notification_on_invalid_signature() {
        let (tx, mut rx) = mpsc::channel(64);
        let mut registry = ToolRegistry::new();
        registry.set_lifecycle_sender(tx);
        let manifest = make_community_manifest_bad_sig("bad-sig-tool");
        let result = registry.register(manifest);
        assert!(result.is_err(), "register should fail on invalid signature");
        let event = rx
            .try_recv()
            .expect("should receive ChecksumMismatch notification");
        match event {
            ToolLifecycleEvent::ChecksumMismatch {
                tool_name,
                expected,
                ..
            } => {
                assert_eq!(tool_name, "bad-sig-tool");
                assert_eq!(expected, "deadbeef");
            }
            _ => panic!("Expected ChecksumMismatch variant, got {:?}", event),
        }
    }

    #[test]
    fn tools_for_prompt_includes_compact_schema_summary() {
        let mut registry = ToolRegistry::new();
        let mut manifest = make_core_manifest("file-reader");
        manifest.manifest.description = "Read files".into();
        manifest.capabilities_required.permissions = vec!["fs.user_data:r".to_string()];
        manifest.input_schema = Some(serde_json::json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": { "type": "string" },
                "offset": { "type": "integer" }
            }
        }));
        registry.register(manifest).unwrap();

        let prompt = registry.tools_for_prompt();
        assert!(prompt.contains("## file-reader"), "should have ## heading");
        assert!(prompt.contains("Read files"), "should have description");
        assert!(
            prompt.contains("Permissions: fs.user_data:r"),
            "should have permissions"
        );
        assert!(
            prompt.contains("Input: {offset?:integer, path:string}"),
            "should have compact schema"
        );
    }

    #[test]
    fn tools_for_prompt_shows_fallback_when_no_schema() {
        let mut registry = ToolRegistry::new();
        registry
            .register(make_core_manifest("no-schema-tool"))
            .unwrap();

        let prompt = registry.tools_for_prompt();
        assert!(
            prompt.contains("Input: (see agent-manual tool-detail)"),
            "should fall back when schema is absent"
        );
    }

    #[test]
    fn tools_for_prompt_omits_permissions_line_when_empty() {
        let mut registry = ToolRegistry::new();
        // make_core_manifest has empty permissions by default
        let manifest = make_core_manifest("no-perms-tool");
        assert!(manifest.capabilities_required.permissions.is_empty());
        registry.register(manifest).unwrap();

        let prompt = registry.tools_for_prompt();
        assert!(
            !prompt.contains("Permissions:"),
            "should not emit Permissions line when empty"
        );
    }

    #[test]
    fn tools_for_prompt_is_sorted_by_tool_name() {
        let mut registry = ToolRegistry::new();
        registry.register(make_core_manifest("zeta")).unwrap();
        registry.register(make_core_manifest("alpha")).unwrap();

        let prompt = registry.tools_for_prompt();
        let alpha_pos = prompt.find("## alpha").expect("alpha missing");
        let zeta_pos = prompt.find("## zeta").expect("zeta missing");
        assert!(alpha_pos < zeta_pos, "alpha should appear before zeta");
    }

    #[test]
    fn tools_for_prompt_returns_no_tools_message_when_empty() {
        let registry = ToolRegistry::new();
        assert_eq!(registry.tools_for_prompt(), "No tools available.");
    }

    #[test]
    fn search_by_capability_returns_matching_tools() {
        let mut registry = ToolRegistry::new();

        let mut fs_tool = make_core_manifest("file-reader");
        fs_tool.capabilities_required.permissions = vec!["fs.user_data:r".to_string()];
        registry.register(fs_tool).unwrap();

        let mut mem_tool = make_core_manifest("memory-search");
        mem_tool.capabilities_required.permissions = vec!["memory.semantic:r".to_string()];
        registry.register(mem_tool).unwrap();

        let mut net_tool = make_core_manifest("http-client");
        net_tool.capabilities_required.permissions = vec!["network.outbound:x".to_string()];
        registry.register(net_tool).unwrap();

        let fs_results = registry.search_by_capability("fs");
        assert_eq!(fs_results.len(), 1);
        assert_eq!(fs_results[0].manifest.manifest.name, "file-reader");

        let mem_results = registry.search_by_capability("memory");
        assert_eq!(mem_results.len(), 1);
        assert_eq!(mem_results[0].manifest.manifest.name, "memory-search");

        let none_results = registry.search_by_capability("vault");
        assert!(none_results.is_empty());
    }

    #[test]
    fn search_by_capability_is_case_insensitive() {
        let mut registry = ToolRegistry::new();
        let mut tool = make_core_manifest("fs-tool");
        tool.capabilities_required.permissions = vec!["fs.user_data:r".to_string()];
        registry.register(tool).unwrap();

        assert_eq!(registry.search_by_capability("FS").len(), 1);
        assert_eq!(registry.search_by_capability("Fs").len(), 1);
        assert_eq!(registry.search_by_capability("fs").len(), 1);
    }

    #[test]
    fn search_by_capability_results_sorted_by_name() {
        let mut registry = ToolRegistry::new();

        let mut tool_z = make_core_manifest("zeta-reader");
        tool_z.capabilities_required.permissions = vec!["fs.user_data:r".to_string()];
        registry.register(tool_z).unwrap();

        let mut tool_a = make_core_manifest("alpha-reader");
        tool_a.capabilities_required.permissions = vec!["fs.user_data:r".to_string()];
        registry.register(tool_a).unwrap();

        let results = registry.search_by_capability("fs");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].manifest.manifest.name, "alpha-reader");
        assert_eq!(results[1].manifest.manifest.name, "zeta-reader");
    }

    #[test]
    fn search_by_capability_boundary_aware() {
        let mut registry = ToolRegistry::new();
        let mut tool = make_core_manifest("mem-tool");
        tool.capabilities_required.permissions = vec!["memory.semantic:r".to_string()];
        registry.register(tool).unwrap();

        // "memory" matches at the `.` boundary
        assert_eq!(registry.search_by_capability("memory").len(), 1);
        // "mem" is a raw prefix of "memory.semantic:r" but does not end at `.` or `:`,
        // so boundary-aware matching rejects it
        assert_eq!(registry.search_by_capability("mem").len(), 0);
        // unrelated prefix never matches
        assert_eq!(registry.search_by_capability("net").len(), 0);
        // exact full match also works
        assert_eq!(registry.search_by_capability("memory.semantic:r").len(), 1);
    }

    #[test]
    fn search_by_capability_does_not_match_partial_segment() {
        let mut registry = ToolRegistry::new();
        let mut tool = make_core_manifest("fsstats-tool");
        // permission starts with "fs" but the segment is "fsstats", not "fs"
        tool.capabilities_required.permissions = vec!["fsstats.read:r".to_string()];
        registry.register(tool).unwrap();

        // "fs" must not match "fsstats.read:r" because "s" follows, not "." or ":"
        assert_eq!(registry.search_by_capability("fs").len(), 0);
        // The full first segment matches
        assert_eq!(registry.search_by_capability("fsstats").len(), 1);
    }

    #[test]
    fn search_by_capability_multi_permission_tool() {
        let mut registry = ToolRegistry::new();
        let mut tool = make_core_manifest("hybrid-tool");
        tool.capabilities_required.permissions = vec![
            "fs.user_data:r".to_string(),
            "memory.semantic:w".to_string(),
        ];
        registry.register(tool).unwrap();

        // Tool appears in results for both capability prefixes
        let fs_results = registry.search_by_capability("fs");
        assert_eq!(fs_results.len(), 1);
        assert_eq!(fs_results[0].manifest.manifest.name, "hybrid-tool");

        let mem_results = registry.search_by_capability("memory");
        assert_eq!(mem_results.len(), 1);
        assert_eq!(mem_results[0].manifest.manifest.name, "hybrid-tool");
    }

    #[test]
    fn tools_for_prompt_multiple_permissions_joined() {
        let mut registry = ToolRegistry::new();
        let mut manifest = make_core_manifest("multi-perm-tool");
        manifest.capabilities_required.permissions = vec![
            "fs.user_data:r".to_string(),
            "memory.semantic:w".to_string(),
        ];
        registry.register(manifest).unwrap();

        let prompt = registry.tools_for_prompt();
        assert!(
            prompt.contains("Permissions: fs.user_data:r, memory.semantic:w"),
            "multiple permissions should be joined with ', '"
        );
    }
}
