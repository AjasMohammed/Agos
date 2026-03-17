use crate::kernel::Kernel;
use agentos_bus::KernelResponse;
use agentos_types::*;
use agentos_vault::ZeroizingString;

impl Kernel {
    /// Emit a SecretsAccessAttempt event for audit trail on every vault operation.
    async fn emit_secrets_access(&self, action: &str, key: &str, allowed: bool) {
        self.emit_event(
            EventType::SecretsAccessAttempt,
            EventSource::SecretsVault,
            if allowed {
                EventSeverity::Info
            } else {
                EventSeverity::Warning
            },
            serde_json::json!({
                "action": action,
                "key": key,
                "allowed": allowed,
            }),
            0,
        )
        .await;
    }

    pub(crate) async fn cmd_set_secret(
        &self,
        name: String,
        value: String,
        scope: SecretScope,
        scope_raw: Option<String>,
    ) -> KernelResponse {
        // Wrap value in ZeroizingString immediately to zero it on scope exit.
        let value = ZeroizingString::new(value);

        // Resolve raw scope string to proper SecretScope using kernel's agent registry.
        // Return an error if the specified agent/tool is not found — silently widening
        // to Global would violate the caller's security intent.
        let resolved_scope = if let Some(ref raw) = scope_raw {
            match self.resolve_secret_scope(raw).await {
                Some(s) => s,
                None => {
                    return KernelResponse::Error {
                        message: format!(
                            "Scope '{}' could not be resolved: agent or tool not found",
                            raw
                        ),
                    }
                }
            }
        } else {
            scope
        };
        match self
            .vault
            .set(&name, value.as_str(), SecretOwner::Kernel, resolved_scope)
            .await
        {
            Ok(_) => {
                self.emit_secrets_access("set", &name, true).await;
                KernelResponse::Success { data: None }
            }
            Err(e) => {
                self.emit_secrets_access("set", &name, false).await;
                KernelResponse::Error {
                    message: e.to_string(),
                }
            }
        }
    }

    pub(crate) async fn cmd_list_secrets(&self) -> KernelResponse {
        self.emit_secrets_access("list", "*", true).await;
        match self.vault.list().await {
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
        let new_value = ZeroizingString::new(new_value);
        match self.vault.rotate(&name, new_value.as_str()).await {
            Ok(_) => {
                self.emit_secrets_access("rotate", &name, true).await;
                KernelResponse::Success { data: None }
            }
            Err(e) => {
                self.emit_secrets_access("rotate", &name, false).await;
                KernelResponse::Error {
                    message: e.to_string(),
                }
            }
        }
    }

    pub(crate) async fn cmd_revoke_secret(&self, name: String) -> KernelResponse {
        match self.vault.revoke(&name).await {
            Ok(_) => {
                self.emit_secrets_access("delete", &name, true).await;
                KernelResponse::Success { data: None }
            }
            Err(e) => {
                self.emit_secrets_access("delete", &name, false).await;
                KernelResponse::Error {
                    message: e.to_string(),
                }
            }
        }
    }

    /// Resolve a raw scope string like "agent:name" or "tool:name" to a SecretScope,
    /// looking up agent/tool names in the kernel registries.
    async fn resolve_secret_scope(&self, raw: &str) -> Option<SecretScope> {
        match raw {
            "kernel" => Some(SecretScope::Kernel),
            "global" => Some(SecretScope::Global),
            s if s.starts_with("agent:") => {
                let agent_name = &s[6..];
                if agent_name.is_empty() {
                    return None;
                }
                let registry = self.agent_registry.read().await;
                registry
                    .get_by_name(agent_name)
                    .map(|profile| SecretScope::Agent(profile.id))
            }
            s if s.starts_with("tool:") => {
                let tool_name = &s[5..];
                if tool_name.is_empty() {
                    return None;
                }
                let registry = self.tool_registry.read().await;
                registry
                    .get_by_name(tool_name)
                    .map(|tool| SecretScope::Tool(tool.id))
            }
            _ => None,
        }
    }

    /// Emergency vault lockdown: revoke all proxy tokens and block new issuance.
    pub(crate) async fn cmd_vault_lockdown(&self) -> KernelResponse {
        self.vault.lockdown().await;
        KernelResponse::Success {
            data: Some(serde_json::json!({
                "action": "vault_lockdown",
                "message": "All proxy tokens revoked, new issuance blocked until restart",
            })),
        }
    }
}
