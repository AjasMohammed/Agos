use agentos_types::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionProfile {
    pub name: String,
    pub description: String,
    pub permissions: PermissionSet,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

pub struct ProfileManager {
    profiles: RwLock<HashMap<String, PermissionProfile>>,
}

impl Default for ProfileManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ProfileManager {
    pub fn new() -> Self {
        Self {
            profiles: RwLock::new(HashMap::new()),
        }
    }

    /// Create a new named profile.
    pub fn create(
        &self,
        name: &str,
        description: &str,
        permissions: PermissionSet,
    ) -> Result<(), AgentOSError> {
        let mut profiles = self.profiles.write().unwrap();
        if profiles.contains_key(name) {
            return Err(AgentOSError::SchemaValidation(format!(
                "Profile '{}' already exists",
                name
            )));
        }
        profiles.insert(
            name.to_string(),
            PermissionProfile {
                name: name.to_string(),
                description: description.to_string(),
                permissions,
                created_at: chrono::Utc::now(),
            },
        );
        Ok(())
    }

    /// Delete a profile.
    pub fn delete(&self, name: &str) -> Result<(), AgentOSError> {
        let mut profiles = self.profiles.write().unwrap();
        if profiles.remove(name).is_some() {
            Ok(())
        } else {
            Err(AgentOSError::SchemaValidation(format!(
                "Profile '{}' not found",
                name
            )))
        }
    }

    /// Get a profile by name.
    pub fn get(&self, name: &str) -> Option<PermissionProfile> {
        let profiles = self.profiles.read().unwrap();
        profiles.get(name).cloned()
    }

    /// List all profiles.
    pub fn list_all(&self) -> Vec<PermissionProfile> {
        let profiles = self.profiles.read().unwrap();
        profiles.values().cloned().collect()
    }
}
