use serde::{Deserialize, Serialize};

/// Full tool entry stored in the registry database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolEntry {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub author_pubkey: String,
    pub signature: String,
    pub tags: Vec<String>,
    pub manifest_toml: String,
    pub downloads: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// Lightweight result returned by search and list endpoints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSearchResult {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub downloads: i64,
    pub tags: Vec<String>,
}

/// Request body for the publish endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishRequest {
    pub manifest_toml: String,
}

/// Response body for the publish endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishResponse {
    pub name: String,
    pub version: String,
}

/// Error response body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
}
