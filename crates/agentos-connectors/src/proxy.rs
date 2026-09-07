use crate::definition::*;
use agentos_types::AgentOSError;
use agentos_vault::SecretsVault;
use std::sync::Arc;

/// Executes HTTP requests on behalf of an agent, transparently injecting
/// authentication credentials from the vault.
///
/// The agent never sees raw tokens — it provides tool inputs (like "owner",
/// "repo", "title") and receives structured response data.
pub struct ConnectorProxy {
    manifest: ConnectorManifest,
    vault: Arc<SecretsVault>,
    http_client: reqwest::Client,
}

impl ConnectorProxy {
    pub fn new(manifest: ConnectorManifest, vault: Arc<SecretsVault>) -> Self {
        Self {
            manifest,
            vault,
            http_client: reqwest::Client::new(),
        }
    }

    /// The connector ID (namespace prefix for tools).
    pub fn connector_id(&self) -> &str {
        &self.manifest.connector.id
    }

    /// The full connector manifest.
    pub fn manifest(&self) -> &ConnectorManifest {
        &self.manifest
    }

    /// List the tool names this connector provides (without namespace prefix).
    pub fn tool_names(&self) -> Vec<String> {
        self.manifest.tools.iter().map(|t| t.name.clone()).collect()
    }

    /// List namespaced tool names (e.g., "github.create_issue").
    pub fn namespaced_tool_names(&self) -> Vec<String> {
        let prefix = &self.manifest.connector.id;
        self.manifest
            .tools
            .iter()
            .map(|t| format!("{prefix}.{}", t.name))
            .collect()
    }

    /// Execute a connector tool call.
    ///
    /// 1. Look up the tool definition by name
    /// 2. Partition input into path params, query params, and body
    /// 3. Template the URL path
    /// 4. Fetch credentials from vault (static secret or OAuth)
    /// 5. Inject auth header
    /// 6. Make HTTP request
    /// 7. Apply response_map to extract relevant fields
    /// 8. Return JSON result (never includes raw auth headers)
    pub async fn execute(
        &self,
        tool_name: &str,
        input: serde_json::Value,
    ) -> Result<serde_json::Value, AgentOSError> {
        let tool_def = self
            .manifest
            .tools
            .iter()
            .find(|t| t.name == tool_name)
            .ok_or_else(|| {
                AgentOSError::ToolNotFound(format!(
                    "Tool '{}' not found in connector '{}'",
                    tool_name, self.manifest.connector.id
                ))
            })?;

        // Partition input into path, query, and body parameters
        let (path_params, query_params, body) = partition_params(&input, tool_def);

        // Resolve URL
        let path = resolve_path_template(
            &tool_def.path,
            &serde_json::to_value(&path_params).unwrap_or_default(),
        )
        .map_err(AgentOSError::SchemaValidation)?;
        let url = format!("{}{}", self.manifest.connector.base_url, path);

        // Build request
        let method = tool_def.method.to_reqwest();
        let mut request = self.http_client.request(method.clone(), &url);

        // Add query parameters
        for (k, v) in &query_params {
            request = request.query(&[(k, v)]);
        }

        // Add body for POST/PUT/PATCH
        if matches!(
            tool_def.method,
            HttpMethod::Post | HttpMethod::Put | HttpMethod::Patch
        ) {
            request = request.json(&body);
        }

        // Inject authentication
        request = self.inject_auth(request).await?;

        // Add standard headers
        request = request
            .header("Accept", "application/json")
            .header("User-Agent", "AgentOS-Connector/1.0");

        // Execute with timeout
        let response = request
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: format!("{}.{}", self.manifest.connector.id, tool_name),
                reason: format!("HTTP request failed: {e}"),
            })?;

        let status = response.status();

        // Handle rate limiting (429)
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(AgentOSError::RateLimited {
                detail: format!("Rate limited by {} (HTTP 429)", self.manifest.connector.id),
            });
        }

        // Read response body (truncated)
        let max_bytes = tool_def
            .response_map
            .as_ref()
            .and_then(|rm| rm.max_bytes)
            .unwrap_or(self.manifest.connector.max_response_bytes);

        let body_bytes = response
            .bytes()
            .await
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: format!("{}.{}", self.manifest.connector.id, tool_name),
                reason: format!("Failed to read response body: {e}"),
            })?;

        // Truncate if needed
        let body_str = if body_bytes.len() > max_bytes {
            let truncated = String::from_utf8_lossy(&body_bytes[..max_bytes]);
            format!(
                "{}... [truncated, {} bytes total]",
                truncated,
                body_bytes.len()
            )
        } else {
            String::from_utf8_lossy(&body_bytes).to_string()
        };

        // Handle non-success status codes
        if !status.is_success() {
            return Err(AgentOSError::ToolExecutionFailed {
                tool_name: format!("{}.{}", self.manifest.connector.id, tool_name),
                reason: format!(
                    "HTTP {}: {}",
                    status.as_u16(),
                    // char-safe truncation — byte slicing an external response
                    // body can panic mid-UTF-8-sequence
                    body_str.chars().take(500).collect::<String>()
                ),
            });
        }

        // Parse as JSON
        let response_json: serde_json::Value = serde_json::from_str(&body_str)
            .unwrap_or_else(|_| serde_json::json!({ "raw_response": body_str }));

        // Apply response map if configured
        let result = match &tool_def.response_map {
            Some(map) => apply_response_map(&response_json, map),
            None => response_json,
        };

        Ok(result)
    }

    /// Inject the appropriate authentication header based on the connector's auth config.
    async fn inject_auth(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder, AgentOSError> {
        match &self.manifest.connector.auth {
            AuthConfig::Bearer { vault_key } => {
                let token = self.vault.get(vault_key).await?;
                Ok(request.bearer_auth(token.as_str()))
            }
            AuthConfig::OAuth2 { .. } => {
                let oauth_store = self.vault.oauth_store();
                let cred = oauth_store.get(&self.manifest.connector.id).await?;
                Ok(request.bearer_auth(&cred.access_token))
            }
            AuthConfig::ApiKey { header, vault_key } => {
                let key = self.vault.get(vault_key).await?;
                Ok(request.header(header, key.as_str()))
            }
            AuthConfig::Basic {
                username_vault_key,
                password_vault_key,
            } => {
                let username = self.vault.get(username_vault_key).await?;
                let password = self.vault.get(password_vault_key).await?;
                Ok(request.basic_auth(username.as_str(), Some(password.as_str())))
            }
            AuthConfig::None => Ok(request),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_manifest() -> ConnectorManifest {
        ConnectorManifest {
            connector: ConnectorInfo {
                id: "test".into(),
                name: "Test API".into(),
                version: "1.0.0".into(),
                description: "Test connector".into(),
                base_url: "https://api.example.com".into(),
                auth: AuthConfig::None,
                rate_limit: None,
                max_response_bytes: 32768,
            },
            tools: vec![
                ConnectorToolDef {
                    name: "get_thing".into(),
                    description: "Get a thing".into(),
                    method: HttpMethod::Get,
                    path: "/things/{id}".into(),
                    input_schema: None,
                    response_map: None,
                    query_params: vec!["fields".into()],
                    body_fields: vec![],
                },
                ConnectorToolDef {
                    name: "create_thing".into(),
                    description: "Create a thing".into(),
                    method: HttpMethod::Post,
                    path: "/things".into(),
                    input_schema: None,
                    response_map: None,
                    query_params: vec![],
                    body_fields: vec![],
                },
            ],
        }
    }

    #[test]
    fn test_tool_names() {
        let vault = make_test_vault();
        let proxy = ConnectorProxy::new(make_manifest(), vault);
        assert_eq!(proxy.tool_names(), vec!["get_thing", "create_thing"]);
    }

    #[test]
    fn test_namespaced_tool_names() {
        let vault = make_test_vault();
        let proxy = ConnectorProxy::new(make_manifest(), vault);
        assert_eq!(
            proxy.namespaced_tool_names(),
            vec!["test.get_thing", "test.create_thing"]
        );
    }

    #[test]
    fn test_connector_id() {
        let vault = make_test_vault();
        let proxy = ConnectorProxy::new(make_manifest(), vault);
        assert_eq!(proxy.connector_id(), "test");
    }

    fn make_test_vault() -> Arc<SecretsVault> {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("vault.db");
        let audit_path = tmp.path().join("audit.db");
        let audit = Arc::new(agentos_audit::AuditLog::open(&audit_path).unwrap());
        let passphrase = agentos_vault::ZeroizingString::new("test".into());
        let vault = SecretsVault::initialize(&db_path, &passphrase, audit).unwrap();
        // Leak the TempDir so the vault file persists for the test
        std::mem::forget(tmp);
        Arc::new(vault)
    }
}
