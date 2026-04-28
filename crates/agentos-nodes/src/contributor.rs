use crate::manifest::NodeManifest;

/// Any AgentOS subsystem that wants to expose itself as workflow palette nodes
/// implements this trait. The `NodeRegistry` calls `contribute_nodes()` when
/// building the palette and caches results for 30 seconds.
///
/// Implementors must be cheap to clone (`Arc`-based internal state).
#[async_trait::async_trait]
pub trait NodeContributor: Send + Sync {
    /// Unique prefix used to namespace node IDs from this contributor.
    /// e.g. `"agent"`, `"tool"`, `"channel"`, `"mcp:my-server"`
    fn category_prefix(&self) -> &str;

    /// Human-visible group name shown in the palette sidebar.
    fn category_display_name(&self) -> &str;

    /// Palette ordering hint — lower values appear earlier. Default: 100.
    fn sort_order(&self) -> u32 {
        100
    }

    /// Generate the current set of node manifests for this contributor.
    /// Called on each palette build (result is cached for 30s).
    async fn contribute_nodes(&self) -> Vec<NodeManifest>;
}
