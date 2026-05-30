//! DTOs for the configuration surface (`/api/v1/config`).

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// The full configuration tree, with secret-bearing leaves redacted.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ConfigTree {
    /// The redacted config document as a JSON value.
    #[schema(value_type = Object)]
    pub config: serde_json::Value,
}

/// A single dotted-key lookup result.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ConfigValue {
    /// The dotted key, e.g. `"llm.primary"`.
    pub key: String,
    /// The resolved value.
    #[schema(value_type = Object)]
    pub value: serde_json::Value,
}

/// Request body for `PUT /api/v1/config/{key}`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SetConfigRequest {
    /// The new value to write at the dotted key.
    #[schema(value_type = Object)]
    pub value: serde_json::Value,
}
