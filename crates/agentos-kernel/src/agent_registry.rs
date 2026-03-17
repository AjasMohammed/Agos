use agentos_types::*;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

pub struct AgentRegistry {
    agents: HashMap<AgentID, AgentProfile>,
    name_index: HashMap<String, AgentID>,
    roles: HashMap<RoleID, Role>,
    role_name_index: HashMap<String, RoleID>,
    data_dir: Option<PathBuf>,
}

impl AgentRegistry {
    pub fn new() -> Self {
        let mut registry = Self {
            agents: HashMap::new(),
            name_index: HashMap::new(),
            roles: HashMap::new(),
            role_name_index: HashMap::new(),
            data_dir: None,
        };
        registry.ensure_base_role();
        registry
    }

    pub fn with_persistence(data_dir: PathBuf) -> Self {
        let mut registry = Self {
            agents: HashMap::new(),
            name_index: HashMap::new(),
            roles: HashMap::new(),
            role_name_index: HashMap::new(),
            data_dir: Some(data_dir),
        };
        registry.load_from_disk();
        registry.ensure_base_role();
        registry.save_to_disk();
        registry
    }

    fn ensure_base_role(&mut self) {
        if !self.role_name_index.contains_key("base") {
            let mut base_role = Role::new(
                "base".to_string(),
                "Default role with minimal permissions".to_string(),
            );
            base_role
                .permissions
                .grant("fs.user_data".to_string(), true, true, false, None);
            self.register_role(base_role);
        }
    }

    pub fn register_role(&mut self, role: Role) -> RoleID {
        let id = role.id;
        self.role_name_index.insert(role.name.clone(), id);
        self.roles.insert(id, role);
        self.save_to_disk();
        id
    }

    pub fn get_role_by_name(&self, name: &str) -> Option<&Role> {
        self.role_name_index
            .get(name)
            .and_then(|id| self.roles.get(id))
    }

    pub fn get_role_by_id(&self, id: &RoleID) -> Option<&Role> {
        self.roles.get(id)
    }

    pub fn list_roles(&self) -> Vec<&Role> {
        self.roles.values().collect()
    }

    pub fn unregister_role(&mut self, id: &RoleID) -> Result<(), String> {
        if let Some(role) = self.roles.get(id) {
            if role.name == "base" {
                return Err("Cannot delete the base role".to_string());
            }
            // Ensure no agent is using this roll (optional, but good practice)
            let in_use = self.agents.values().any(|a| a.roles.contains(&role.name));
            if in_use {
                return Err("Cannot delete role while it is assigned to agents".to_string());
            }
            let name = role.name.clone();
            self.roles.remove(id);
            self.role_name_index.remove(&name);
            self.save_to_disk();
        }
        Ok(())
    }

    pub fn update_role_permissions(
        &mut self,
        role_name: &str,
        new_perms: PermissionSet,
    ) -> Result<(), String> {
        if let Some(id) = self.role_name_index.get(role_name).copied() {
            if let Some(role) = self.roles.get_mut(&id) {
                role.permissions = new_perms;
                self.save_to_disk();
                return Ok(());
            }
        }
        Err(format!("Role '{}' not found", role_name))
    }

    pub fn register(&mut self, profile: AgentProfile) -> AgentID {
        let id = profile.id;
        self.name_index.insert(profile.name.clone(), id);
        self.agents.insert(id, profile);
        self.save_to_disk();
        id
    }

    pub fn get_by_id(&self, id: &AgentID) -> Option<&AgentProfile> {
        self.agents.get(id)
    }

    pub fn get_by_name(&self, name: &str) -> Option<&AgentProfile> {
        self.name_index.get(name).and_then(|id| self.agents.get(id))
    }

    pub fn list_all(&self) -> Vec<&AgentProfile> {
        self.agents.values().collect()
    }

    pub fn update_status(&mut self, id: &AgentID, status: AgentStatus) {
        if let Some(agent) = self.agents.get_mut(id) {
            agent.status = status;
            self.save_to_disk();
        }
    }

    pub fn remove(&mut self, id: &AgentID) {
        if let Some(agent) = self.agents.remove(id) {
            self.name_index.remove(&agent.name);
            self.save_to_disk();
        }
    }

    pub fn assign_role(&mut self, agent_id: &AgentID, role_name: String) -> Result<(), String> {
        if !self.role_name_index.contains_key(&role_name) {
            return Err(format!("Role '{}' does not exist", role_name));
        }
        if let Some(agent) = self.agents.get_mut(agent_id) {
            if !agent.roles.contains(&role_name) {
                agent.roles.push(role_name);
                self.save_to_disk();
            }
            Ok(())
        } else {
            Err("Agent not found".to_string())
        }
    }

    pub fn remove_role(&mut self, agent_id: &AgentID, role_name: &str) -> Result<(), String> {
        if let Some(agent) = self.agents.get_mut(agent_id) {
            agent.roles.retain(|r| r != role_name);
            self.save_to_disk();
            Ok(())
        } else {
            Err("Agent not found".to_string())
        }
    }

    /// Computes the effective permissions for an agent by combining its direct permissions
    /// with the permissions of all its assigned roles.
    pub fn compute_effective_permissions(&self, agent_id: &AgentID) -> PermissionSet {
        let mut effective = PermissionSet::new();
        if let Some(agent) = self.agents.get(agent_id) {
            // First, apply direct agent permissions
            for entry in agent.permissions.entries() {
                effective.grant(
                    entry.resource.clone(),
                    entry.read,
                    entry.write,
                    entry.execute,
                    entry.expires_at,
                );
            }
            // Next, merge in all role permissions
            for role_name in &agent.roles {
                if let Some(role) = self.get_role_by_name(role_name) {
                    for entry in role.permissions.entries() {
                        effective.grant(
                            entry.resource.clone(),
                            entry.read,
                            entry.write,
                            entry.execute,
                            entry.expires_at,
                        );
                    }
                }
            }
        }
        effective
    }

    // --- Persistence ---

    fn load_from_disk(&mut self) {
        if let Some(dir) = &self.data_dir {
            let agents_file = dir.join("agents.json");
            let roles_file = dir.join("roles.json");

            if let Ok(content) = fs::read_to_string(&roles_file) {
                if let Ok(roles_list) = serde_json::from_str::<Vec<Role>>(&content) {
                    for role in roles_list {
                        let id = role.id;
                        self.role_name_index.insert(role.name.clone(), id);
                        self.roles.insert(id, role);
                    }
                }
            }

            if let Ok(content) = fs::read_to_string(&agents_file) {
                if let Ok(agents_list) = serde_json::from_str::<Vec<AgentProfile>>(&content) {
                    for agent in agents_list {
                        let id = agent.id;
                        self.name_index.insert(agent.name.clone(), id);
                        self.agents.insert(id, agent);
                    }
                }
            }
        }
    }

    fn save_to_disk(&self) {
        if let Some(dir) = &self.data_dir {
            let agents_file = dir.join("agents.json");
            let roles_file = dir.join("roles.json");

            let agents_list: Vec<&AgentProfile> = self.agents.values().collect();
            match serde_json::to_string_pretty(&agents_list) {
                Ok(json) => {
                    if let Err(e) = fs::write(&agents_file, json) {
                        tracing::warn!(
                            path = %agents_file.display(),
                            error = %e,
                            "Failed to persist agent registry to disk — in-memory state diverges"
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to serialize agent registry");
                }
            }

            let roles_list: Vec<&Role> = self.roles.values().collect();
            match serde_json::to_string_pretty(&roles_list) {
                Ok(json) => {
                    if let Err(e) = fs::write(&roles_file, json) {
                        tracing::warn!(
                            path = %roles_file.display(),
                            error = %e,
                            "Failed to persist role registry to disk — in-memory state diverges"
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to serialize role registry");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_effective_permissions() {
        let mut registry = AgentRegistry::new();
        let mut agent = AgentProfile {
            id: AgentID::new(),
            name: "test".to_string(),
            provider: LLMProvider::Ollama,
            model: "test".to_string(),
            status: AgentStatus::Online,
            permissions: PermissionSet::new(),
            roles: vec!["base".to_string()],
            current_task: None,
            description: "".to_string(),
            created_at: chrono::Utc::now(),
            last_active: chrono::Utc::now(),
            public_key_hex: None,
        };
        agent
            .permissions
            .grant("custom".to_string(), true, false, false, None);

        let agent_id = registry.register(agent);
        let perms = registry.compute_effective_permissions(&agent_id);

        assert!(perms.check("fs.user_data", PermissionOp::Write));
        assert!(perms.check("custom", PermissionOp::Read));
    }
}

impl Default for AgentRegistry {
    fn default() -> Self {
        Self::new()
    }
}
