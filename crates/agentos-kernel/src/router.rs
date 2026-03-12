use agentos_types::*;
use regex::Regex;
use serde::{Deserialize, Serialize};

/// Strategy to use when routing a task to an LLM agent without a preferred agent.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RoutingStrategy {
    /// Pick the most capable model (e.g. highest context window).
    #[default]
    CapabilityFirst,
    /// Pick the cheapest model (not yet measurable easily, defaults to capability).
    CostFirst,
    /// Pick the fastest model.
    LatencyFirst,
    /// Distribute round-robin.
    RoundRobin,
}

/// A parsed matching rule based on the prompt description
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingRule {
    /// Optional regex pattern to match against the prompt text.
    pub task_pattern: Option<String>,
    /// The name of the preferred agent for this match.
    pub preferred_agent: String,
    /// A fallback agent name if the preferred agent is offline.
    pub fallback_agent: Option<String>,
}

pub struct TaskRouter {
    pub strategy: RoutingStrategy,
    pub rules: Vec<RoutingRule>,
    round_robin_index: std::sync::atomic::AtomicUsize,
}

impl TaskRouter {
    pub fn new(strategy: RoutingStrategy, rules: Vec<RoutingRule>) -> Self {
        Self {
            strategy,
            rules,
            round_robin_index: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Select the best agent for a task.
    /// Returns the active `AgentID` of the selected agent.
    pub async fn route(
        &self,
        prompt: &str,
        agents: &[AgentProfile],
    ) -> Result<AgentID, AgentOSError> {
        if agents.is_empty() {
            return Err(AgentOSError::KernelError {
                reason: "No agents available for routing".to_string(),
            });
        }

        let online_agents: Vec<&AgentProfile> = agents
            .iter()
            .filter(|a| a.status == AgentStatus::Online || a.status == AgentStatus::Idle)
            .collect();

        if online_agents.is_empty() {
            return Err(AgentOSError::KernelError {
                reason: "No online agents available for routing".to_string(),
            });
        }

        // 1. Evaluate Routing Rules
        for rule in &self.rules {
            let matches = if let Some(pattern) = &rule.task_pattern {
                if let Ok(re) = Regex::new(pattern) {
                    re.is_match(prompt)
                } else {
                    false // Ignore invalid regex rules
                }
            } else {
                true // Rule with no pattern matches anything
            };

            if matches {
                // Try preferred agent
                if let Some(agent) = online_agents
                    .iter()
                    .find(|a| a.name == rule.preferred_agent)
                {
                    return Ok(agent.id);
                }

                // Try fallback agent
                if let Some(fallback) = &rule.fallback_agent {
                    if let Some(agent) = online_agents.iter().find(|a| &a.name == fallback) {
                        return Ok(agent.id);
                    }
                }
            }
        }

        // 2. Apply Strategy
        match self.strategy {
            RoutingStrategy::CapabilityFirst => {
                // We'll approximate by picking a cloud provider over Ollama/Custom if available,
                // or just pick the first online for now since detailed capabilities aren't in AgentProfile.
                // A better capability check would consult actual `sys_capabilities` from context.
                // For MVP: Prefer OpenAI/Anthropic/Gemini over Ollama/Custom.
                let mut sorted = online_agents.clone();
                sorted.sort_by_key(|a| match a.provider {
                    LLMProvider::Anthropic => 4,
                    LLMProvider::OpenAI => 3,
                    LLMProvider::Gemini => 2,
                    LLMProvider::Custom(_) => 1,
                    LLMProvider::Ollama => 0,
                });

                Ok(sorted.last().unwrap().id)
            }
            RoutingStrategy::CostFirst | RoutingStrategy::LatencyFirst => {
                // Approximate CostFirst/LatencyFirst:
                // Local/Custom is cheapest & fastest (typically).
                let mut sorted = online_agents.clone();
                sorted.sort_by_key(|a| match a.provider {
                    LLMProvider::Ollama => 4,
                    LLMProvider::Custom(_) => 3,
                    LLMProvider::Gemini => 2,
                    LLMProvider::OpenAI => 1,
                    LLMProvider::Anthropic => 0,
                });
                Ok(sorted.last().unwrap().id)
            }
            RoutingStrategy::RoundRobin => {
                let idx = self
                    .round_robin_index
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    % online_agents.len();
                Ok(online_agents[idx].id)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_agent(name: &str, provider: LLMProvider) -> AgentProfile {
        AgentProfile {
            id: AgentID::new(),
            name: name.to_string(),
            provider,
            model: "model".into(),
            status: AgentStatus::Online,
            permissions: agentos_types::PermissionSet::new(),
            roles: vec![],
            current_task: None,
            description: "".into(),
            created_at: chrono::Utc::now(),
            last_active: chrono::Utc::now(),
            public_key_hex: None,
        }
    }

    #[tokio::test]
    async fn test_routing_capability_first() {
        let router = TaskRouter::new(RoutingStrategy::CapabilityFirst, vec![]);
        let a1 = dummy_agent("local", LLMProvider::Ollama);
        let a2 = dummy_agent("cloud", LLMProvider::OpenAI);

        let id = router
            .route("hello", &[a1.clone(), a2.clone()])
            .await
            .unwrap();
        assert_eq!(id, a2.id); // OpenAI is ranked higher than Ollama
    }

    #[tokio::test]
    async fn test_routing_round_robin() {
        let router = TaskRouter::new(RoutingStrategy::RoundRobin, vec![]);
        let a1 = dummy_agent("a", LLMProvider::Ollama);
        let a2 = dummy_agent("b", LLMProvider::Ollama);

        let agents = vec![a1.clone(), a2.clone()];

        let id1 = router.route("t1", &agents).await.unwrap();
        let id2 = router.route("t2", &agents).await.unwrap();
        let id3 = router.route("t3", &agents).await.unwrap();

        assert_eq!(id1, a1.id);
        assert_eq!(id2, a2.id);
        assert_eq!(id3, a1.id); // Should cycle
    }

    #[tokio::test]
    async fn test_routing_rules_preferred() {
        let rules = vec![RoutingRule {
            task_pattern: Some(".*code.*".into()),
            preferred_agent: "coder".into(),
            fallback_agent: None,
        }];
        let router = TaskRouter::new(RoutingStrategy::CostFirst, rules);
        let a1 = dummy_agent("local", LLMProvider::Ollama);
        let a2 = dummy_agent("coder", LLMProvider::Anthropic);

        let id = router
            .route("write me some rust code", &[a1.clone(), a2.clone()])
            .await
            .unwrap();
        assert_eq!(id, a2.id); // Even though CostFirst prefers local, rule matches first
    }

    #[tokio::test]
    async fn test_routing_rules_fallback() {
        let rules = vec![RoutingRule {
            task_pattern: Some(".*code.*".into()),
            preferred_agent: "coder".into(), // Offline
            fallback_agent: Some("local".into()),
        }];
        let router = TaskRouter::new(RoutingStrategy::CostFirst, rules);

        // Coder is offline
        let mut a_coder = dummy_agent("coder", LLMProvider::Anthropic);
        a_coder.status = AgentStatus::Offline;

        let a_local = dummy_agent("local", LLMProvider::Ollama);

        let id = router
            .route("write code", &[a_local.clone(), a_coder.clone()])
            .await
            .unwrap();
        assert_eq!(id, a_local.id); // Falls back to local agent
    }
}
