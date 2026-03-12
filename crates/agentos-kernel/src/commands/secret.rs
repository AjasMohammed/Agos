use crate::kernel::Kernel;
use agentos_bus::KernelResponse;
use agentos_types::*;

impl Kernel {
    pub(crate) async fn cmd_set_secret(
        &self,
        name: String,
        value: String,
        scope: SecretScope,
        scope_raw: Option<String>,
    ) -> KernelResponse {
        // Resolve raw scope string to proper SecretScope using kernel's agent registry
        let resolved_scope = if let Some(ref raw) = scope_raw {
            self.resolve_secret_scope(raw).await.unwrap_or(scope)
        } else {
            scope
        };
        match self
            .vault
            .set(&name, &value, SecretOwner::Kernel, resolved_scope)
        {
            Ok(_) => KernelResponse::Success { data: None },
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    pub(crate) async fn cmd_list_secrets(&self) -> KernelResponse {
        match self.vault.list() {
            Ok(list) => KernelResponse::SecretList(list),
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    pub(crate) async fn cmd_rotate_secret(
        &self,
        name: String,
        new_value: String,
    ) -> KernelResponse {
        match self.vault.rotate(&name, &new_value) {
            Ok(_) => KernelResponse::Success { data: None },
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    pub(crate) async fn cmd_revoke_secret(&self, name: String) -> KernelResponse {
        match self.vault.revoke(&name) {
            Ok(_) => KernelResponse::Success { data: None },
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    /// Resolve a raw scope string like "agent:name" or "tool:name" to a SecretScope,
    /// looking up agent/tool names in the kernel registries.
    async fn resolve_secret_scope(&self, raw: &str) -> Option<SecretScope> {
        match raw {
            "global" => Some(SecretScope::Global),
            s if s.starts_with("agent:") => {
                let agent_name = &s[6..];
                let registry = self.agent_registry.read().await;
                if let Some(profile) = registry.get_by_name(agent_name) {
                    Some(SecretScope::Agent(profile.id))
                } else {
                    tracing::warn!(
                        agent = agent_name,
                        "Agent not found for scope resolution, using Global"
                    );
                    Some(SecretScope::Global)
                }
            }
            s if s.starts_with("tool:") => {
                let tool_name = &s[5..];
                let registry = self.tool_registry.read().await;
                if let Some(tool) = registry.get_by_name(tool_name) {
                    Some(SecretScope::Tool(tool.id))
                } else {
                    tracing::warn!(
                        tool = tool_name,
                        "Tool not found for scope resolution, using Global"
                    );
                    Some(SecretScope::Global)
                }
            }
            _ => None,
        }
    }

    /// Emergency vault lockdown: revoke all proxy tokens and block new issuance.
    pub(crate) async fn cmd_vault_lockdown(&self) -> KernelResponse {
        self.vault.lockdown();
        KernelResponse::Success {
            data: Some(serde_json::json!({
                "action": "vault_lockdown",
                "message": "All proxy tokens revoked, new issuance blocked until restart",
            })),
        }
    }
}
