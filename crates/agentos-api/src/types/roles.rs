//! DTOs for the role-management (governance) REST surface.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A reusable role that groups a set of permissions.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ApiRole {
    pub name: String,
    pub description: String,
    /// Permissions in `"resource:rwxqo"` form (only the set flags are included).
    pub permissions: Vec<String>,
    pub created_at: DateTime<Utc>,
}

/// Request body for `POST /api/v1/roles`.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CreateRoleRequest {
    pub name: String,
    pub description: Option<String>,
    /// Permissions in `"resource:rwxqo"` form, e.g. `["fs.user_data:rw", "memory.semantic:rq"]`.
    pub permissions: Vec<String>,
}
