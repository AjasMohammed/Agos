use agentos_types::{reject_traversal, PluginManifest, ToolID, TrustTier};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::hooks::HookRegistry;
use crate::tool_registry::ToolRegistry;

/// Verify an Ed25519 plugin signature.
/// Returns `true` if the signature is valid over the canonical manifest JSON.
fn verify_plugin_signature(manifest: &PluginManifest, pubkey_hex: &str, sig_hex: &str) -> bool {
    use ed25519_dalek::{Signature, VerifyingKey};

    let Ok(pubkey_bytes) = hex::decode(pubkey_hex) else {
        return false;
    };
    let Ok(sig_bytes) = hex::decode(sig_hex) else {
        return false;
    };
    let Ok(pubkey_arr): Result<[u8; 32], _> = pubkey_bytes.try_into() else {
        return false;
    };
    let Ok(sig_arr): Result<[u8; 64], _> = sig_bytes.try_into() else {
        return false;
    };
    let Ok(verifying_key) = VerifyingKey::from_bytes(&pubkey_arr) else {
        return false;
    };
    let signature = Signature::from_bytes(&sig_arr);

    // Build a stable canonical payload using serde_json with sorted keys.
    // Covers all security-relevant fields so partial tampering invalidates the sig.
    // Uses serde serialization (not {Debug}) for stability across Rust versions.
    let payload_map = serde_json::json!({
        "id": manifest.id,
        "version": manifest.version,
        "trust_tier": manifest.trust_tier,
        "permissions": manifest.permissions,
        "tools": manifest.tools,
        "channels": manifest.channels.iter().map(|c| &c.id).collect::<Vec<_>>(),
        "memory_backend": manifest.memory_backend,
    });
    let payload = serde_json::to_string(&payload_map).unwrap_or_default();

    use ed25519_dalek::Verifier;
    verifying_key.verify(payload.as_bytes(), &signature).is_ok()
}

/// Runtime status of a discovered plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginStatus {
    /// Manifest read, no code loaded.
    Discovered,
    /// Fully loaded and running.
    Active,
    /// User explicitly disabled.
    Disabled,
    /// Blocked: trust_tier = Blocked or signature verification failed.
    Blocked { reason: String },
}

/// An entry in the plugin registry.
#[derive(Debug, Clone)]
pub struct PluginEntry {
    pub manifest: PluginManifest,
    /// Absolute path to the `plugin.toml` file.
    pub manifest_path: PathBuf,
    pub status: PluginStatus,
    /// Tool IDs registered by this plugin (populated on activation, cleared on deactivation).
    pub registered_tool_ids: Vec<ToolID>,
}

/// Registry that discovers plugins from manifests and lazily activates them.
///
/// Discovery is fast (TOML reads only, no code execution). Activation loads
/// and registers the plugin's tools into the `ToolRegistry`.
pub struct PluginRegistry {
    entries: RwLock<HashMap<String, PluginEntry>>,
    hook_registry: Arc<HookRegistry>,
    tool_registry: Arc<RwLock<ToolRegistry>>,
}

impl PluginRegistry {
    pub fn new(
        hook_registry: Arc<HookRegistry>,
        tool_registry: Arc<RwLock<ToolRegistry>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            entries: RwLock::new(HashMap::new()),
            hook_registry,
            tool_registry,
        })
    }

    /// Scan `dirs` for `plugin.toml` files. Returns the count of newly discovered plugins.
    /// Already-known plugins are skipped. Blocked plugins are flagged but counted.
    pub async fn discover(&self, dirs: &[PathBuf]) -> usize {
        let mut count = 0;
        for dir in dirs {
            count += self.scan_directory(dir).await;
        }
        count
    }

    async fn scan_directory(&self, dir: &Path) -> usize {
        // Collect candidate paths on a blocking thread, then process async.
        let dir_buf = dir.to_path_buf();
        let candidates = tokio::task::spawn_blocking(move || {
            let Ok(read_dir) = std::fs::read_dir(&dir_buf) else {
                return vec![];
            };
            let mut paths = Vec::new();
            for entry in read_dir.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let nested = path.join("plugin.toml");
                    if nested.exists() {
                        paths.push(nested);
                    }
                } else if path.file_name() == Some(std::ffi::OsStr::new("plugin.toml")) {
                    paths.push(path);
                }
            }
            paths
        })
        .await
        .unwrap_or_else(|e| {
            warn!(error = %e, "Plugin directory scan task failed");
            vec![]
        });

        let mut count = 0;
        for path in &candidates {
            if self.load_manifest(path).await.is_some() {
                count += 1;
            }
        }
        count
    }

    async fn load_manifest(&self, path: &Path) -> Option<()> {
        // Read and parse on a blocking thread to avoid stalling the async runtime.
        let path_buf = path.to_path_buf();
        let path_str = path.display().to_string();
        let (content, manifest) = tokio::task::spawn_blocking(move || {
            let content = std::fs::read_to_string(&path_buf)?;
            let manifest: PluginManifest = toml::from_str(&content)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            Ok::<_, std::io::Error>((content, manifest))
        })
        .await
        .ok()
        .and_then(|r| match r {
            Ok((c, m)) => Some((c, m)),
            Err(e) => {
                warn!(path = %path_str, error = %e, "Failed to load plugin manifest");
                None
            }
        })?;
        let _ = content; // raw TOML content; reserved for whole-file sig verification if needed

        // Validate plugin ID: must be non-empty, kebab-case, no path traversal.
        let id = &manifest.id;
        if id.is_empty()
            || reject_traversal(id).is_err()
            || id.contains('/')
            || id.contains('\\')
            || !id
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        {
            warn!(plugin_id = %id, "Rejected plugin with invalid ID");
            return None;
        }

        // Determine initial status: validate trust tier and signature.
        let status = match manifest.trust_tier {
            TrustTier::Blocked => PluginStatus::Blocked {
                reason: "trust_tier = blocked in manifest".to_string(),
            },
            // Core plugins ship in the binary distribution — no sig needed.
            TrustTier::Core => PluginStatus::Discovered,
            // Community and Verified require a valid Ed25519 signature.
            TrustTier::Community | TrustTier::Verified => {
                match (&manifest.author_pubkey, &manifest.signature) {
                    (Some(pubkey_hex), Some(sig_hex)) => {
                        if verify_plugin_signature(&manifest, pubkey_hex, sig_hex) {
                            PluginStatus::Discovered
                        } else {
                            PluginStatus::Blocked {
                                reason: "Ed25519 signature verification failed".to_string(),
                            }
                        }
                    }
                    _ => PluginStatus::Blocked {
                        reason:
                            "Community/Verified plugins must include author_pubkey and signature"
                                .to_string(),
                    },
                }
            }
        };

        let id = manifest.id.clone();
        let mut entries = self.entries.write().await;
        if entries.contains_key(&id) {
            // Duplicate plugin ID — first manifest wins; log so users know.
            warn!(
                plugin_id = %id,
                path = %path.display(),
                "Skipping duplicate plugin manifest (ID already registered)"
            );
            return None;
        }
        info!(plugin_id = %id, status = ?status, "Discovered plugin");
        entries.insert(
            id,
            PluginEntry {
                manifest,
                manifest_path: path.to_path_buf(),
                status,
                registered_tool_ids: Vec::new(),
            },
        );
        Some(())
    }

    /// Activate a plugin by ID. No-op if already active.
    /// Loads the plugin's declared tool manifests and registers them in the ToolRegistry.
    /// Returns `Err` if the plugin is blocked or unknown.
    /// Disabled plugins can be re-activated.
    ///
    /// Uses a single write-lock for the entire check-and-update to prevent
    /// TOCTOU races from concurrent `activate` calls on the same plugin.
    pub async fn activate(&self, plugin_id: &str) -> anyhow::Result<()> {
        // Collect needed info under lock, then drop lock before loading tools (blocking I/O).
        let (manifest_dir, tool_paths) = {
            let mut entries = self.entries.write().await;
            let entry = entries
                .get_mut(plugin_id)
                .ok_or_else(|| anyhow::anyhow!("Plugin '{}' not found", plugin_id))?;

            match &entry.status {
                PluginStatus::Active => return Ok(()), // no-op
                PluginStatus::Blocked { reason } => {
                    anyhow::bail!("Plugin '{}' is blocked: {}", plugin_id, reason)
                }
                PluginStatus::Discovered | PluginStatus::Disabled => {}
            }

            // Collect tool manifest paths relative to the plugin manifest directory.
            let manifest_dir = entry
                .manifest_path
                .parent()
                .unwrap_or(&entry.manifest_path)
                .to_path_buf();
            let tool_paths: Vec<PathBuf> = entry
                .manifest
                .tools
                .iter()
                .map(|rel| manifest_dir.join(rel))
                .collect();

            // Mark active now — tools will be registered below.
            entry.status = PluginStatus::Active;
            (manifest_dir, tool_paths)
        };

        // Load and register tool manifests (blocking I/O outside lock).
        let mut registered = Vec::new();
        for tool_path in &tool_paths {
            let path = tool_path.clone();
            let loaded =
                tokio::task::spawn_blocking(move || agentos_tools::loader::load_manifest(&path))
                    .await
                    .ok()
                    .and_then(|r| r.ok());

            if let Some(loaded) = loaded {
                let tool_name = loaded.manifest.manifest.name.clone();
                let mut tr = self.tool_registry.write().await;
                match tr.register(loaded.manifest) {
                    Ok(tool_id) => {
                        registered.push(tool_id);
                        info!(plugin_id = %plugin_id, tool = %tool_name, "Registered tool from plugin");
                    }
                    Err(e) => {
                        warn!(plugin_id = %plugin_id, tool = %tool_name, error = %e, "Failed to register tool from plugin");
                    }
                }
            } else {
                warn!(plugin_id = %plugin_id, path = %tool_path.display(), "Failed to load tool manifest from plugin");
            }
        }

        // Store registered IDs so deactivate() can clean them up.
        {
            let mut entries = self.entries.write().await;
            if let Some(entry) = entries.get_mut(plugin_id) {
                entry.registered_tool_ids = registered;
            }
        }

        self.hook_registry
            .fire(&agentos_types::HookEvent::PluginActivated {
                plugin_id: plugin_id.to_string(),
            })
            .await;
        info!(plugin_id = %plugin_id, manifest_dir = %manifest_dir.display(), "Plugin activated");
        Ok(())
    }

    /// Deactivate a plugin by ID. Unregisters all tools it registered.
    pub async fn deactivate(&self, plugin_id: &str) -> anyhow::Result<()> {
        let tool_ids = {
            let mut entries = self.entries.write().await;
            match entries.get_mut(plugin_id) {
                Some(e) if e.status == PluginStatus::Active => {
                    e.status = PluginStatus::Disabled;
                    let ids = std::mem::take(&mut e.registered_tool_ids);
                    info!(plugin_id = %plugin_id, "Deactivated plugin");
                    ids
                }
                Some(e) => {
                    anyhow::bail!(
                        "Plugin '{}' is not active (status: {:?})",
                        plugin_id,
                        e.status
                    )
                }
                None => anyhow::bail!("Plugin '{}' not found", plugin_id),
            }
        };

        // Unregister the tools this plugin contributed.
        if !tool_ids.is_empty() {
            let mut tr = self.tool_registry.write().await;
            for tool_id in &tool_ids {
                tr.unregister(tool_id);
            }
            info!(plugin_id = %plugin_id, count = tool_ids.len(), "Unregistered plugin tools");
        }
        self.hook_registry
            .fire(&agentos_types::HookEvent::PluginDeactivated {
                plugin_id: plugin_id.to_string(),
            })
            .await;
        Ok(())
    }

    /// Return a snapshot of all discovered plugins, sorted by plugin ID.
    pub async fn list(&self) -> Vec<PluginEntry> {
        let mut entries: Vec<PluginEntry> = self.entries.read().await.values().cloned().collect();
        entries.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
        entries
    }

    /// Return the status of a specific plugin, or `None` if unknown.
    pub async fn status(&self, plugin_id: &str) -> Option<PluginStatus> {
        self.entries
            .read()
            .await
            .get(plugin_id)
            .map(|e| e.status.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn make_registry() -> Arc<PluginRegistry> {
        let hook_registry = HookRegistry::new();
        let tool_registry = Arc::new(RwLock::new(ToolRegistry::new()));
        PluginRegistry::new(hook_registry, tool_registry)
    }

    fn write_manifest(dir: &TempDir, name: &str, content: &str) -> PathBuf {
        let plugin_dir = dir.path().join(name);
        std::fs::create_dir_all(&plugin_dir).unwrap();
        let path = plugin_dir.join("plugin.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        write!(f, "{}", content).unwrap();
        path
    }

    const VALID_MANIFEST: &str = r#"
id = "test-plugin"
display_name = "Test Plugin"
version = "1.0.0"
description = "A test plugin"
trust_tier = "core"
permissions = ["network.outbound"]
"#;

    const BLOCKED_MANIFEST: &str = r#"
id = "bad-plugin"
display_name = "Bad Plugin"
version = "1.0.0"
description = "A blocked plugin"
trust_tier = "blocked"
permissions = []
"#;

    #[tokio::test]
    async fn test_discover_valid_plugin() {
        let tmp = TempDir::new().unwrap();
        write_manifest(&tmp, "test-plugin", VALID_MANIFEST);

        let registry = make_registry();
        let count = registry.discover(&[tmp.path().to_path_buf()]).await;
        assert_eq!(count, 1);

        let list = registry.list().await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].manifest.id, "test-plugin");
        assert_eq!(list[0].status, PluginStatus::Discovered);
    }

    #[tokio::test]
    async fn test_blocked_plugin_flagged_at_discovery() {
        let tmp = TempDir::new().unwrap();
        write_manifest(&tmp, "bad-plugin", BLOCKED_MANIFEST);

        let registry = make_registry();
        registry.discover(&[tmp.path().to_path_buf()]).await;

        let list = registry.list().await;
        assert_eq!(list.len(), 1);
        assert!(matches!(list[0].status, PluginStatus::Blocked { .. }));
    }

    #[tokio::test]
    async fn test_activate_plugin() {
        let tmp = TempDir::new().unwrap();
        write_manifest(&tmp, "test-plugin", VALID_MANIFEST);

        let registry = make_registry();
        registry.discover(&[tmp.path().to_path_buf()]).await;
        registry.activate("test-plugin").await.unwrap();

        assert_eq!(
            registry.status("test-plugin").await,
            Some(PluginStatus::Active)
        );
    }

    #[tokio::test]
    async fn test_activate_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        write_manifest(&tmp, "test-plugin", VALID_MANIFEST);

        let registry = make_registry();
        registry.discover(&[tmp.path().to_path_buf()]).await;
        registry.activate("test-plugin").await.unwrap();
        // Second activate should succeed without error.
        registry.activate("test-plugin").await.unwrap();
    }

    #[tokio::test]
    async fn test_activate_blocked_returns_err() {
        let tmp = TempDir::new().unwrap();
        write_manifest(&tmp, "bad-plugin", BLOCKED_MANIFEST);

        let registry = make_registry();
        registry.discover(&[tmp.path().to_path_buf()]).await;
        assert!(registry.activate("bad-plugin").await.is_err());
    }

    #[tokio::test]
    async fn test_deactivate_active_plugin() {
        let tmp = TempDir::new().unwrap();
        write_manifest(&tmp, "test-plugin", VALID_MANIFEST);

        let registry = make_registry();
        registry.discover(&[tmp.path().to_path_buf()]).await;
        registry.activate("test-plugin").await.unwrap();
        registry.deactivate("test-plugin").await.unwrap();

        assert_eq!(
            registry.status("test-plugin").await,
            Some(PluginStatus::Disabled)
        );
    }

    #[tokio::test]
    async fn test_unknown_plugin_returns_none() {
        let registry = make_registry();
        assert!(registry.status("nonexistent").await.is_none());
    }

    #[tokio::test]
    async fn test_duplicate_discovery_skipped() {
        let tmp = TempDir::new().unwrap();
        write_manifest(&tmp, "test-plugin", VALID_MANIFEST);

        let registry = make_registry();
        let first = registry.discover(&[tmp.path().to_path_buf()]).await;
        let second = registry.discover(&[tmp.path().to_path_buf()]).await;

        assert_eq!(first, 1);
        assert_eq!(second, 0); // already discovered
        assert_eq!(registry.list().await.len(), 1);
    }
}
