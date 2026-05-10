use crate::{contributor::NodeContributor, error::NodeManifestError, manifest::NodeManifest};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

const CONTRIBUTOR_CACHE_TTL: Duration = Duration::from_secs(30);

#[derive(Clone, Default)]
pub struct NodeRegistry {
    static_nodes: Arc<RwLock<BTreeMap<String, NodeManifest>>>,
    contributors: Arc<RwLock<Vec<ContributorEntry>>>,
}

struct ContributorEntry {
    contributor: Arc<dyn NodeContributor>,
    cached: Option<(Instant, Vec<NodeManifest>)>,
}

#[derive(Serialize, Clone)]
pub struct PaletteGroup {
    pub category: String,
    pub display_name: String,
    pub sort_order: u32,
    pub nodes: Vec<NodeManifest>,
}

impl NodeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load static TOML node manifests from all files in the given directories.
    /// Returns the count of manifests successfully loaded.
    pub async fn load_from_dirs<P: AsRef<Path>>(
        &self,
        dirs: &[P],
    ) -> Result<usize, NodeManifestError> {
        let mut count = 0;
        for dir in dirs {
            let dir = dir.as_ref();
            if !dir.exists() {
                continue;
            }
            let read_dir = std::fs::read_dir(dir).map_err(|e| NodeManifestError::Io {
                path: dir.display().to_string(),
                source: e,
            })?;
            for entry in read_dir.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                    continue;
                }
                let raw = std::fs::read_to_string(&path).map_err(|e| NodeManifestError::Io {
                    path: path.display().to_string(),
                    source: e,
                })?;
                let manifest: NodeManifest =
                    toml::from_str(&raw).map_err(|e| NodeManifestError::Parse {
                        path: path.display().to_string(),
                        source: e,
                    })?;
                let id = manifest.node.id.clone();
                self.static_nodes.write().await.insert(id, manifest);
                count += 1;
            }
        }
        Ok(count)
    }

    /// Register a dynamic contributor. Nodes it contributes appear in the palette.
    pub async fn add_contributor(&self, contributor: Arc<dyn NodeContributor>) {
        self.contributors.write().await.push(ContributorEntry {
            contributor,
            cached: None,
        });
    }

    /// Look up a node manifest by ID. Checks static nodes first, then contributors.
    pub async fn get(&self, id: &str) -> Option<NodeManifest> {
        if let Some(m) = self.static_nodes.read().await.get(id) {
            return Some(m.clone());
        }
        for entry in self.contributors.read().await.iter() {
            if let Some((ts, nodes)) = &entry.cached {
                if ts.elapsed() < CONTRIBUTOR_CACHE_TTL {
                    if let Some(m) = nodes.iter().find(|m| m.node.id == id) {
                        return Some(m.clone());
                    }
                    continue;
                }
            }
            // Cache miss — cannot refresh from a shared read lock; caller uses palette() for full refresh.
            let nodes = entry.contributor.contribute_nodes().await;
            if let Some(m) = nodes.iter().find(|m| m.node.id == id) {
                return Some(m.clone());
            }
        }
        None
    }

    /// Build the full palette: all static + all contributor nodes, grouped by category.
    pub async fn palette(&self) -> Vec<PaletteGroup> {
        // category → (display_name, sort_order, nodes)
        let mut by_category: BTreeMap<String, (String, u32, Vec<NodeManifest>)> = BTreeMap::new();

        for m in self.static_nodes.read().await.values() {
            let entry = by_category
                .entry(m.node.category.clone())
                .or_insert_with(|| (title_case(&m.node.category), 50, vec![]));
            entry.2.push(m.clone());
        }

        let mut contrib_guard = self.contributors.write().await;
        for entry in contrib_guard.iter_mut() {
            let nodes = {
                let fresh = if let Some((ts, cached)) = &entry.cached {
                    if ts.elapsed() < CONTRIBUTOR_CACHE_TTL {
                        cached.clone()
                    } else {
                        let n = entry.contributor.contribute_nodes().await;
                        entry.cached = Some((Instant::now(), n.clone()));
                        n
                    }
                } else {
                    let n = entry.contributor.contribute_nodes().await;
                    entry.cached = Some((Instant::now(), n.clone()));
                    n
                };
                fresh
            };
            let order = entry.contributor.sort_order();
            let display = entry.contributor.category_display_name().to_string();
            for m in nodes {
                let cat_entry = by_category
                    .entry(m.node.category.clone())
                    .or_insert_with(|| (display.clone(), order, vec![]));
                cat_entry.2.push(m);
            }
        }

        let mut groups: Vec<PaletteGroup> = by_category
            .into_iter()
            .map(|(cat, (display_name, sort_order, mut nodes))| {
                nodes.sort_by(|a, b| a.node.display_name.cmp(&b.node.display_name));
                PaletteGroup {
                    category: cat,
                    display_name,
                    sort_order,
                    nodes,
                }
            })
            .collect();
        groups.sort_by_key(|g| g.sort_order);
        groups
    }

    /// Register a single static manifest directly (useful in tests and embedded manifests).
    pub async fn register_static(&self, manifest: NodeManifest) {
        let id = manifest.node.id.clone();
        self.static_nodes.write().await.insert(id, manifest);
    }

    /// Invalidate cached nodes for all contributors (forces refresh on next palette call).
    pub async fn invalidate_cache(&self) {
        for entry in self.contributors.write().await.iter_mut() {
            entry.cached = None;
        }
    }
}

fn title_case(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{NodeExecute, NodeManifestBody};

    fn make_manifest(id: &str, category: &str) -> NodeManifest {
        NodeManifest {
            node: NodeManifestBody {
                id: id.to_string(),
                display_name: id.to_string(),
                category: category.to_string(),
                execute: NodeExecute::Builtin {
                    name: "noop".to_string(),
                },
                ..Default::default()
            },
        }
    }

    struct MockContributor {
        nodes: Vec<NodeManifest>,
    }

    #[async_trait::async_trait]
    impl NodeContributor for MockContributor {
        fn category_prefix(&self) -> &str {
            "mock"
        }
        fn category_display_name(&self) -> &str {
            "Mock"
        }
        async fn contribute_nodes(&self) -> Vec<NodeManifest> {
            self.nodes.clone()
        }
    }

    #[tokio::test]
    async fn test_palette_groups_by_category() {
        let registry = NodeRegistry::new();
        registry
            .add_contributor(Arc::new(MockContributor {
                nodes: vec![
                    make_manifest("mock.a", "mock"),
                    make_manifest("mock.b", "mock"),
                    make_manifest("mock.c", "other"),
                ],
            }))
            .await;

        let palette = registry.palette().await;
        assert_eq!(palette.len(), 2);
        let mock_group = palette.iter().find(|g| g.category == "mock").unwrap();
        assert_eq!(mock_group.nodes.len(), 2);
    }

    #[tokio::test]
    async fn test_get_static_node() {
        let registry = NodeRegistry::new();
        let m = make_manifest("start", "core");
        registry
            .static_nodes
            .write()
            .await
            .insert("start".into(), m);
        let found = registry.get("start").await;
        assert!(found.is_some());
        assert_eq!(found.unwrap().node.id, "start");
    }
}
