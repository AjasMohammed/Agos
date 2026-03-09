use crate::capability::PermissionSet;
use crate::ids::RoleID;
use serde::{Deserialize, Serialize};

/// A reusable profile that groups permissions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Role {
    pub id: RoleID,
    pub name: String,
    pub permissions: PermissionSet,
    pub description: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl Role {
    pub fn new(name: String, description: String) -> Self {
        Self {
            id: RoleID::new(),
            name,
            permissions: PermissionSet::new(),
            description,
            created_at: chrono::Utc::now(),
        }
    }
}
