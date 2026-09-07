//! Kernel-side implementation of the `SkillInstaller` trait used by the
//! `skill-create` tool. Owns the `SkillRegistry` write lock + the shared
//! installed-skills snapshot, so the tool can stay in `agentos-tools`
//! without taking a hard dependency on `agentos-skills`.

use agentos_skills::SkillRegistry;
use agentos_tools::agent_manual::SharedInstalledSkills;
use agentos_tools::skill_create::SkillInstaller;
use async_trait::async_trait;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct KernelSkillInstaller {
    skill_registry: Arc<RwLock<SkillRegistry>>,
    installed_skills_snapshot: SharedInstalledSkills,
}

impl KernelSkillInstaller {
    pub fn new(
        skill_registry: Arc<RwLock<SkillRegistry>>,
        installed_skills_snapshot: SharedInstalledSkills,
    ) -> Self {
        Self {
            skill_registry,
            installed_skills_snapshot,
        }
    }
}

#[async_trait]
impl SkillInstaller for KernelSkillInstaller {
    async fn install_from_dir(
        &self,
        dir: &Path,
        overwrite: bool,
    ) -> Result<(String, String), String> {
        let (manifest, prompt) =
            agentos_skills::load_skill_from_dir(dir).map_err(|e| e.to_string())?;
        let name = manifest.skill.name.clone();
        let version = manifest.skill.version.clone();

        // Lock scope — release before refreshing the snapshot below so we
        // don't hold the write lock across an extra read lock. The duplicate
        // decision is authoritative here (under the write lock), closing the
        // TOCTOU window left by the tool-side snapshot pre-check.
        {
            let mut registry = self.skill_registry.write().await;
            if registry.get(&name).is_some() {
                if !overwrite {
                    // Fail-closed: a concurrent install or stale snapshot let a
                    // duplicate slip past the tool's pre-check. Reject rather
                    // than silently clobber the existing skill.
                    return Err(format!(
                        "skill '{name}' already installed (pass overwrite=true to replace)"
                    ));
                }
                // Overwrite requested — remove before re-inserting so
                // `install` doesn't error AlreadyInstalled.
                let _ = registry.remove(&name);
            }
            registry
                .install(manifest, prompt)
                .map_err(|e| e.to_string())?;
        }

        // Refresh the installed-skills snapshot so `agent-manual` and
        // `skill-prompt` immediately see the new skill.
        let snapshot = {
            let registry = self.skill_registry.read().await;
            crate::kernel::Kernel::build_skill_snapshot(&registry)
        };
        {
            let mut guard = self.installed_skills_snapshot.write().await;
            *guard = snapshot;
        }

        tracing::info!(
            skill = %name,
            version = %version,
            "Skill installed via skill-create tool"
        );
        Ok((name, version))
    }
}
