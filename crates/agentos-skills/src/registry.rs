use agentos_types::skill::SkillManifest;
use std::collections::HashMap;
use std::path::Path;

/// Errors that can occur during skill registry operations.
#[derive(Debug, thiserror::Error)]
pub enum SkillRegistryError {
    #[error("Skill '{0}' already installed")]
    AlreadyInstalled(String),

    #[error("Skill '{0}' not found")]
    NotFound(String),

    #[error("Failed to load skill from '{path}': {source}")]
    LoadError {
        path: String,
        source: Box<crate::manifest::SkillLoadError>,
    },

    #[error("IO error scanning '{path}': {source}")]
    IoError {
        path: String,
        source: Box<std::io::Error>,
    },
}

/// A skill that has been loaded into the registry.
#[derive(Debug, Clone)]
pub struct InstalledSkill {
    pub manifest: SkillManifest,
    pub system_prompt: String,
}

/// Registry of installed skills.
///
/// Skills are loaded from directories containing `SKILL.toml` + prompt files.
/// The registry provides install, remove, and lookup operations.
pub struct SkillRegistry {
    skills: HashMap<String, InstalledSkill>,
}

impl SkillRegistry {
    /// Create an empty skill registry.
    pub fn new() -> Self {
        Self {
            skills: HashMap::new(),
        }
    }

    /// Scan a base directory for subdirectories containing `SKILL.toml`,
    /// load each valid skill, and return the number of skills loaded.
    ///
    /// Subdirectories that fail to load are logged and skipped.
    pub fn load_from_dir(&mut self, base: &Path) -> Result<usize, SkillRegistryError> {
        if !base.exists() {
            tracing::debug!(path = %base.display(), "Skills directory does not exist, skipping");
            return Ok(0);
        }

        let entries = std::fs::read_dir(base).map_err(|e| SkillRegistryError::IoError {
            path: base.display().to_string(),
            source: Box::new(e),
        })?;

        let mut loaded = 0;
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to read directory entry in skills dir");
                    continue;
                }
            };

            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            // Skip directories that don't contain SKILL.toml
            if !path.join("SKILL.toml").exists() {
                continue;
            }

            match crate::manifest::load_skill_from_dir(&path) {
                Ok((manifest, prompt)) => {
                    let name = manifest.skill.name.clone();
                    if self.skills.contains_key(&name) {
                        tracing::warn!(
                            skill = %name,
                            path = %path.display(),
                            "Skipping duplicate skill (already loaded)"
                        );
                        continue;
                    }
                    tracing::info!(
                        skill = %name,
                        version = %manifest.skill.version,
                        path = %path.display(),
                        "Loaded skill"
                    );
                    self.skills.insert(
                        name,
                        InstalledSkill {
                            manifest,
                            system_prompt: prompt,
                        },
                    );
                    loaded += 1;
                }
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "Failed to load skill, skipping"
                    );
                }
            }
        }

        Ok(loaded)
    }

    /// Install a skill into the registry.
    pub fn install(
        &mut self,
        manifest: SkillManifest,
        system_prompt: String,
    ) -> Result<(), SkillRegistryError> {
        let name = manifest.skill.name.clone();
        if self.skills.contains_key(&name) {
            return Err(SkillRegistryError::AlreadyInstalled(name));
        }
        self.skills.insert(
            name,
            InstalledSkill {
                manifest,
                system_prompt,
            },
        );
        Ok(())
    }

    /// Remove a skill from the registry by name.
    pub fn remove(&mut self, name: &str) -> Result<(), SkillRegistryError> {
        if self.skills.remove(name).is_none() {
            return Err(SkillRegistryError::NotFound(name.to_string()));
        }
        Ok(())
    }

    /// Look up a skill by name.
    pub fn get(&self, name: &str) -> Option<&InstalledSkill> {
        self.skills.get(name)
    }

    /// List all installed skill manifests.
    pub fn list(&self) -> Vec<&SkillManifest> {
        let mut manifests: Vec<_> = self.skills.values().map(|s| &s.manifest).collect();
        manifests.sort_by(|a, b| a.skill.name.cmp(&b.skill.name));
        manifests
    }

    /// Return the number of installed skills.
    pub fn len(&self) -> usize {
        self.skills.len()
    }

    /// Check if the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }
}

impl Default for SkillRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_skill_dir(base: &Path, name: &str, version: &str) {
        let dir = base.join(name);
        fs::create_dir_all(&dir).unwrap();
        let toml = format!(
            r#"
[skill]
name = "{name}"
version = "{version}"
description = "Test skill {name}"
author = "test"

[agent]
system_prompt_file = "prompt.md"
"#
        );
        fs::write(dir.join("SKILL.toml"), toml).unwrap();
        fs::write(
            dir.join("prompt.md"),
            format!("You are the {} skill.", name),
        )
        .unwrap();
    }

    #[test]
    fn test_registry_new_is_empty() {
        let registry = SkillRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
        assert!(registry.list().is_empty());
    }

    #[test]
    fn test_load_from_dir_multiple_skills() {
        let tmp = TempDir::new().unwrap();
        create_skill_dir(tmp.path(), "alpha-skill", "1.0.0");
        create_skill_dir(tmp.path(), "beta-skill", "2.0.0");

        let mut registry = SkillRegistry::new();
        let loaded = registry.load_from_dir(tmp.path()).unwrap();
        assert_eq!(loaded, 2);
        assert_eq!(registry.len(), 2);

        let alpha = registry.get("alpha-skill").unwrap();
        assert_eq!(alpha.manifest.skill.version, "1.0.0");
        assert_eq!(alpha.system_prompt, "You are the alpha-skill skill.");

        let beta = registry.get("beta-skill").unwrap();
        assert_eq!(beta.manifest.skill.version, "2.0.0");
    }

    #[test]
    fn test_load_from_dir_nonexistent() {
        let tmp = TempDir::new().unwrap();
        let nonexistent = tmp.path().join("does-not-exist");
        let mut registry = SkillRegistry::new();
        let loaded = registry.load_from_dir(&nonexistent).unwrap();
        assert_eq!(loaded, 0);
    }

    #[test]
    fn test_load_from_dir_skips_non_directories() {
        let tmp = TempDir::new().unwrap();
        create_skill_dir(tmp.path(), "valid-skill", "1.0.0");
        // Create a regular file (not a directory) — should be skipped
        fs::write(tmp.path().join("not-a-dir.txt"), "just a file").unwrap();

        let mut registry = SkillRegistry::new();
        let loaded = registry.load_from_dir(tmp.path()).unwrap();
        assert_eq!(loaded, 1);
    }

    #[test]
    fn test_load_from_dir_skips_invalid_skills() {
        let tmp = TempDir::new().unwrap();
        create_skill_dir(tmp.path(), "valid-skill", "1.0.0");

        // Create an invalid skill directory (bad TOML)
        let bad_dir = tmp.path().join("bad-skill");
        fs::create_dir_all(&bad_dir).unwrap();
        fs::write(bad_dir.join("SKILL.toml"), "not valid toml {{{{").unwrap();

        let mut registry = SkillRegistry::new();
        let loaded = registry.load_from_dir(tmp.path()).unwrap();
        assert_eq!(loaded, 1);
        assert!(registry.get("valid-skill").is_some());
    }

    #[test]
    fn test_install_and_remove() {
        let mut registry = SkillRegistry::new();

        let manifest = toml::from_str::<agentos_types::skill::SkillManifest>(
            r#"
[skill]
name = "my-skill"
version = "1.0.0"
description = "test"
author = "test"

[agent]
system_prompt_file = "prompt.md"
"#,
        )
        .unwrap();

        registry
            .install(manifest.clone(), "prompt content".to_string())
            .unwrap();
        assert_eq!(registry.len(), 1);
        assert!(registry.get("my-skill").is_some());

        // Duplicate install should fail
        let err = registry
            .install(manifest, "prompt content".to_string())
            .unwrap_err();
        assert!(matches!(err, SkillRegistryError::AlreadyInstalled(_)));

        // Remove
        registry.remove("my-skill").unwrap();
        assert!(registry.is_empty());

        // Remove non-existent should fail
        let err = registry.remove("nonexistent").unwrap_err();
        assert!(matches!(err, SkillRegistryError::NotFound(_)));
    }

    #[test]
    fn test_list_sorted() {
        let tmp = TempDir::new().unwrap();
        create_skill_dir(tmp.path(), "charlie", "1.0.0");
        create_skill_dir(tmp.path(), "alpha", "1.0.0");
        create_skill_dir(tmp.path(), "bravo", "1.0.0");

        let mut registry = SkillRegistry::new();
        registry.load_from_dir(tmp.path()).unwrap();

        let names: Vec<_> = registry
            .list()
            .iter()
            .map(|m| m.skill.name.as_str())
            .collect();
        assert_eq!(names, vec!["alpha", "bravo", "charlie"]);
    }
}
