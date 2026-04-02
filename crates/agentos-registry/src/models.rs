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
    pub artifact_type: String,
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
    pub artifact_type: String,
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

/// A user review for a registry entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Review {
    pub id: i64,
    pub tool_name: String,
    pub author_key: String,
    pub rating: u8,
    pub body: Option<String>,
    pub created_at: String,
}

/// Request body for submitting a review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewRequest {
    pub author_key: String,
    pub rating: u8,
    pub body: Option<String>,
}

/// Aggregate statistics for the registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryStats {
    pub total_tools: i64,
    pub total_skills: i64,
    pub total_reviews: i64,
}
