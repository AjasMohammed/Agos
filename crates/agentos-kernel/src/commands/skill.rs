use crate::Kernel;
use agentos_bus::message::KernelResponse;
use serde_json::json;

impl Kernel {
    pub(crate) async fn cmd_skill_install(&self, path: String) -> KernelResponse {
        let skill_path = std::path::Path::new(&path);
        if !skill_path.exists() {
            return KernelResponse::Error {
                message: format!("Skill directory not found: {}", path),
            };
        }

        match agentos_skills::load_skill_from_dir(skill_path) {
            Ok((manifest, prompt)) => {
                let name = manifest.skill.name.clone();
                let version = manifest.skill.version.clone();
                let install_result = {
                    let mut registry = self.skill_registry.write().await;
                    registry.install(manifest, prompt)
                };
                match install_result {
                    Ok(()) => {
                        tracing::info!(skill = %name, version = %version, "Skill installed");
                        // Keep the agent-manual's live skills snapshot in sync
                        // with the registry. Same pattern channel register/
                        // deregister uses — see `refresh_connected_channels_snapshot`.
                        self.refresh_installed_skills_snapshot().await;
                        KernelResponse::Success {
                            data: Some(json!({
                                "skill_name": name,
                                "version": version,
                            })),
                        }
                    }
                    Err(e) => KernelResponse::Error {
                        message: e.to_string(),
                    },
                }
            }
            Err(e) => KernelResponse::Error {
                message: format!("Failed to load skill from '{}': {}", path, e),
            },
        }
    }

    pub(crate) async fn cmd_skill_remove(&self, name: String) -> KernelResponse {
        let remove_result = {
            let mut registry = self.skill_registry.write().await;
            registry.remove(&name)
        };
        match remove_result {
            Ok(()) => {
                tracing::info!(skill = %name, "Skill removed");
                self.refresh_installed_skills_snapshot().await;
                KernelResponse::Success { data: None }
            }
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    pub(crate) async fn cmd_skill_list(&self) -> KernelResponse {
        let registry = self.skill_registry.read().await;
        let skills: Vec<serde_json::Value> = registry
            .list()
            .iter()
            .map(|m| {
                json!({
                    "name": m.skill.name,
                    "version": m.skill.version,
                    "description": m.skill.description,
                    "author": m.skill.author,
                    "trust_tier": m.skill.trust_tier,
                    "triggers": {
                        "schedule": m.triggers.schedule,
                        "events": m.triggers.events,
                    },
                    "tools": {
                        "required": m.tools.required,
                        "optional": m.tools.optional,
                    },
                    "budget": {
                        "max_cost_per_run": m.budget.max_cost_per_run,
                        "max_tokens_per_run": m.budget.max_tokens_per_run,
                    },
                })
            })
            .collect();
        KernelResponse::SkillList(skills)
    }

    pub(crate) async fn cmd_skill_run(
        &self,
        name: String,
        _input: Option<String>,
    ) -> KernelResponse {
        let registry = self.skill_registry.read().await;
        match registry.get(&name) {
            Some(_skill) => {
                // Phase 2: actual skill execution (spawn agent with skill prompt + tools + budget).
                // For now, return a placeholder indicating the skill was found.
                KernelResponse::Error {
                    message: format!(
                        "Skill '{}' found but execution is not yet implemented (Phase 2)",
                        name
                    ),
                }
            }
            None => KernelResponse::Error {
                message: format!("Skill '{}' not found", name),
            },
        }
    }

    pub(crate) async fn cmd_skill_status(&self, name: String) -> KernelResponse {
        let registry = self.skill_registry.read().await;
        match registry.get(&name) {
            Some(skill) => {
                let info = json!({
                    "name": skill.manifest.skill.name,
                    "version": skill.manifest.skill.version,
                    "description": skill.manifest.skill.description,
                    "author": skill.manifest.skill.author,
                    "trust_tier": skill.manifest.skill.trust_tier,
                    "license": skill.manifest.skill.license,
                    "triggers": {
                        "schedule": skill.manifest.triggers.schedule,
                        "events": skill.manifest.triggers.events,
                    },
                    "agent": {
                        "system_prompt_file": skill.manifest.agent.system_prompt_file,
                        "roles": skill.manifest.agent.roles,
                        "default_provider": skill.manifest.agent.default_provider,
                        "default_model": skill.manifest.agent.default_model,
                    },
                    "tools": {
                        "required": skill.manifest.tools.required,
                        "optional": skill.manifest.tools.optional,
                    },
                    "permissions": {
                        "required": skill.manifest.permissions.required,
                    },
                    "budget": {
                        "max_cost_per_run": skill.manifest.budget.max_cost_per_run,
                        "max_tokens_per_run": skill.manifest.budget.max_tokens_per_run,
                    },
                    "system_prompt_length": skill.system_prompt.len(),
                    "status": "installed",
                });
                KernelResponse::SkillStatusInfo(info)
            }
            None => KernelResponse::Error {
                message: format!("Skill '{}' not found", name),
            },
        }
    }
}
