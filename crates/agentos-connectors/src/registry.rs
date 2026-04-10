use crate::definition::ConnectorManifest;
use crate::proxy::ConnectorProxy;
use agentos_types::AgentOSError;
use agentos_vault::SecretsVault;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Manages active connector proxies and routes namespaced tool calls.
///
/// When an agent calls `github.create_issue`, the registry:
/// 1. Splits the name at the first `.` to get connector_id="github", tool_name="create_issue"
/// 2. Looks up the connector proxy for "github"
/// 3. Delegates execution to the proxy
pub struct ConnectorRegistry {
    connectors: RwLock<HashMap<String, Arc<ConnectorProxy>>>,
    vault: Arc<SecretsVault>,
}

impl ConnectorRegistry {
    pub fn new(vault: Arc<SecretsVault>) -> Self {
        Self {
            connectors: RwLock::new(HashMap::new()),
            vault,
        }
    }

    /// Register a connector from its manifest.
    pub async fn register(&self, manifest: ConnectorManifest) -> Result<(), AgentOSError> {
        let id = manifest.connector.id.clone();
        let proxy = Arc::new(ConnectorProxy::new(manifest, Arc::clone(&self.vault)));

        tracing::info!(
            connector = %id,
            tools = ?proxy.tool_names(),
            "Registering connector"
        );

        self.connectors.write().await.insert(id, proxy);
        Ok(())
    }

    /// Unregister a connector by ID.
    pub async fn deregister(&self, connector_id: &str) -> Result<(), AgentOSError> {
        let removed = self.connectors.write().await.remove(connector_id);
        if removed.is_some() {
            tracing::info!(connector = %connector_id, "Deregistered connector");
            Ok(())
        } else {
            Err(AgentOSError::ToolNotFound(format!(
                "Connector not registered: {connector_id}"
            )))
        }
    }

    /// List all registered connector manifests (metadata only).
    pub async fn list(&self) -> Vec<ConnectorManifest> {
        self.connectors
            .read()
            .await
            .values()
            .map(|proxy| proxy.manifest().clone())
            .collect()
    }

    /// Check if a tool name looks like a connector call (contains a dot).
    pub fn is_connector_call(tool_name: &str) -> bool {
        tool_name.contains('.')
    }

    /// Route a namespaced tool call (e.g., "github.create_issue").
    ///
    /// Returns `None` if no connector matches the namespace prefix.
    /// Returns `Some(Ok(result))` on success, `Some(Err(e))` on failure.
    pub async fn route(
        &self,
        namespaced_tool: &str,
        input: serde_json::Value,
    ) -> Option<Result<serde_json::Value, AgentOSError>> {
        let (connector_id, tool_name) = namespaced_tool.split_once('.')?;

        let connectors = self.connectors.read().await;
        let proxy = connectors.get(connector_id)?;

        Some(proxy.execute(tool_name, input).await)
    }

    /// Get a list of all namespaced tool names across all connectors.
    /// Useful for injecting into the LLM tool manifest list.
    pub async fn all_tool_names(&self) -> Vec<String> {
        let connectors = self.connectors.read().await;
        connectors
            .values()
            .flat_map(|proxy| proxy.namespaced_tool_names())
            .collect()
    }

    /// Get the number of registered connectors.
    pub async fn count(&self) -> usize {
        self.connectors.read().await.len()
    }

    /// Check if a specific connector is registered.
    pub async fn has_connector(&self, connector_id: &str) -> bool {
        self.connectors.read().await.contains_key(connector_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::*;

    fn make_test_vault() -> Arc<SecretsVault> {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("vault.db");
        let audit_path = tmp.path().join("audit.db");
        let audit = Arc::new(agentos_audit::AuditLog::open(&audit_path).unwrap());
        let passphrase = agentos_vault::ZeroizingString::new("test".into());
        let vault = SecretsVault::initialize(&db_path, &passphrase, audit).unwrap();
        std::mem::forget(tmp);
        Arc::new(vault)
    }

    fn github_manifest() -> ConnectorManifest {
        ConnectorManifest {
            connector: ConnectorInfo {
                id: "github".into(),
                name: "GitHub".into(),
                version: "1.0.0".into(),
                description: "GitHub API".into(),
                base_url: "https://api.github.com".into(),
                auth: AuthConfig::None,
                rate_limit: None,
                max_response_bytes: 32768,
            },
            tools: vec![
                ConnectorToolDef {
                    name: "list_repos".into(),
                    description: "List repos".into(),
                    method: HttpMethod::Get,
                    path: "/user/repos".into(),
                    input_schema: None,
                    response_map: None,
                    query_params: vec![],
                    body_fields: vec![],
                },
                ConnectorToolDef {
                    name: "create_issue".into(),
                    description: "Create issue".into(),
                    method: HttpMethod::Post,
                    path: "/repos/{owner}/{repo}/issues".into(),
                    input_schema: None,
                    response_map: None,
                    query_params: vec![],
                    body_fields: vec![],
                },
            ],
        }
    }

    #[tokio::test]
    async fn test_register_and_list() {
        let vault = make_test_vault();
        let registry = ConnectorRegistry::new(vault);

        registry.register(github_manifest()).await.unwrap();

        let list = registry.list().await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].connector.id, "github");
    }

    #[tokio::test]
    async fn test_deregister() {
        let vault = make_test_vault();
        let registry = ConnectorRegistry::new(vault);

        registry.register(github_manifest()).await.unwrap();
        assert_eq!(registry.count().await, 1);

        registry.deregister("github").await.unwrap();
        assert_eq!(registry.count().await, 0);
    }

    #[tokio::test]
    async fn test_deregister_not_found() {
        let vault = make_test_vault();
        let registry = ConnectorRegistry::new(vault);
        assert!(registry.deregister("nonexistent").await.is_err());
    }

    #[test]
    fn test_is_connector_call() {
        assert!(ConnectorRegistry::is_connector_call("github.create_issue"));
        assert!(ConnectorRegistry::is_connector_call("slack.post_message"));
        assert!(!ConnectorRegistry::is_connector_call("file-reader"));
        assert!(!ConnectorRegistry::is_connector_call("shell-exec"));
    }

    #[tokio::test]
    async fn test_route_unknown_connector() {
        let vault = make_test_vault();
        let registry = ConnectorRegistry::new(vault);

        let result = registry.route("unknown.tool", serde_json::json!({})).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_all_tool_names() {
        let vault = make_test_vault();
        let registry = ConnectorRegistry::new(vault);

        registry.register(github_manifest()).await.unwrap();

        let names = registry.all_tool_names().await;
        assert!(names.contains(&"github.list_repos".to_string()));
        assert!(names.contains(&"github.create_issue".to_string()));
    }

    #[tokio::test]
    async fn test_has_connector() {
        let vault = make_test_vault();
        let registry = ConnectorRegistry::new(vault);

        assert!(!registry.has_connector("github").await);
        registry.register(github_manifest()).await.unwrap();
        assert!(registry.has_connector("github").await);
    }

    #[tokio::test]
    async fn test_route_no_dot_returns_none() {
        let vault = make_test_vault();
        let registry = ConnectorRegistry::new(vault);
        let result = registry.route("file-reader", serde_json::json!({})).await;
        assert!(result.is_none());
    }
}
