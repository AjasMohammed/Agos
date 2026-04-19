//! Hybrid tool selector — reduces LLM context cost by injecting only the tools
//! relevant to a given task instead of the full registered set.
//!
//! Three-layer algorithm:
//!   1. **Permission filter** — remove tools the agent's PermissionSet cannot call.
//!   2. **Always-on partition** — named tools and groups always included if permitted.
//!   3. **Group detection + semantic ranking** — keyword signals from the task prompt
//!      activate whole tool groups; remaining candidates are ranked by embedding
//!      cosine similarity and the top-K are kept.
//!
//! Falls back to keyword scoring when the embedding model is unavailable.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use agentos_capability::parse_permission_str;
use agentos_types::{PermissionOp, PermissionSet, ToolManifest};
use tokio::sync::RwLock;
use tracing::warn;

use crate::config::ToolSelectionConfig;

/// Keyword signals that activate each tool group.
/// Conservative — false positives (including an extra group) are acceptable;
/// false negatives (missing a needed tool) waste tokens on discovery.
static GROUP_SIGNALS: &[(&str, &[&str])] = &[
    (
        "fs",
        &[
            "file",
            "read",
            "write",
            "directory",
            "folder",
            "path",
            "glob",
            "grep",
            "edit",
            "save",
            "load",
            "create file",
            "delete file",
            "list files",
            "diff",
            "content",
            "text file",
            "parse",
        ],
    ),
    (
        "network",
        &[
            "http", "fetch", "web", "search", "url", "download", "api", "request", "dns", "browse",
            "internet", "website", "endpoint", "curl", "get ", "post ", "scrape",
        ],
    ),
    (
        "process",
        &[
            "run",
            "exec",
            "shell",
            "command",
            "script",
            "process",
            "build",
            "compile",
            "test",
            "install",
            "program",
            "binary",
            "subprocess",
            "terminal",
            "bash",
            "python",
            "cargo",
            "npm",
            "make",
        ],
    ),
    (
        "coordination",
        &[
            "agent",
            "spawn",
            "delegate",
            "parallel",
            "sub-agent",
            "coordinate",
            "team",
            "worker",
            "task",
            "assign",
            "distribute",
            "orchestrat",
            "pipeline",
            "workflow",
            "multi-agent",
        ],
    ),
    (
        "memory",
        &[
            "remember",
            "recall",
            "memory",
            "procedure",
            "note",
            "scratchpad",
            "knowledge",
            "archive",
            "store",
            "episodic",
            "semantic",
            "past",
            "history",
            "learned",
            "fact",
            "wiki",
            "graph",
        ],
    ),
    (
        "events",
        &[
            "event",
            "subscribe",
            "timer",
            "schedule",
            "alert",
            "watch",
            "monitor",
            "trigger",
            "notification",
            "hook",
            "listen",
            "poll",
        ],
    ),
    (
        "hal",
        &[
            "audio",
            "camera",
            "webcam",
            "bluetooth",
            "display",
            "print",
            "usb",
            "hardware",
            "sensor",
            "screen",
            "device",
            "peripheral",
            "speaker",
            "microphone",
            "gpu",
        ],
    ),
    (
        "iot",
        &[
            "mqtt",
            "home assistant",
            "iot",
            "smart home",
            "automation",
            "sensor",
            "actuator",
            "thermostat",
            "light bulb",
        ],
    ),
    (
        "container",
        &[
            "container",
            "docker",
            "isolat",
            "sandbox",
            "virtual",
            "image",
            "pod",
        ],
    ),
    (
        "kmc",
        &[
            "environment",
            " env ",
            "storage zone",
            "capability",
            "provision",
            "runtime env",
            "python env",
            "node env",
        ],
    ),
    (
        "comms",
        &[
            "ask user",
            "notify",
            "tell the user",
            "inform",
            "confirm",
            "approval",
            "conversation",
            "message the user",
        ],
    ),
];

/// Per-task embedding cache entry.
struct ToolEmbedding {
    /// L2-normalised 384-dim MiniLM vector.
    vector: Vec<f32>,
}

pub struct ToolSelector {
    config: Arc<ToolSelectionConfig>,
    embedder: Option<Arc<agentos_memory::Embedder>>,
    /// Cached per-tool embeddings populated lazily on first `select` call.
    embedding_cache: RwLock<HashMap<String, ToolEmbedding>>,
}

impl ToolSelector {
    pub fn new(
        config: ToolSelectionConfig,
        embedder: Option<Arc<agentos_memory::Embedder>>,
    ) -> Self {
        Self {
            config: Arc::new(config),
            embedder,
            embedding_cache: RwLock::new(HashMap::new()),
        }
    }

    /// Build the tool manifest list for one LLM call.
    ///
    /// Returns manifests sorted alphabetically (stable across iterations so
    /// prompt-cache keys stay consistent).
    pub async fn select(
        &self,
        task_prompt: &str,
        all_manifests: &[ToolManifest],
        permissions: &PermissionSet,
        skip_selection: bool,
    ) -> Vec<ToolManifest> {
        // Fast path: selection disabled globally or per-task.
        if !self.config.enabled || skip_selection {
            let mut out = self.filter_by_permissions(all_manifests, permissions);
            out.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
            return out;
        }

        // Layer 1 — permission filter.
        let permitted = self.filter_by_permissions(all_manifests, permissions);

        // Layer 2 — always-on partition.
        let always_on_names: HashSet<&str> = self
            .config
            .always_on_tools
            .iter()
            .map(String::as_str)
            .collect();
        let always_on_groups: HashSet<&str> = self
            .config
            .always_on_groups
            .iter()
            .map(String::as_str)
            .collect();

        let mut selected: Vec<ToolManifest> = Vec::new();
        let mut selected_names: HashSet<String> = HashSet::new();
        let mut candidates: Vec<ToolManifest> = Vec::new();

        for m in permitted {
            let name = &m.manifest.name;
            let group = m.manifest.group.as_str();
            if always_on_names.contains(name.as_str()) || always_on_groups.contains(group) {
                selected_names.insert(name.clone());
                selected.push(m);
            } else {
                candidates.push(m);
            }
        }

        // Layer 3a — group detection from task prompt.
        let active_groups = Self::detect_groups(task_prompt);
        let mut remaining: Vec<ToolManifest> = Vec::new();
        for m in candidates {
            let group = m.manifest.group.as_str();
            if active_groups.contains(group) && !selected_names.contains(&m.manifest.name) {
                selected_names.insert(m.manifest.name.clone());
                selected.push(m);
            } else if !selected_names.contains(&m.manifest.name) {
                remaining.push(m);
            }
        }

        // Layer 3b — semantic ranking from remaining candidates.
        // Treat max_total_tools = 0 as "no cap" so operators can disable the cap
        // without accidentally zeroing out the semantic-ranking budget.
        let effective_cap = if self.config.max_total_tools == 0 {
            usize::MAX
        } else {
            self.config.max_total_tools
        };
        let budget = effective_cap
            .saturating_sub(selected.len())
            .min(self.config.semantic_top_k);

        if budget > 0 && !remaining.is_empty() {
            let top = self.semantic_rank(task_prompt, &remaining, budget).await;
            for m in top {
                if !selected_names.contains(&m.manifest.name) {
                    selected_names.insert(m.manifest.name.clone());
                    selected.push(m);
                }
            }
        }

        // Enforce hard cap (effective_cap = usize::MAX when max_total_tools = 0).
        if selected.len() > effective_cap {
            selected.truncate(effective_cap);
            // Rebuild the name set so the floor fill below doesn't treat truncated
            // tools as still-selected and skip filling the gap.
            selected_names = selected.iter().map(|m| m.manifest.name.clone()).collect();
        }

        // Floor guarantee — fill from remaining if selection came up short.
        if selected.len() < self.config.min_tools {
            for m in remaining {
                if selected.len() >= self.config.min_tools {
                    break;
                }
                if !selected_names.contains(&m.manifest.name) {
                    selected.push(m);
                }
            }
        }

        selected.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
        selected
    }

    /// Register a new tool's embedding so it's ready before the first `select`.
    /// Called when MCP tools are attached at runtime.
    pub async fn register_tool_embedding(&self, name: &str, description: &str) {
        if let Some(emb) = &self.embedder {
            let text = description.to_string();
            let emb = Arc::clone(emb);
            match tokio::task::spawn_blocking(move || emb.embed(&[text.as_str()])).await {
                Ok(Ok(vecs)) if !vecs.is_empty() => {
                    let v = normalize(&vecs[0]);
                    self.embedding_cache
                        .write()
                        .await
                        .insert(name.to_string(), ToolEmbedding { vector: v });
                }
                _ => {}
            }
        }
    }

    // ── Private helpers ──────────────────────────────────────────────────────

    /// Remove tools whose required permissions the PermissionSet cannot satisfy.
    fn filter_by_permissions(
        &self,
        manifests: &[ToolManifest],
        permissions: &PermissionSet,
    ) -> Vec<ToolManifest> {
        manifests
            .iter()
            .filter(|m| {
                m.capabilities_required
                    .permissions
                    .iter()
                    .all(|perm_str| permission_granted(permissions, perm_str))
            })
            .cloned()
            .collect()
    }

    /// Keyword-based group detection from the task prompt.
    fn detect_groups(prompt: &str) -> HashSet<&'static str> {
        let lower = prompt.to_lowercase();
        let mut groups = HashSet::new();
        for (group, signals) in GROUP_SIGNALS {
            if signals.iter().any(|s| lower.contains(*s)) {
                groups.insert(*group);
            }
        }
        groups
    }

    /// Return the top-`k` manifests most similar to `query`, using cached
    /// tool embeddings. Falls back to keyword scoring if embedder unavailable.
    async fn semantic_rank(
        &self,
        query: &str,
        candidates: &[ToolManifest],
        k: usize,
    ) -> Vec<ToolManifest> {
        if candidates.is_empty() || k == 0 {
            return vec![];
        }

        // Ensure cache is populated for all candidates.
        self.warm_cache(candidates).await;

        // Embed the query.
        let query_vec: Option<Vec<f32>> = match &self.embedder {
            Some(emb) => {
                let q = query.to_string();
                let emb = Arc::clone(emb);
                tokio::task::spawn_blocking(move || emb.embed(&[q.as_str()]))
                    .await
                    .ok()
                    .and_then(|r| r.ok())
                    .and_then(|mut v| v.pop())
                    .map(|v| normalize(&v))
            }
            None => None,
        };

        match query_vec {
            Some(qv) => {
                let cache = self.embedding_cache.read().await;
                let mut scored: Vec<(f32, &ToolManifest)> = candidates
                    .iter()
                    .map(|m| {
                        let score = cache
                            .get(&m.manifest.name)
                            .map(|e| cosine_sim(&qv, &e.vector))
                            .unwrap_or(0.0);
                        (score, m)
                    })
                    .collect();
                scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
                scored.into_iter().take(k).map(|(_, m)| m.clone()).collect()
            }
            None => {
                // Fallback: keyword overlap between task prompt and tool description.
                keyword_rank(query, candidates, k)
            }
        }
    }

    /// Pre-compute embeddings for any candidates missing from the cache.
    async fn warm_cache(&self, candidates: &[ToolManifest]) {
        let Some(emb) = &self.embedder else { return };

        let missing: Vec<(String, String)> = {
            let cache = self.embedding_cache.read().await;
            candidates
                .iter()
                .filter(|m| !cache.contains_key(&m.manifest.name))
                .map(|m| (m.manifest.name.clone(), m.manifest.description.clone()))
                .collect()
        };

        if missing.is_empty() {
            return;
        }

        let texts: Vec<String> = missing.iter().map(|(_, d)| d.clone()).collect();
        let emb = Arc::clone(emb);

        match tokio::task::spawn_blocking(move || {
            let text_refs: Vec<&str> = texts.iter().map(String::as_str).collect();
            emb.embed(&text_refs)
        })
        .await
        {
            Ok(Ok(vecs)) if vecs.len() == missing.len() => {
                let mut cache = self.embedding_cache.write().await;
                for ((name, _), vec) in missing.into_iter().zip(vecs) {
                    cache.insert(
                        name,
                        ToolEmbedding {
                            vector: normalize(&vec),
                        },
                    );
                }
            }
            Ok(Err(e)) => warn!("tool embedding batch failed: {e}"),
            _ => {}
        }
    }
}

// ── Math helpers ─────────────────────────────────────────────────────────────

fn normalize(v: &[f32]) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm < 1e-9 {
        return v.to_vec();
    }
    v.iter().map(|x| x / norm).collect()
}

fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

/// Simple keyword overlap fallback used when the embedding model is unavailable.
fn keyword_rank(query: &str, candidates: &[ToolManifest], k: usize) -> Vec<ToolManifest> {
    let query_words: HashSet<&str> = query.split_whitespace().collect();
    let mut scored: Vec<(usize, &ToolManifest)> = candidates
        .iter()
        .map(|m| {
            let desc_words: HashSet<&str> = m.manifest.description.split_whitespace().collect();
            let overlap = query_words.intersection(&desc_words).count();
            (overlap, m)
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.into_iter().take(k).map(|(_, m)| m.clone()).collect()
}

// ── Permission helper ─────────────────────────────────────────────────────────

/// Returns true if the agent's PermissionSet satisfies the raw permission
/// string from a tool manifest (e.g. `"fs.user_data:r"`, `"memory.semantic:rw"`).
fn permission_granted(permissions: &PermissionSet, perm_str: &str) -> bool {
    // Empty string / wildcard — no permission required.
    if perm_str.is_empty() || perm_str == "*" {
        return true;
    }

    match parse_permission_str(perm_str) {
        Ok(entry) => {
            let res = &entry.resource;
            let mut ok = true;
            if entry.read {
                ok = ok && permissions.check(res, PermissionOp::Read);
            }
            if entry.write {
                ok = ok && permissions.check(res, PermissionOp::Write);
            }
            if entry.execute {
                ok = ok && permissions.check(res, PermissionOp::Execute);
            }
            ok
        }
        Err(_) => {
            // Malformed permission string — exclude tool (fail closed for security).
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::tool::{ToolCapabilities, ToolInfo, ToolOutputs, ToolSchema};

    fn make_manifest(name: &str, group: &str, description: &str, perms: &[&str]) -> ToolManifest {
        ToolManifest {
            manifest: ToolInfo {
                name: name.to_string(),
                version: "1.0.0".to_string(),
                description: description.to_string(),
                author: "test".to_string(),
                checksum: None,
                author_pubkey: None,
                signature: None,
                trust_tier: agentos_types::TrustTier::Core,
                tags: None,
                capability_tags: vec![],
                group: group.to_string(),
            },
            capabilities_required: ToolCapabilities {
                permissions: perms.iter().map(|s| s.to_string()).collect(),
            },
            capabilities_provided: ToolOutputs { outputs: vec![] },
            intent_schema: ToolSchema {
                input: String::new(),
                output: String::new(),
            },
            input_schema: None,
            sandbox: agentos_types::ToolSandbox {
                network: false,
                fs_write: false,
                gpu: false,
                max_memory_mb: 64,
                max_cpu_ms: 5000,
                syscalls: vec![],
                weight: None,
            },
            executor: agentos_types::ToolExecutor::default(),
            fallbacks: vec![],
            risk_class: agentos_types::RiskClass::ReadonlyScoped,
        }
    }

    fn open_permissions() -> PermissionSet {
        use agentos_types::{PermissionEntry, PermissionSet};
        PermissionSet {
            entries: vec![PermissionEntry {
                resource: "*".to_string(),
                read: true,
                write: true,
                execute: true,
                query: true,
                observe: true,
                expires_at: None,
            }],
            deny_entries: vec![],
        }
    }

    fn no_permissions() -> PermissionSet {
        PermissionSet::default()
    }

    #[test]
    fn test_permission_filter_removes_unpermitted() {
        let selector = ToolSelector::new(ToolSelectionConfig::default(), None);
        let manifests = vec![
            make_manifest("tool-a", "misc", "does stuff", &[]),
            make_manifest("tool-b", "misc", "needs fs", &["fs.user_data:r"]),
        ];
        let result = selector.filter_by_permissions(&manifests, &no_permissions());
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].manifest.name, "tool-a");
    }

    #[test]
    fn test_permission_filter_allows_all_with_wildcard() {
        let selector = ToolSelector::new(ToolSelectionConfig::default(), None);
        let manifests = vec![
            make_manifest("tool-a", "misc", "needs fs", &["fs.user_data:r"]),
            make_manifest("tool-b", "misc", "needs net", &["network.outbound:x"]),
        ];
        let result = selector.filter_by_permissions(&manifests, &open_permissions());
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_group_detection_fs() {
        let groups = ToolSelector::detect_groups("please read the config file and parse it");
        assert!(groups.contains("fs"));
    }

    #[test]
    fn test_group_detection_network() {
        let groups = ToolSelector::detect_groups("fetch the latest news from the web");
        assert!(groups.contains("network"));
    }

    #[test]
    fn test_group_detection_process() {
        let groups = ToolSelector::detect_groups("run the build script and compile the project");
        assert!(groups.contains("process"));
    }

    #[test]
    fn test_group_detection_coordination() {
        let groups = ToolSelector::detect_groups("spawn two agents to work in parallel");
        assert!(groups.contains("coordination"));
    }

    #[test]
    fn test_group_detection_no_match() {
        // A very generic prompt should not match specific hardware groups
        let groups = ToolSelector::detect_groups("what is the meaning of life");
        assert!(!groups.contains("hal"));
        assert!(!groups.contains("iot"));
        assert!(!groups.contains("container"));
    }

    #[tokio::test]
    async fn test_select_always_on_always_included() {
        let mut config = ToolSelectionConfig::default();
        config.always_on_tools = vec!["think".into()];
        config.always_on_groups = vec!["core".into()];
        let selector = ToolSelector::new(config, None);

        let manifests = vec![
            make_manifest("think", "core", "reasoning tool", &[]),
            make_manifest(
                "shell-exec",
                "process",
                "run shell commands",
                &["process.exec:x"],
            ),
        ];
        // Use no_permissions so shell-exec is filtered out.
        let result = selector
            .select(
                "think about this problem",
                &manifests,
                &no_permissions(),
                false,
            )
            .await;
        // "think" has no permissions requirement so it passes the filter and is always-on.
        assert!(result.iter().any(|m| m.manifest.name == "think"));
        // "shell-exec" needs process.exec:x which no_permissions doesn't grant.
        assert!(!result.iter().any(|m| m.manifest.name == "shell-exec"));
    }

    #[tokio::test]
    async fn test_select_skip_selection_returns_all_permitted() {
        let selector = ToolSelector::new(ToolSelectionConfig::default(), None);
        let manifests = vec![
            make_manifest("tool-a", "misc", "misc tool", &[]),
            make_manifest("tool-b", "misc", "misc tool 2", &[]),
        ];
        let result = selector
            .select("do something", &manifests, &open_permissions(), true)
            .await;
        assert_eq!(result.len(), 2);
    }

    #[tokio::test]
    async fn test_select_group_activated_by_signal() {
        let mut config = ToolSelectionConfig::default();
        config.always_on_tools = vec![];
        config.always_on_groups = vec![];
        config.semantic_top_k = 0; // disable semantic ranking
        config.min_tools = 0;
        let selector = ToolSelector::new(config, None);

        let manifests = vec![
            make_manifest("file-reader", "fs", "reads files", &[]),
            make_manifest("spawn-agent", "coordination", "spawns agents", &[]),
        ];
        let result = selector
            .select(
                "please read the config file",
                &manifests,
                &open_permissions(),
                false,
            )
            .await;
        // "fs" group should be activated by "file" signal
        assert!(result.iter().any(|m| m.manifest.name == "file-reader"));
        // "coordination" group should NOT be activated
        assert!(!result.iter().any(|m| m.manifest.name == "spawn-agent"));
    }

    #[tokio::test]
    async fn test_select_max_total_cap_enforced() {
        let mut config = ToolSelectionConfig::default();
        config.always_on_tools = vec![];
        config.always_on_groups = vec!["misc".into()]; // select all misc
        config.max_total_tools = 2;
        config.min_tools = 0;
        let selector = ToolSelector::new(config, None);

        let manifests: Vec<_> = (0..10)
            .map(|i| make_manifest(&format!("tool-{i}"), "misc", "a tool", &[]))
            .collect();
        let result = selector
            .select("do something", &manifests, &open_permissions(), false)
            .await;
        assert!(result.len() <= 2);
    }

    #[tokio::test]
    async fn test_select_min_floor_filled() {
        let mut config = ToolSelectionConfig::default();
        config.always_on_tools = vec![];
        config.always_on_groups = vec![];
        config.semantic_top_k = 0;
        config.min_tools = 3;
        let selector = ToolSelector::new(config, None);

        let manifests: Vec<_> = (0..5)
            .map(|i| make_manifest(&format!("tool-{i}"), "misc", "a tool", &[]))
            .collect();
        // No signals match "misc" group, semantic is off, but min_tools=3 guarantees 3
        let result = selector
            .select("something vague", &manifests, &open_permissions(), false)
            .await;
        assert!(result.len() >= 3);
    }
}
