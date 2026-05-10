use agentos_nodes::{
    NodeContributor, NodeExecute, NodeManifest, NodeManifestBody, NodePort, NodeProperty,
    PropertyOption, PropertyType,
};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::agent_registry::AgentRegistry;

pub struct AgentNodeContributor {
    agents: Arc<RwLock<AgentRegistry>>,
}

impl AgentNodeContributor {
    pub fn new(agents: Arc<RwLock<AgentRegistry>>) -> Self {
        Self { agents }
    }
}

#[async_trait::async_trait]
impl NodeContributor for AgentNodeContributor {
    fn category_prefix(&self) -> &str {
        "agent"
    }

    fn category_display_name(&self) -> &str {
        "Agents"
    }

    fn sort_order(&self) -> u32 {
        10
    }

    async fn contribute_nodes(&self) -> Vec<NodeManifest> {
        let registry = self.agents.read().await;
        registry
            .list_all()
            .into_iter()
            .map(|a| {
                let agent_name = a.name.clone();
                NodeManifest {
                    node: NodeManifestBody {
                        id: format!("agent.{}", agent_name),
                        display_name: format!("Agent: {}", agent_name),
                        description: a.description.clone(),
                        category: "agents".into(),
                        icon: "cpu".into(),
                        color: "#6366f1".into(),
                        risk_class: "exec_capable".into(),
                        inputs: vec![NodePort {
                            kind: "main".into(),
                            required: true,
                            ..Default::default()
                        }],
                        outputs: vec![NodePort {
                            kind: "main".into(),
                            ..Default::default()
                        }],
                        properties: vec![
                            NodeProperty {
                                name: "task".into(),
                                display_name: "Task Prompt".into(),
                                property_type: PropertyType::Template,
                                required: true,
                                placeholder: Some("Analyze {{input}} and return a summary".into()),
                                ..Default::default()
                            },
                            NodeProperty {
                                name: "timeout_minutes".into(),
                                display_name: "Timeout (min)".into(),
                                property_type: PropertyType::Number,
                                default: serde_json::json!(5),
                                ..Default::default()
                            },
                            NodeProperty {
                                name: "thinking_level".into(),
                                display_name: "Thinking Level".into(),
                                property_type: PropertyType::Options,
                                options: vec![
                                    PropertyOption {
                                        value: serde_json::json!("off"),
                                        label: "Off".into(),
                                        ..Default::default()
                                    },
                                    PropertyOption {
                                        value: serde_json::json!("medium"),
                                        label: "Medium".into(),
                                        ..Default::default()
                                    },
                                    PropertyOption {
                                        value: serde_json::json!("high"),
                                        label: "High".into(),
                                        ..Default::default()
                                    },
                                ],
                                default: serde_json::json!("off"),
                                ..Default::default()
                            },
                        ],
                        execute: NodeExecute::Agent {
                            agent_property: "__fixed__".into(),
                            task_template: format!("__agent__:{}:{{{{task}}}}", agent_name),
                        },
                        ..Default::default()
                    },
                }
            })
            .collect()
    }
}
