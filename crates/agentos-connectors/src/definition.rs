use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Defines a connector to an external API service.
///
/// Connectors are loaded from TOML manifest files in the `connectors/` directory.
/// Each connector defines a namespace of tools backed by a single external API,
/// along with the authentication method and rate limit configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorManifest {
    pub connector: ConnectorInfo,
    #[serde(default)]
    pub tools: Vec<ConnectorToolDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub base_url: String,
    pub auth: AuthConfig,
    #[serde(default)]
    pub rate_limit: Option<RateLimitConfig>,
    /// Default response size limit in bytes (default: 32768).
    #[serde(default = "default_max_response_bytes")]
    pub max_response_bytes: usize,
}

fn default_max_response_bytes() -> usize {
    32768
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthConfig {
    /// Authorization: Bearer <token> from vault
    Bearer {
        /// Vault secret name that holds the token
        vault_key: String,
    },
    /// OAuth2 (token fetched from vault's OAuth store by connector_id)
    #[serde(rename = "oauth2")]
    OAuth2 {
        #[serde(default)]
        scopes: Vec<String>,
    },
    /// Custom API key header
    ApiKey { header: String, vault_key: String },
    /// HTTP Basic auth
    Basic {
        username_vault_key: String,
        password_vault_key: String,
    },
    /// No authentication required (public APIs)
    None,
}

/// Defines a single tool within a connector.
///
/// When an agent calls `github.create_issue`, the `github` prefix is the
/// connector ID and `create_issue` is the tool name. The connector proxy
/// handles URL construction, auth injection, and response mapping.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorToolDef {
    pub name: String,
    pub description: String,
    pub method: HttpMethod,
    pub path: String,
    #[serde(default)]
    pub input_schema: Option<serde_json::Value>,
    #[serde(default)]
    pub response_map: Option<ResponseMap>,
    #[serde(default)]
    pub query_params: Vec<String>,
    /// Fields from input to use as the JSON body (POST/PUT/PATCH).
    /// If empty, the entire input (minus path params and query params) is sent.
    #[serde(default)]
    pub body_fields: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl HttpMethod {
    pub fn to_reqwest(&self) -> reqwest::Method {
        match self {
            HttpMethod::Get => reqwest::Method::GET,
            HttpMethod::Post => reqwest::Method::POST,
            HttpMethod::Put => reqwest::Method::PUT,
            HttpMethod::Patch => reqwest::Method::PATCH,
            HttpMethod::Delete => reqwest::Method::DELETE,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    pub requests_per_minute: u32,
    pub burst: u32,
}

/// Controls which fields from the HTTP response JSON are returned to the agent.
/// This prevents flooding the LLM context with large response payloads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseMap {
    /// JSON pointer paths to extract (e.g., "/id", "/html_url", "/title").
    /// If empty, the full response body is returned (truncated).
    #[serde(default)]
    pub fields: Vec<ResponseField>,
    /// Maximum response body bytes before truncation (overrides connector default).
    #[serde(default)]
    pub max_bytes: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseField {
    /// JSON pointer (RFC 6901) to extract from the response.
    pub source: String,
    /// Optional key rename in the output.
    #[serde(default)]
    pub rename: Option<String>,
}

/// Resolves path template variables like `/repos/{owner}/{repo}/issues`.
pub fn resolve_path_template(template: &str, params: &serde_json::Value) -> Result<String, String> {
    let mut result = template.to_string();
    if let Some(obj) = params.as_object() {
        for (key, value) in obj {
            let placeholder = format!("{{{key}}}");
            if result.contains(&placeholder) {
                let val_str = match value {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string().trim_matches('"').to_string(),
                };
                // Reject path-traversal and URL-manipulation characters
                if val_str.contains("..") || val_str.contains('#') || val_str.contains('?') {
                    return Err(format!(
                        "Path parameter '{key}' contains forbidden characters: {val_str}"
                    ));
                }
                // URL-encode the value to prevent injection
                let encoded = percent_encoding::utf8_percent_encode(
                    &val_str,
                    percent_encoding::NON_ALPHANUMERIC,
                )
                .to_string();
                result = result.replace(&placeholder, &encoded);
            }
        }
    }

    // Check for unresolved placeholders
    if result.contains('{') && result.contains('}') {
        let unresolved: Vec<&str> = result
            .split('{')
            .skip(1)
            .filter_map(|s| s.split('}').next())
            .collect();
        return Err(format!(
            "Unresolved path parameters: {}",
            unresolved.join(", ")
        ));
    }

    Ok(result)
}

/// Extract path parameter names from a URL template.
pub fn extract_path_params(template: &str) -> Vec<String> {
    template
        .split('{')
        .skip(1)
        .filter_map(|s| s.split('}').next().map(|p| p.to_string()))
        .collect()
}

/// Apply a ResponseMap to extract specific fields from a JSON response.
pub fn apply_response_map(response: &serde_json::Value, map: &ResponseMap) -> serde_json::Value {
    if map.fields.is_empty() {
        return response.clone();
    }

    let mut result = serde_json::Map::new();
    for field in &map.fields {
        if let Some(value) = response.pointer(&field.source) {
            let key = field
                .rename
                .as_deref()
                .unwrap_or_else(|| field.source.rsplit('/').next().unwrap_or(&field.source))
                .to_string();
            result.insert(key, value.clone());
        }
    }
    serde_json::Value::Object(result)
}

/// Build a HashMap of path, query, and body parameters from the tool input.
pub fn partition_params(
    input: &serde_json::Value,
    tool_def: &ConnectorToolDef,
) -> (
    HashMap<String, serde_json::Value>,
    HashMap<String, String>,
    serde_json::Value,
) {
    let path_param_names = extract_path_params(&tool_def.path);
    let obj = input.as_object().cloned().unwrap_or_default();

    let mut path_params = HashMap::new();
    let mut query_params = HashMap::new();
    let mut body = serde_json::Map::new();

    for (key, value) in &obj {
        if path_param_names.contains(key) {
            path_params.insert(key.clone(), value.clone());
        } else if tool_def.query_params.contains(key) {
            query_params.insert(key.clone(), value.to_string().trim_matches('"').to_string());
        } else {
            body.insert(key.clone(), value.clone());
        }
    }

    (path_params, query_params, serde_json::Value::Object(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_path_template() {
        let params = serde_json::json!({"owner": "foo", "repo": "bar"});
        let result = resolve_path_template("/repos/{owner}/{repo}/issues", &params).unwrap();
        assert_eq!(result, "/repos/foo/bar/issues");
    }

    #[test]
    fn test_resolve_path_template_missing_param() {
        let params = serde_json::json!({"owner": "foo"});
        let result = resolve_path_template("/repos/{owner}/{repo}/issues", &params);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("repo"));
    }

    #[test]
    fn test_resolve_path_template_rejects_traversal() {
        let params = serde_json::json!({"owner": "../../admin", "repo": "bar"});
        let result = resolve_path_template("/repos/{owner}/{repo}/issues", &params);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("forbidden"));
    }

    #[test]
    fn test_resolve_path_template_rejects_query_injection() {
        let params = serde_json::json!({"owner": "foo?admin=true", "repo": "bar"});
        let result = resolve_path_template("/repos/{owner}/{repo}/issues", &params);
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_path_params() {
        let params = extract_path_params("/repos/{owner}/{repo}/issues");
        assert_eq!(params, vec!["owner", "repo"]);
    }

    #[test]
    fn test_extract_path_params_none() {
        let params = extract_path_params("/user/repos");
        assert!(params.is_empty());
    }

    #[test]
    fn test_apply_response_map() {
        let response = serde_json::json!({
            "id": 42,
            "html_url": "https://github.com/foo/bar/issues/1",
            "title": "Bug report",
            "body": "Very long body text...",
            "labels": [{"name": "bug"}]
        });

        let map = ResponseMap {
            fields: vec![
                ResponseField {
                    source: "/id".into(),
                    rename: None,
                },
                ResponseField {
                    source: "/html_url".into(),
                    rename: Some("url".into()),
                },
                ResponseField {
                    source: "/title".into(),
                    rename: None,
                },
            ],
            max_bytes: None,
        };

        let result = apply_response_map(&response, &map);
        assert_eq!(result["id"], 42);
        assert_eq!(result["url"], "https://github.com/foo/bar/issues/1");
        assert_eq!(result["title"], "Bug report");
        assert!(result.get("body").is_none());
        assert!(result.get("labels").is_none());
    }

    #[test]
    fn test_partition_params() {
        let tool = ConnectorToolDef {
            name: "create_issue".into(),
            description: "test".into(),
            method: HttpMethod::Post,
            path: "/repos/{owner}/{repo}/issues".into(),
            input_schema: None,
            response_map: None,
            query_params: vec!["per_page".into()],
            body_fields: vec![],
        };

        let input = serde_json::json!({
            "owner": "foo",
            "repo": "bar",
            "per_page": "30",
            "title": "Bug",
            "body": "Details"
        });

        let (path, query, body) = partition_params(&input, &tool);
        assert_eq!(path.get("owner").unwrap(), "foo");
        assert_eq!(path.get("repo").unwrap(), "bar");
        assert_eq!(query.get("per_page").unwrap(), "30");
        assert_eq!(body["title"], "Bug");
        assert_eq!(body["body"], "Details");
        assert!(body.get("owner").is_none());
    }

    #[test]
    fn test_deserialize_manifest() {
        let toml_str = r#"
[connector]
id = "github"
name = "GitHub"
version = "1.0.0"
description = "GitHub API connector"
base_url = "https://api.github.com"

[connector.auth]
type = "oauth2"
scopes = ["repo", "read:org"]

[[tools]]
name = "list_repos"
description = "List repositories for the authenticated user"
method = "get"
path = "/user/repos"
query_params = ["per_page", "sort"]

[[tools]]
name = "create_issue"
description = "Create an issue in a repository"
method = "post"
path = "/repos/{owner}/{repo}/issues"
"#;

        let manifest: ConnectorManifest = toml::from_str(toml_str).unwrap();
        assert_eq!(manifest.connector.id, "github");
        assert_eq!(manifest.connector.base_url, "https://api.github.com");
        assert!(matches!(manifest.connector.auth, AuthConfig::OAuth2 { .. }));
        assert_eq!(manifest.tools.len(), 2);
        assert_eq!(manifest.tools[0].name, "list_repos");
        assert_eq!(manifest.tools[1].name, "create_issue");
        assert_eq!(manifest.tools[0].query_params, vec!["per_page", "sort"]);
    }
}
