//! DTO for the read-only LLM provider catalog.

use serde::{Deserialize, Serialize};

/// One LLM provider available to `POST /api/v1/agents` — either a built-in
/// adapter (`source = "built-in"`) or an entry from `config/providers.toml`
/// (`source = "catalog"`).
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ApiProvider {
    pub name: String,
    pub display_name: String,
    /// `"built-in"` or `"catalog"`.
    pub source: String,
    /// Environment variable the API key is read from (empty for local providers).
    #[serde(default)]
    pub api_key_env: String,
    /// Whether a key is present for `api_key_env` (never the key itself).
    pub api_key_set: bool,
    #[serde(default)]
    pub default_model: String,
    /// Wire protocol a catalog provider speaks (e.g. `"openai"`); empty for built-ins.
    #[serde(default)]
    pub compatible_with: String,
    /// Models the catalog lists for this provider; empty for built-ins.
    #[serde(default)]
    pub models: Vec<String>,
}
