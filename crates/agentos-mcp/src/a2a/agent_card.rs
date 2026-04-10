/// A2A Agent Card — the discoverable identity document of an AgentOS agent.
///
/// Served at `GET /.well-known/agent.json`. External agents fetch this URL to
/// discover what capabilities the agent offers and how to authenticate.
use serde::{Deserialize, Serialize};

/// The complete agent identity and capability advertisement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCard {
    /// Human-readable name of this agent.
    pub name: String,

    /// Description of what this agent does.
    pub description: String,

    /// The base URL where this agent's A2A endpoints are reachable.
    /// E.g. "http://localhost:3001"
    pub url: String,

    /// Protocol version this agent speaks.
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,

    /// Provider identifier — always "agentos" for AgentOS agents.
    pub provider: String,

    /// Semantic version of this agent's implementation.
    pub version: String,

    /// Capabilities this agent can perform on behalf of external agents.
    pub capabilities: Vec<AgentCapability>,

    /// Authentication requirements for calling this agent.
    pub authentication: AuthRequirement,
}

/// A single capability this agent exposes to other agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCapability {
    /// Unique capability name (used in task delegations).
    pub name: String,

    /// Human-readable description.
    pub description: String,

    /// JSON Schema for the task input.
    #[serde(rename = "inputSchema")]
    pub input_schema: serde_json::Value,

    /// JSON Schema for the task output.
    #[serde(rename = "outputSchema")]
    pub output_schema: serde_json::Value,
}

/// Authentication options an external agent must satisfy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "scheme", rename_all = "snake_case")]
pub enum AuthRequirement {
    /// No authentication required (only for local/trusted deployments).
    None,
    /// HTTP Bearer token in the `Authorization` header.
    Bearer {
        /// Human-readable description of how to obtain a token.
        description: String,
    },
    /// AgentOS CapabilityToken (HMAC-SHA256 signed).
    CapabilityToken {
        /// Token scope required.
        scope: String,
    },
}

impl AgentCard {
    /// Build a default AgentCard for an AgentOS MCP server.
    /// Capabilities are derived from the list of available MCP tools.
    pub fn from_tools(
        name: &str,
        description: &str,
        base_url: &str,
        tool_names: &[String],
        auth: AuthRequirement,
    ) -> Self {
        let capabilities = tool_names
            .iter()
            .map(|t| AgentCapability {
                name: t.clone(),
                description: format!("Invoke AgentOS tool: {}", t),
                input_schema: serde_json::json!({"type": "object"}),
                output_schema: serde_json::json!({"type": "object"}),
            })
            .collect();

        Self {
            name: name.to_string(),
            description: description.to_string(),
            url: base_url.to_string(),
            protocol_version: "1.0".to_string(),
            provider: "agentos".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            capabilities,
            authentication: auth,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_card_serialization_roundtrip() {
        let card = AgentCard::from_tools(
            "test-agent",
            "A test agent",
            "http://localhost:3001",
            &["file-reader".to_string(), "shell-exec".to_string()],
            AuthRequirement::Bearer {
                description: "Use your API key".to_string(),
            },
        );
        let json = serde_json::to_string(&card).unwrap();
        let parsed: AgentCard = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "test-agent");
        assert_eq!(parsed.capabilities.len(), 2);
        assert_eq!(parsed.provider, "agentos");
    }

    #[test]
    fn auth_none_serializes_correctly() {
        let auth = AuthRequirement::None;
        let json = serde_json::to_value(&auth).unwrap();
        assert_eq!(json["scheme"], "none");
    }

    #[test]
    fn auth_bearer_serializes_correctly() {
        let auth = AuthRequirement::Bearer {
            description: "Use API key".to_string(),
        };
        let json = serde_json::to_value(&auth).unwrap();
        assert_eq!(json["scheme"], "bearer");
        assert!(json["description"].as_str().is_some());
    }
}
