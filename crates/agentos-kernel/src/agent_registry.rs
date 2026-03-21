use agentos_types::*;
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Maximum size for a registry JSON file (64 MiB). Guards against OOM on read.
const MAX_REGISTRY_FILE_BYTES: u64 = 64 * 1024 * 1024;

pub struct AgentRegistry {
    agents: HashMap<AgentID, AgentProfile>,
    name_index: HashMap<String, AgentID>,
    roles: HashMap<RoleID, Role>,
    role_name_index: HashMap<String, RoleID>,
    data_dir: Option<PathBuf>,
    /// Corruption events detected during `load_from_disk` — `(file_path, parse_error)`.
    /// Drain via [`AgentRegistry::drain_corruption_events`] and forward to the audit log.
    corruption_events: Vec<(String, String)>,
    /// Last write error from `save_to_disk`, if any. Allows callers to detect
    /// persistent disk failures programmatically without changing every call site
    /// to handle `Result`. Reset to `None` on each successful write.
    last_write_error: Option<String>,
}

impl AgentRegistry {
    pub fn new() -> Self {
        let mut registry = Self {
            agents: HashMap::new(),
            name_index: HashMap::new(),
            roles: HashMap::new(),
            role_name_index: HashMap::new(),
            data_dir: None,
            corruption_events: Vec::new(),
            last_write_error: None,
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
            corruption_events: Vec::new(),
            last_write_error: None,
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
            // Ensure no agent is using this role (optional, but good practice)
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

    /// Returns only agents that are Online, Idle, or Busy (not Offline).
    pub fn list_online(&self) -> Vec<&AgentProfile> {
        self.agents
            .values()
            .filter(|a| a.status != AgentStatus::Offline)
            .collect()
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

    /// Remove the Offline entry for `name` if one exists.
    /// Called before registering a replacement agent with the same name but a
    /// different provider/model, to prevent unbounded growth of orphaned entries.
    pub fn remove_offline_by_name(&mut self, name: &str) {
        if let Some(&id) = self.name_index.get(name) {
            if let Some(agent) = self.agents.get(&id) {
                if agent.status == AgentStatus::Offline {
                    self.agents.remove(&id);
                    self.name_index.remove(name);
                    self.save_to_disk();
                }
            }
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

    /// Update an agent's direct permission set and persist to disk.
    pub fn update_agent_permissions(
        &mut self,
        agent_id: &AgentID,
        perms: PermissionSet,
    ) -> Result<(), String> {
        if let Some(agent) = self.agents.get_mut(agent_id) {
            agent.permissions = perms;
            self.save_to_disk();
            Ok(())
        } else {
            Err(format!("Agent '{}' not found", agent_id))
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
                effective.grant_entry(entry);
            }
            // Next, merge in all role permissions
            for role_name in &agent.roles {
                if let Some(role) = self.get_role_by_name(role_name) {
                    for entry in role.permissions.entries() {
                        effective.grant_entry(entry);
                    }
                }
            }
        }
        effective
    }

    // --- Persistence ---

    /// Drain corruption events recorded during the last `load_from_disk` call.
    ///
    /// Each entry is `(file_path, parse_error)`. The kernel's startup sequence should
    /// call this after construction and write each entry to the audit log as a
    /// `KernelSubsystemError` event for post-incident analysis.
    pub fn drain_corruption_events(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.corruption_events)
    }

    /// Attempt to restore in-memory registry state from backup files (`*.json.bak`).
    ///
    /// Parses both backup files before mutating any in-memory state, so either both
    /// files are recovered or neither is (atomic with respect to the in-memory state).
    ///
    /// Returns `Ok(true)` if at least one backup was successfully loaded, `Ok(false)`
    /// if no backup files exist, or `Err` if the backup files are themselves corrupt.
    pub fn recover_registry(&mut self) -> Result<bool, String> {
        let Some(dir) = self.data_dir.clone() else {
            return Ok(false);
        };

        let agents_bak = dir.join("agents.json.bak");
        let roles_bak = dir.join("roles.json.bak");

        // Parse both backups BEFORE mutating self (atomic with respect to in-memory state).
        let roles_data: Option<Vec<Role>> = if roles_bak.exists() {
            Some(
                Self::read_registry_file(&roles_bak)
                    .map_err(|e| e.to_string())
                    .and_then(|c| serde_json::from_str(&c).map_err(|e| e.to_string()))
                    .map_err(|e| {
                        format!(
                            "Failed to recover roles from backup {}: {}",
                            roles_bak.display(),
                            e
                        )
                    })?,
            )
        } else {
            None
        };

        let agents_data: Option<Vec<AgentProfile>> = if agents_bak.exists() {
            Some(
                Self::read_registry_file(&agents_bak)
                    .map_err(|e| e.to_string())
                    .and_then(|c| serde_json::from_str(&c).map_err(|e| e.to_string()))
                    .map_err(|e| {
                        format!(
                            "Failed to recover agents from backup {}: {}",
                            agents_bak.display(),
                            e
                        )
                    })?,
            )
        } else {
            None
        };

        // Both parse checks passed — now commit to in-memory state.
        let mut recovered = false;

        if let Some(roles_list) = roles_data {
            self.roles.clear();
            self.role_name_index.clear();
            for role in roles_list {
                let id = role.id;
                self.role_name_index.insert(role.name.clone(), id);
                self.roles.insert(id, role);
            }
            tracing::warn!(path = %roles_bak.display(), "Role registry recovered from backup");
            recovered = true;
        }

        if let Some(agents_list) = agents_data {
            self.agents.clear();
            self.name_index.clear();
            for mut agent in agents_list {
                agent.status = AgentStatus::Offline;
                let id = agent.id;
                self.name_index.insert(agent.name.clone(), id);
                self.agents.insert(id, agent);
            }
            tracing::warn!(path = %agents_bak.display(), "Agent registry recovered from backup");
            recovered = true;
        }

        if recovered {
            self.ensure_base_role();
            self.save_to_disk();
        }

        Ok(recovered)
    }

    fn load_from_disk(&mut self) {
        let Some(dir) = self.data_dir.clone() else {
            return;
        };

        let roles_file = dir.join("roles.json");
        let agents_file = dir.join("agents.json");

        let (roles_list, roles_from_bak) =
            Self::load_json_list_with_backup::<Role>(&roles_file, &mut self.corruption_events);
        for role in roles_list {
            let id = role.id;
            self.role_name_index.insert(role.name.clone(), id);
            self.roles.insert(id, role);
        }
        // Heal the primary from backup so that the subsequent `save_to_disk` (called by
        // `with_persistence`) does not copy the still-corrupt primary into `.bak` and
        // destroy the only valid backup.
        if roles_from_bak {
            let bak = Self::bak_path(&roles_file);
            if let Err(e) = fs::copy(&bak, &roles_file) {
                tracing::warn!(
                    path = %roles_file.display(),
                    error = %e,
                    "Failed to heal roles.json from backup — backup may be overwritten on next write"
                );
            }
        }

        let (agents_list, agents_from_bak) = Self::load_json_list_with_backup::<AgentProfile>(
            &agents_file,
            &mut self.corruption_events,
        );
        for mut agent in agents_list {
            // All loaded agents are Offline until they explicitly reconnect.
            // This prevents ghost-Online entries from appearing after a
            // kernel crash or unclean shutdown.
            agent.status = AgentStatus::Offline;
            let id = agent.id;
            self.name_index.insert(agent.name.clone(), id);
            self.agents.insert(id, agent);
        }
        // Same heal logic for agents.
        if agents_from_bak {
            let bak = Self::bak_path(&agents_file);
            if let Err(e) = fs::copy(&bak, &agents_file) {
                tracing::warn!(
                    path = %agents_file.display(),
                    error = %e,
                    "Failed to heal agents.json from backup — backup may be overwritten on next write"
                );
            }
        }
    }

    /// Read a JSON array from `path`, falling back to `{path}.bak` on parse failure.
    ///
    /// Returns `(data, used_backup)`. On total failure (both files corrupt or absent)
    /// returns an empty `Vec`. Parse errors are appended to `events` as
    /// `(file_path, error_message)` pairs so the caller can forward them to the audit log.
    fn load_json_list_with_backup<T: DeserializeOwned>(
        path: &Path,
        events: &mut Vec<(String, String)>,
    ) -> (Vec<T>, bool) {
        let bak_path = Self::bak_path(path);

        match Self::read_registry_file(path) {
            Ok(content) => match serde_json::from_str::<Vec<T>>(&content) {
                Ok(data) => return (data, false),
                Err(e) => {
                    let path_str = path.display().to_string();
                    let err_str = e.to_string();
                    tracing::error!(
                        path = %path_str,
                        error = %err_str,
                        "CRITICAL: Registry file is corrupted — attempting backup recovery"
                    );
                    events.push((path_str, err_str));
                }
            },
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "Failed to read registry file"
                );
            }
            _ => {} // Not found — normal on first boot.
        }

        // Primary file failed; try the backup.
        if bak_path.exists() {
            match Self::read_registry_file(&bak_path) {
                Ok(content) => match serde_json::from_str::<Vec<T>>(&content) {
                    Ok(data) => {
                        tracing::warn!(
                            bak = %bak_path.display(),
                            "Registry recovered from backup — operator should inspect primary file"
                        );
                        return (data, true);
                    }
                    Err(e) => {
                        let path_str = bak_path.display().to_string();
                        let err_str = e.to_string();
                        tracing::error!(
                            path = %path_str,
                            error = %err_str,
                            "CRITICAL: Both registry and backup are corrupted — starting with empty registry"
                        );
                        events.push((path_str, err_str));
                    }
                },
                Err(e) => {
                    tracing::warn!(
                        path = %bak_path.display(),
                        error = %e,
                        "Failed to read backup registry file"
                    );
                }
            }
        }

        (Vec::new(), false)
    }

    /// Returns the last write error from `save_to_disk`, if any. Allows callers
    /// (e.g. the kernel health monitor) to detect persistent disk failures.
    pub fn last_write_error(&self) -> Option<&str> {
        self.last_write_error.as_deref()
    }

    fn save_to_disk(&mut self) {
        if let Some(dir) = &self.data_dir {
            let agents_file = dir.join("agents.json");
            let roles_file = dir.join("roles.json");

            // Sort for deterministic output — prevents spurious .bak divergence when
            // HashMap iteration order changes between runs.
            let mut agents_list: Vec<&AgentProfile> = self.agents.values().collect();
            agents_list.sort_by_key(|a| a.id.to_string());
            if let Err(e) = Self::write_json_with_backup(&agents_file, agents_list) {
                self.last_write_error = Some(e.clone());
                return;
            }

            let mut roles_list: Vec<&Role> = self.roles.values().collect();
            roles_list.sort_by_key(|r| r.id.to_string());
            if let Err(e) = Self::write_json_with_backup(&roles_file, roles_list) {
                self.last_write_error = Some(e);
                return;
            }

            // Both files written successfully — clear any previous error.
            self.last_write_error = None;
        }
    }

    /// Serialize `data` to `path` atomically (via `.tmp` + rename).
    ///
    /// Before overwriting the live file, the existing content is copied to
    /// `{path}.bak` so a previous valid state can be recovered if the primary
    /// is ever found corrupt at the next startup.
    fn write_json_with_backup<T: serde::Serialize>(path: &Path, data: T) -> Result<(), String> {
        let bak_path = Self::bak_path(path);
        let tmp_path = Self::tmp_path(path);

        let json = serde_json::to_string_pretty(&data).map_err(|e| {
            let msg = format!("Failed to serialize registry {}: {}", path.display(), e);
            tracing::warn!("{}", msg);
            msg
        })?;

        // Snapshot the current live file to .bak before overwriting.
        if path.exists() {
            if let Err(e) = fs::copy(path, &bak_path) {
                tracing::warn!(
                    path = %path.display(),
                    bak = %bak_path.display(),
                    error = %e,
                    "Failed to create registry backup — write will continue without safety copy"
                );
            }
        }

        fs::write(&tmp_path, &json)
            .and_then(|_| fs::rename(&tmp_path, path))
            .map_err(|e| {
                let msg = format!(
                    "Failed to persist registry to disk at {}: {} — in-memory state diverges",
                    path.display(),
                    e
                );
                tracing::warn!("{}", msg);
                msg
            })
    }

    /// Read a registry file, enforcing a maximum size to prevent OOM.
    fn read_registry_file(path: &Path) -> std::io::Result<String> {
        let meta = fs::metadata(path)?;
        if meta.len() > MAX_REGISTRY_FILE_BYTES {
            return Err(std::io::Error::other(format!(
                "Registry file too large: {} bytes (max {})",
                meta.len(),
                MAX_REGISTRY_FILE_BYTES
            )));
        }
        fs::read_to_string(path)
    }

    /// Build a `{path}.bak` path without going through lossy UTF-8 display.
    fn bak_path(path: &Path) -> PathBuf {
        let mut s = path.as_os_str().to_os_string();
        s.push(".bak");
        PathBuf::from(s)
    }

    /// Build a `{path}.tmp` path without going through lossy UTF-8 display.
    fn tmp_path(path: &Path) -> PathBuf {
        let mut s = path.as_os_str().to_os_string();
        s.push(".tmp");
        PathBuf::from(s)
    }
}

impl Default for AgentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_agent(name: &str) -> AgentProfile {
        AgentProfile {
            id: AgentID::new(),
            name: name.to_string(),
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
        }
    }

    #[test]
    fn test_compute_effective_permissions() {
        let mut registry = AgentRegistry::new();
        let mut agent = make_agent("test");
        agent
            .permissions
            .grant("custom".to_string(), true, false, false, None);

        let agent_id = registry.register(agent);
        let perms = registry.compute_effective_permissions(&agent_id);

        assert!(perms.check("fs.user_data", PermissionOp::Write));
        assert!(perms.check("custom", PermissionOp::Read));
    }

    #[test]
    fn test_backup_created_on_write() {
        let dir = TempDir::new().unwrap();
        let mut registry = AgentRegistry::with_persistence(dir.path().to_path_buf());
        // First registration: writes agents.json for the first time (no prior file → no .bak).
        registry.register(make_agent("alice"));
        // Second registration: copies agents.json → agents.json.bak, then overwrites.
        registry.register(make_agent("bob"));

        assert!(
            dir.path().join("agents.json.bak").exists(),
            "agents.json.bak should exist after second write"
        );
    }

    #[test]
    fn test_corrupted_primary_falls_back_to_backup_and_recovers_data() {
        let dir = TempDir::new().unwrap();

        {
            let mut r = AgentRegistry::with_persistence(dir.path().to_path_buf());
            // First register: .bak gets the pre-alice state; agents.json gets alice.
            r.register(make_agent("alice"));
            // Second register: .bak gets alice; agents.json gets alice+bob.
            r.register(make_agent("bob"));
        }

        // .bak now contains alice. Corrupt the primary file.
        fs::write(dir.path().join("agents.json"), b"{ invalid json ]]]").unwrap();

        // Reopen: load_from_disk should fall back to .bak (alice), heal the primary,
        // then save_to_disk (called by with_persistence) writes the valid state.
        let mut registry = AgentRegistry::with_persistence(dir.path().to_path_buf());

        // A corruption event must have been recorded.
        let events = registry.drain_corruption_events();
        assert!(
            !events.is_empty(),
            "Expected a corruption event for agents.json"
        );
        assert!(
            events[0].0.contains("agents.json"),
            "Corruption event should reference agents.json, got: {}",
            events[0].0
        );

        // Alice must be present — she was in the backup.
        let names: Vec<&str> = registry
            .list_all()
            .iter()
            .map(|a| a.name.as_str())
            .collect();
        assert!(
            names.contains(&"alice"),
            "Expected alice recovered from .bak, got: {:?}",
            names
        );
    }

    #[test]
    fn test_corrupted_primary_does_not_overwrite_good_backup() {
        let dir = TempDir::new().unwrap();

        {
            let mut r = AgentRegistry::with_persistence(dir.path().to_path_buf());
            r.register(make_agent("alice")); // .bak=[], agents.json=[alice]
            r.register(make_agent("bob")); // .bak=[alice], agents.json=[alice,bob]
        }

        fs::write(dir.path().join("agents.json"), b"corrupted").unwrap();

        // Reopen: fallback to .bak (alice) → heal primary → save (copies healed to .bak).
        let _registry = AgentRegistry::with_persistence(dir.path().to_path_buf());

        // The .bak must still be parseable — it should NOT have been replaced with the
        // corrupt primary.
        let bak_content = fs::read_to_string(dir.path().join("agents.json.bak")).unwrap();
        let parsed: Result<Vec<AgentProfile>, _> = serde_json::from_str(&bak_content);
        assert!(
            parsed.is_ok(),
            "agents.json.bak should still be valid JSON after recovery, got: {:?}",
            bak_content
        );
    }

    #[test]
    fn test_recover_registry_is_atomic_on_parse_failure() {
        let dir = TempDir::new().unwrap();

        {
            let mut r = AgentRegistry::with_persistence(dir.path().to_path_buf());
            r.register(make_agent("alice"));
            r.register(make_agent("bob")); // creates .bak with alice
        }

        let mut registry = AgentRegistry::with_persistence(dir.path().to_path_buf());
        // Corrupt ONLY the roles.bak so recover_registry fails partway through.
        fs::write(dir.path().join("roles.json.bak"), b"not json").unwrap();

        // Capture state before the attempted recovery.
        let names_before: Vec<String> =
            registry.list_all().iter().map(|a| a.name.clone()).collect();

        let result = registry.recover_registry();
        assert!(result.is_err(), "Expected Err when roles.bak is corrupt");

        // In-memory state must be unchanged (atomicity — roles were parsed first and failed).
        let names_after: Vec<String> = registry.list_all().iter().map(|a| a.name.clone()).collect();
        assert_eq!(
            names_before, names_after,
            "In-memory agent list must not change when recovery fails"
        );
    }

    #[test]
    fn test_recover_registry_restores_agents_from_bak() {
        let dir = TempDir::new().unwrap();

        {
            let mut r = AgentRegistry::with_persistence(dir.path().to_path_buf());
            r.register(make_agent("alice"));
            r.register(make_agent("bob")); // .bak now contains alice
        }

        // Corrupt primary so the auto-fallback in `with_persistence` will load from .bak.
        fs::write(dir.path().join("agents.json"), b"corrupted").unwrap();

        let mut registry = AgentRegistry::with_persistence(dir.path().to_path_buf());
        // with_persistence already recovered alice from .bak. Now call recover_registry
        // to verify it works as an explicit operator-triggered path.
        let recovered = registry.recover_registry().unwrap();

        assert!(
            recovered,
            "recover_registry should return true when .bak exists"
        );
        let names: Vec<&str> = registry
            .list_all()
            .iter()
            .map(|a| a.name.as_str())
            .collect();
        assert!(
            names.contains(&"alice"),
            "Expected alice after recovery, got: {:?}",
            names
        );
    }

    #[test]
    fn test_drain_corruption_events_clears_field() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("agents.json"), b"bad json").unwrap();

        let mut registry = AgentRegistry::with_persistence(dir.path().to_path_buf());
        let first = registry.drain_corruption_events();
        let second = registry.drain_corruption_events();

        assert!(!first.is_empty(), "First drain should return events");
        assert!(
            second.is_empty(),
            "Second drain should be empty after first drain"
        );
    }

    #[test]
    fn test_save_is_deterministic() {
        let dir = TempDir::new().unwrap();
        let mut registry = AgentRegistry::with_persistence(dir.path().to_path_buf());
        registry.register(make_agent("alice"));
        registry.register(make_agent("bob"));
        registry.register(make_agent("carol"));

        let content1 = fs::read_to_string(dir.path().join("agents.json")).unwrap();
        // Force another save without changing data.
        registry.save_to_disk();
        let content2 = fs::read_to_string(dir.path().join("agents.json")).unwrap();

        assert_eq!(
            content1, content2,
            "Repeated saves of the same data should produce identical output"
        );
    }
}
