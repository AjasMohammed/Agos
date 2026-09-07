use agentos_nodes::{
    NodeContributor, NodeExecute, NodeManifest, NodeManifestBody, NodePort, NodeProperty,
    PropertyType,
};
use std::sync::Arc;

use agentos_mcp::McpSupervisor;

pub struct McpNodeContributor {
    supervisor: Arc<McpSupervisor>,
}

impl McpNodeContributor {
    pub fn new(supervisor: Arc<McpSupervisor>) -> Self {
        Self { supervisor }
    }
}

#[async_trait::async_trait]
impl NodeContributor for McpNodeContributor {
    fn category_prefix(&self) -> &str {
        "mcp"
    }

    fn category_display_name(&self) -> &str {
        "MCP Tools"
    }

    fn sort_order(&self) -> u32 {
        40
    }

    async fn contribute_nodes(&self) -> Vec<NodeManifest> {
        let statuses = self.supervisor.server_statuses().await;
        let mut nodes = Vec::new();
        for (server_name, _status, _tool_count, _stats, _note) in &statuses {
            let tools = self
                .supervisor
                .server_tools(server_name)
                .await
                .unwrap_or_default();
            for tool in tools {
                let tool_id = format!("mcp:{}:{}", server_name, tool.name);
                let description = tool.description.clone();
                let mut properties = vec![NodeProperty {
                    name: "input".into(),
                    display_name: "Input".into(),
                    property_type: PropertyType::Json,
                    description: description.clone(),
                    ..Default::default()
                }];
                if let Some(obj) = tool.input_schema.get("properties").and_then(|p| p.as_object())
                {
                    properties = obj
                        .iter()
                        .map(|(k, v)| NodeProperty {
                            name: k.clone(),
                            display_name: k.replace('_', " "),
                            property_type: match v.get("type").and_then(|t| t.as_str()) {
                                Some("number") | Some("integer") => PropertyType::Number,
                                Some("boolean") => PropertyType::Boolean,
                                Some("object") | Some("array") => PropertyType::Json,
                                _ => PropertyType::String,
                            },
                            description: v
                                .get("description")
                                .and_then(|d| d.as_str())
                                .unwrap_or("")
                                .to_string(),
                            ..Default::default()
                        })
                        .collect();
                }
                nodes.push(NodeManifest {
                    node: NodeManifestBody {
                        id: format!("mcp.{}.{}", server_name, tool.name),
                        display_name: format!("{}: {}", server_name, tool.name),
                        description,
                        category: "mcp".into(),
                        subcategory: Some(server_name.clone()),
                        icon: "plug".into(),
                        color: "#8b5cf6".into(),
                        risk_class: "readonly_external".into(),
                        inputs: vec![NodePort {
                            kind: "main".into(),
                            required: true,
                            ..Default::default()
                        }],
                        outputs: vec![NodePort {
                            kind: "main".into(),
                            ..Default::default()
                        }],
                        properties,
                        execute: NodeExecute::Tool {
                            tool_id,
                            parameter_mapping: Default::default(),
                        },
                        ..Default::default()
                    },
                });
            }
        }
        nodes
    }
}
