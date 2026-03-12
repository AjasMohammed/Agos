use crate::kernel::Kernel;
use agentos_bus::KernelResponse;

impl Kernel {
    /// Show an agent's Ed25519 public key identity.
    pub(crate) async fn cmd_identity_show(&self, agent_name: String) -> KernelResponse {
        let registry = self.agent_registry.read().await;
        let profile = match registry.get_by_name(&agent_name) {
            Some(p) => p,
            None => {
                return KernelResponse::Error {
                    message: format!("Agent '{}' not found", agent_name),
                }
            }
        };
        let agent_id = profile.id;
        let public_key = profile.public_key_hex.clone();
        drop(registry);

        let has_signing_key = self
            .identity_manager
            .load_signing_key(&agent_id)
            .ok()
            .flatten()
            .is_some();

        KernelResponse::Success {
            data: Some(serde_json::json!({
                "agent_id": agent_id.to_string(),
                "agent_name": agent_name,
                "public_key": public_key,
                "has_signing_key": has_signing_key,
            })),
        }
    }

    /// Revoke an agent's Ed25519 identity and optionally revoke its capability permissions.
    pub(crate) async fn cmd_identity_revoke(&self, agent_name: String) -> KernelResponse {
        let registry = self.agent_registry.read().await;
        let profile = match registry.get_by_name(&agent_name) {
            Some(p) => p,
            None => {
                return KernelResponse::Error {
                    message: format!("Agent '{}' not found", agent_name),
                }
            }
        };
        let agent_id = profile.id;
        drop(registry);

        // Revoke Ed25519 identity
        if let Err(e) = self.identity_manager.revoke_identity(&agent_id) {
            return KernelResponse::Error {
                message: format!("Failed to revoke identity: {}", e),
            };
        }

        // Also revoke capability permissions
        self.capability_engine.revoke_agent(&agent_id);

        KernelResponse::Success {
            data: Some(serde_json::json!({
                "agent_id": agent_id.to_string(),
                "agent_name": agent_name,
                "identity_revoked": true,
                "permissions_revoked": true,
            })),
        }
    }
}
