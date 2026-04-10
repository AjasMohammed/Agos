use crate::kernel::Kernel;
use agentos_bus::KernelResponse;
use agentos_types::*;

impl Kernel {
    pub(crate) async fn cmd_create_webhook_endpoint(
        &self,
        agent_name: &str,
        provider_str: &str,
        debounce_seconds: u64,
    ) -> KernelResponse {
        // Resolve agent
        let agent_id = {
            let registry = self.agent_registry.read().await;
            match registry.get_by_name(agent_name) {
                Some(agent) => agent.id,
                None => {
                    return KernelResponse::Error {
                        message: format!("Agent not found: {agent_name}"),
                    };
                }
            }
        };

        // Parse provider — try JSON first (e.g. "\"github\""), fall back to quoted string
        let provider: WebhookProvider = serde_json::from_str(&format!("\"{provider_str}\""))
            .unwrap_or(WebhookProvider::Generic);

        match self
            .webhook_registry
            .create_endpoint(agent_id, provider, debounce_seconds)
            .await
        {
            Ok((meta, secret)) => KernelResponse::Success {
                data: Some(serde_json::json!({
                    "endpoint_id": meta.id.to_string(),
                    "url": format!("/api/v1/webhooks/incoming/{}", meta.id),
                    "secret": secret,
                    "provider": meta.provider,
                    "debounce_seconds": meta.debounce_seconds,
                })),
            },
            Err(e) => KernelResponse::Error {
                message: format!("Failed to create webhook endpoint: {e}"),
            },
        }
    }

    pub(crate) async fn cmd_list_webhook_endpoints(
        &self,
        agent_name: Option<&str>,
    ) -> KernelResponse {
        let agent_id = match agent_name {
            Some(name) => {
                let registry = self.agent_registry.read().await;
                match registry.get_by_name(name) {
                    Some(agent) => Some(agent.id),
                    None => {
                        return KernelResponse::Error {
                            message: format!("Agent not found: {name}"),
                        };
                    }
                }
            }
            None => None,
        };

        let endpoints = self
            .webhook_registry
            .list_endpoints(agent_id.as_ref())
            .await;

        KernelResponse::WebhookEndpointList { endpoints }
    }

    pub(crate) async fn cmd_delete_webhook_endpoint(
        &self,
        endpoint_id_str: &str,
    ) -> KernelResponse {
        let endpoint_id: WebhookEndpointID = match endpoint_id_str.parse() {
            Ok(id) => id,
            Err(_) => {
                return KernelResponse::Error {
                    message: format!("Invalid webhook endpoint ID: {endpoint_id_str}"),
                };
            }
        };

        match self.webhook_registry.delete_endpoint(&endpoint_id).await {
            Ok(()) => KernelResponse::Success {
                data: Some(serde_json::json!({
                    "deleted": endpoint_id.to_string(),
                })),
            },
            Err(e) => KernelResponse::Error {
                message: format!("Failed to delete webhook endpoint: {e}"),
            },
        }
    }
}
