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
    /// Monotonic counter bumped on every mutation (register/unregister/remove).
    /// Consumers (semantic index, Tier-0 index) can detect stale state cheaply.
    revision: u64,
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

fn manifest_enabled_for_build(tool_name: &str) -> bool {
    match tool_name {
        "audio" => cfg!(feature = "audio"),
        "bluetooth" => cfg!(feature = "bluetooth"),
        "display-config" => cfg!(feature = "display"),
        "printer" => cfg!(feature = "printer"),
        "raw-usb" => cfg!(feature = "raw-usb"),
        "usb-storage" => cfg!(feature = "usb-storage"),
        "webcam" => cfg!(feature = "webcam"),
        _ => true,
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            name_index: HashMap::new(),
            loaded: Vec::new(),
            crl: RevocationList::new(),
            lifecycle_sender: None,
            revision: 0,
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
            revision: 0,
        }
    }

    /// Monotonic counter, bumped on every mutation (register/unregister/remove).
    /// Callers can cheaply detect stale cached state (semantic index, Tier-0 line).
    pub fn revision(&self) -> u64 {
        self.revision
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
    ///
    /// `trust_tier = "core"` is reserved for distribution-trusted manifests
    /// shipped under `core_dir`. A user-dir manifest declaring `trust_tier =
    /// "core"` is rejected with `ToolBlocked` to prevent privilege-tier
    /// laundering — without this gate, dropping a TOML file into
    /// `tools/user/` would skip the Ed25519 signature check that protects
    /// `Verified`/`Community` tiers, and would also satisfy the privileged-
    /// executor gate in `signing::verify_manifest`.
    pub fn load_from_dirs_with_crl(
        core_dir: &Path,
        user_dir: &Path,
        crl: RevocationList,
    ) -> Result<Self, AgentOSError> {
        let mut registry = Self::with_crl(crl);

        for (dir, is_core) in [(core_dir, true), (user_dir, false)] {
            if !dir.exists() {
                continue;
            }
            let manifests = load_all_manifests(dir)?;
            for loaded in manifests {
                let name = loaded.manifest.manifest.name.clone();
                if !manifest_enabled_for_build(&name) {
                    tracing::debug!(
                        tool = %name,
                        "Skipping manifest because corresponding kernel feature is disabled"
                    );
                    continue;
                }
                if !is_core && loaded.manifest.manifest.trust_tier == agentos_types::TrustTier::Core
                {
                    tracing::error!(
                        tool = %name,
                        path = %loaded.manifest_dir.display(),
                        "Rejecting user-dir manifest that claims trust_tier = core; \
                         core tier is reserved for distribution-shipped manifests"
                    );
                    return Err(AgentOSError::ToolBlocked { name });
                }
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
        self.revision += 1;

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

    /// Unregister a tool by its `ToolID`. No-op if the ID is not registered.
    /// Used by the plugin registry to clean up tools when a plugin is deactivated.
    pub fn unregister(&mut self, tool_id: &ToolID) {
        if let Some(tool) = self.tools.remove(tool_id) {
            self.name_index.remove(&tool.manifest.manifest.name);
            self.revision += 1;
            tracing::debug!(tool_id = %tool_id, name = %tool.manifest.manifest.name, "Tool unregistered");
        }
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
            self.revision += 1;

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

    /// Category breakdown of registered tools. Used for the compact L0 prompt.
    pub fn category_counts(&self) -> std::collections::BTreeMap<String, usize> {
        let mut counts = std::collections::BTreeMap::new();
        for tool in self.tools.values() {
            let cat = agentos_tools::agent_manual::AgentManualTool::infer_tool_category(
                &tool.manifest.manifest.name,
                &tool.manifest.manifest.capability_tags,
                tool.manifest.manifest.tags.as_deref(),
            );
            *counts.entry(cat).or_insert(0) += 1;
        }
        counts
    }

    /// Compact L0 tool catalogue for system prompts — one line listing category counts.
    pub fn tools_for_prompt(&self) -> String {
        if self.tools.is_empty() {
            return "No tools available.".to_string();
        }
        let counts = self.category_counts();
        let total: usize = counts.values().sum();
        let parts: Vec<String> = counts.iter().map(|(cat, n)| format!("{cat}:{n}")).collect();
        format!(
            "Tools ({total}): {counts}. Use list-tools(category=<name>|tag=<tag>|page=N) · search-tools(query=...) · describe-tool(name=...) to explore. Note: dynamic MCP tools may not appear; run `agentos mcp list` for all MCP servers.",
            total = total,
            counts = parts.join(" ")
        )
    }

    /// Compact L0 tool catalogue with usage-ranked top-N names per category.
    ///
    /// `usage` maps tool-name -> decayed usage score (empty -> name-sorted). The
    /// grouping reuses the same `infer_tool_category` logic as
    /// [`Self::category_counts`], so the per-category counts here can never drift
    /// from `category_counts`/`tools_for_prompt`.
    ///
    /// Bounded two ways: at most `max_names_per_category` names per category, and
    /// a soft total budget of `max_tokens` (≈4 chars/token) over the names
    /// portion — once exceeded, the remaining (alphabetically later) categories
    /// degrade to counts-only so the line can never grow unbounded as the tool
    /// set grows.
    pub fn tools_for_prompt_ranked(
        &self,
        usage: &std::collections::HashMap<String, f64>,
        max_names_per_category: usize,
        max_tokens: usize,
    ) -> String {
        if self.tools.is_empty() {
            return "No tools available.".to_string();
        }
        let mut by_cat: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();
        for tool in self.tools.values() {
            let cat = agentos_tools::agent_manual::AgentManualTool::infer_tool_category(
                &tool.manifest.manifest.name,
                &tool.manifest.manifest.capability_tags,
                tool.manifest.manifest.tags.as_deref(),
            );
            by_cat
                .entry(cat)
                .or_default()
                .push(tool.manifest.manifest.name.clone());
        }
        let total: usize = by_cat.values().map(Vec::len).sum();
        let names_budget = max_tokens.saturating_mul(4);
        let per_cat = max_names_per_category.max(1);

        let mut parts: Vec<String> = Vec::with_capacity(by_cat.len());
        let mut used = 0usize;
        for (cat, mut names) in by_cat {
            let count = names.len();
            // Usage-ranked, with a name tie-break so the order is a deterministic
            // total order even when a score is missing or NaN.
            names.sort_by(|a, b| {
                let ua = usage.get(a).copied().unwrap_or(0.0);
                let ub = usage.get(b).copied().unwrap_or(0.0);
                ub.partial_cmp(&ua)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.cmp(b))
            });
            let shown = names.len().min(per_cat);
            let more = count.saturating_sub(shown);
            let suffix = if more > 0 {
                format!(" +{more}")
            } else {
                String::new()
            };
            let with_names = format!("{cat}({count}): {}{suffix}", names[..shown].join(", "));
            // Degrade to counts-only once the names budget is spent.
            let part = if used + with_names.len() <= names_budget {
                used += with_names.len() + 2; // +2 ≈ "; " separator
                with_names
            } else {
                format!("{cat}({count})")
            };
            parts.push(part);
        }
        format!(
            "Tools ({total}): {}. Use list-tools(category=<name>|tag=<tag>|page=N) · search-tools(query=...) · describe-tool(name=...) to explore. Note: dynamic MCP tools may not appear; run `agentos mcp list` for all MCP servers.",
            parts.join("; ")
        )
    }

    /// Get the full per-tool prompt block listing (verbose form).
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
    pub fn tools_for_prompt_verbose(&self) -> String {
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

            let input_line = match compact_input_schema(tool.manifest.payload_schema.as_ref()) {
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
                tags: None,
                capability_tags: vec![],
                group: String::new(),
            },
            capabilities_required: ToolCapabilities {
                permissions: vec![],
            },
            capabilities_provided: ToolOutputs { outputs: vec![] },
            intent_schema: ToolSchema {
                input: "TestInput".to_string(),
                output: "TestOutput".to_string(),
            },
            payload_schema: None,
            examples: vec![],
            sandbox: ToolSandbox {
                network: false,
                fs_write: false,
                gpu: false,
                max_memory_mb: 64,
                max_cpu_ms: 5000,
                syscalls: vec![],
                weight: None,
            },
            executor: ToolExecutor::default(),
            fallbacks: vec![],
            risk_class: Default::default(),
            usage_hints: None,
            tags: vec![],
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
                tags: None,
                capability_tags: vec![],
                group: String::new(),
            },
            capabilities_required: ToolCapabilities {
                permissions: vec![],
            },
            capabilities_provided: ToolOutputs { outputs: vec![] },
            intent_schema: ToolSchema {
                input: "TestInput".to_string(),
                output: "TestOutput".to_string(),
            },
            payload_schema: None,
            examples: vec![],
            sandbox: ToolSandbox {
                network: false,
                fs_write: false,
                gpu: false,
                max_memory_mb: 64,
                max_cpu_ms: 5000,
                syscalls: vec![],
                weight: None,
            },
            executor: ToolExecutor::default(),
            fallbacks: vec![],
            risk_class: Default::default(),
            usage_hints: None,
            tags: vec![],
        }
    }

    #[test]
    fn peripheral_manifest_filters_match_compiled_features() {
        assert_eq!(manifest_enabled_for_build("audio"), cfg!(feature = "audio"));
        assert_eq!(
            manifest_enabled_for_build("bluetooth"),
            cfg!(feature = "bluetooth")
        );
        assert_eq!(
            manifest_enabled_for_build("display-config"),
            cfg!(feature = "display")
        );
        assert_eq!(
            manifest_enabled_for_build("raw-usb"),
            cfg!(feature = "raw-usb")
        );
        assert_eq!(
            manifest_enabled_for_build("usb-storage"),
            cfg!(feature = "usb-storage")
        );
        assert_eq!(
            manifest_enabled_for_build("webcam"),
            cfg!(feature = "webcam")
        );
        assert!(manifest_enabled_for_build("file-reader"));
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
        manifest.payload_schema = Some(serde_json::json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": { "type": "string" },
                "offset": { "type": "integer" }
            }
        }));
        registry.register(manifest).unwrap();

        let prompt = registry.tools_for_prompt_verbose();
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

        let prompt = registry.tools_for_prompt_verbose();
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

        let prompt = registry.tools_for_prompt_verbose();
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

        let prompt = registry.tools_for_prompt_verbose();
        let alpha_pos = prompt.find("## alpha").expect("alpha missing");
        let zeta_pos = prompt.find("## zeta").expect("zeta missing");
        assert!(alpha_pos < zeta_pos, "alpha should appear before zeta");
    }

    #[test]
    fn tools_for_prompt_returns_no_tools_message_when_empty() {
        let registry = ToolRegistry::new();
        assert_eq!(registry.tools_for_prompt_verbose(), "No tools available.");
    }

    #[test]
    fn tools_for_prompt_ranked_empty_registry() {
        let registry = ToolRegistry::new();
        assert_eq!(
            registry.tools_for_prompt_ranked(&std::collections::HashMap::new(), 5, 200),
            "No tools available."
        );
    }

    #[test]
    fn tools_for_prompt_ranked_total_matches_category_counts() {
        let mut registry = ToolRegistry::new();
        registry
            .register(make_core_manifest("memory-write"))
            .unwrap();
        registry
            .register(make_core_manifest("memory-search"))
            .unwrap();
        registry.register(make_core_manifest("core-tool")).unwrap();
        // Counts in the ranked line must equal category_counts (same grouping).
        let total: usize = registry.category_counts().values().sum();
        let line = registry.tools_for_prompt_ranked(&std::collections::HashMap::new(), 5, 200);
        assert_eq!(total, 3);
        assert!(
            line.starts_with(&format!("Tools ({total}):")),
            "got: {line}"
        );
    }

    #[test]
    fn tools_for_prompt_ranked_caps_names_and_marks_overflow() {
        let mut registry = ToolRegistry::new();
        for n in [
            "memory-write",
            "memory-search",
            "memory-read",
            "memory-delete",
        ] {
            registry.register(make_core_manifest(n)).unwrap();
        }
        // 4 memory tools, only 2 names shown → "+2" overflow marker.
        let line = registry.tools_for_prompt_ranked(&std::collections::HashMap::new(), 2, 200);
        assert!(line.contains("memory(4):"), "got: {line}");
        assert!(line.contains("+2"), "expected overflow marker, got: {line}");
    }

    #[test]
    fn tools_for_prompt_ranked_surfaces_high_usage_name() {
        let mut registry = ToolRegistry::new();
        for n in ["memory-write", "memory-search", "memory-read"] {
            registry.register(make_core_manifest(n)).unwrap();
        }
        let mut usage = std::collections::HashMap::new();
        usage.insert("memory-read".to_string(), 99.0);
        // max_names=1 → only the highest-usage memory tool is named.
        let line = registry.tools_for_prompt_ranked(&usage, 1, 200);
        assert!(line.contains("memory-read"), "got: {line}");
        assert!(
            !line.contains("memory-search"),
            "low-usage tool must be hidden at max_names=1, got: {line}"
        );
    }

    #[test]
    fn tools_for_prompt_ranked_is_deterministic() {
        let mut registry = ToolRegistry::new();
        for n in ["memory-write", "core-a", "core-b"] {
            registry.register(make_core_manifest(n)).unwrap();
        }
        let usage = std::collections::HashMap::new();
        let a = registry.tools_for_prompt_ranked(&usage, 5, 200);
        let b = registry.tools_for_prompt_ranked(&usage, 5, 200);
        assert_eq!(a, b);
    }

    #[test]
    fn tools_for_prompt_ranked_degrades_to_counts_only_past_budget() {
        let mut registry = ToolRegistry::new();
        for n in ["memory-write", "memory-search", "core-a", "core-b"] {
            registry.register(make_core_manifest(n)).unwrap();
        }
        // max_tokens=1 → ~4-char names budget → no category's names fit → the
        // line degrades to counts-only (no "category(N): names" colon form).
        let line = registry.tools_for_prompt_ranked(&std::collections::HashMap::new(), 5, 1);
        assert!(line.contains("memory(2)"), "got: {line}");
        assert!(line.contains("core(2)"), "got: {line}");
        assert!(
            !line.contains("memory(2):"),
            "expected counts-only under a 0 budget, got: {line}"
        );
    }

    #[test]
    fn tools_for_prompt_ranked_handles_nan_score_without_panic() {
        let mut registry = ToolRegistry::new();
        for n in ["memory-alpha", "memory-beta"] {
            registry.register(make_core_manifest(n)).unwrap();
        }
        let mut usage = std::collections::HashMap::new();
        usage.insert("memory-alpha".to_string(), f64::NAN);
        usage.insert("memory-beta".to_string(), 1.0);
        // NaN compares Equal in the comparator; the name tie-break keeps the sort
        // a total order, so this must not panic and both names appear.
        let line = registry.tools_for_prompt_ranked(&usage, 5, 200);
        assert!(line.contains("memory-alpha"), "got: {line}");
        assert!(line.contains("memory-beta"), "got: {line}");
    }

    #[test]
    fn registry_revision_bumps_on_register_unregister_remove() {
        let mut registry = ToolRegistry::new();
        assert_eq!(registry.revision(), 0);
        let id = registry.register(make_core_manifest("rev-tool-a")).unwrap();
        assert_eq!(registry.revision(), 1);
        registry.register(make_core_manifest("rev-tool-b")).unwrap();
        assert_eq!(registry.revision(), 2);
        registry.unregister(&id);
        assert_eq!(registry.revision(), 3);
        registry.register(make_core_manifest("rev-tool-c")).unwrap();
        registry.remove("rev-tool-c").unwrap();
        assert_eq!(registry.revision(), 5);
        // No-op unregister doesn't bump.
        registry.unregister(&id);
        assert_eq!(registry.revision(), 5);
    }

    #[test]
    fn core_manifests_have_taxonomy_tag_and_meta_tools_keep_meta() {
        // Loads the shipped tools/core manifests and enforces the Phase-4 tag
        // contract: every manifest's top-level `tags` must carry AT LEAST ONE
        // MANIFEST_TAG_TAXONOMY_V1 value (extra free-form tags like "control" /
        // "privileged" are allowed), and the meta-tagged discovery/coordination
        // tools must keep `meta` (the Phase-3 scoping escape hatch).
        let core = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/core");
        let user = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/__none__");
        let registry = ToolRegistry::load_from_dirs(&core, &user).expect("core manifests load");
        let taxonomy = agentos_types::tool::MANIFEST_TAG_TAXONOMY_V1;
        for tool in registry.list_all() {
            let name = &tool.manifest.manifest.name;
            let tags = &tool.manifest.tags;
            assert!(
                !tags.is_empty(),
                "core manifest '{name}' has empty top-level tags"
            );
            assert!(
                tags.iter().any(|t| taxonomy.contains(&t.as_str())),
                "core manifest '{name}' has no MANIFEST_TAG_TAXONOMY_V1 tag (has {tags:?})"
            );
        }
        for name in [
            "search-tools",
            "describe-tool",
            "list-tools",
            "agent-self",
            "agent-manual",
            "spawn-agent",
            "await-agents",
            "escalation-status",
        ] {
            if let Some(tool) = registry.get_by_name(name) {
                assert!(
                    tool.manifest.tags.iter().any(|t| t == "meta"),
                    "meta tool '{name}' lost its `meta` tag"
                );
            }
        }
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

        let prompt = registry.tools_for_prompt_verbose();
        assert!(
            prompt.contains("Permissions: fs.user_data:r, memory.semantic:w"),
            "multiple permissions should be joined with ', '"
        );
    }
}
