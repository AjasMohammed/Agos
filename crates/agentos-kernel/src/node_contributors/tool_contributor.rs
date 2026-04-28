use agentos_nodes::{
    NodeContributor, NodeExecute, NodeManifest, NodeManifestBody, NodePort, NodeProperty,
    PropertyType,
};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::tool_registry::ToolRegistry;

pub struct ToolNodeContributor {
    tools: Arc<RwLock<ToolRegistry>>,
}

impl ToolNodeContributor {
    pub fn new(tools: Arc<RwLock<ToolRegistry>>) -> Self {
        Self { tools }
    }
}

#[async_trait::async_trait]
impl NodeContributor for ToolNodeContributor {
    fn category_prefix(&self) -> &str {
        "tool"
    }

    fn category_display_name(&self) -> &str {
        "Tools"
    }

    fn sort_order(&self) -> u32 {
        20
    }

    async fn contribute_nodes(&self) -> Vec<NodeManifest> {
        let registry = self.tools.read().await;
        registry
            .list_all()
            .into_iter()
            .map(|rt| {
                let tm = &rt.manifest;
                let info = &tm.manifest;
                let properties = json_schema_to_properties(tm.input_schema.as_ref());
                NodeManifest {
                    node: NodeManifestBody {
                        id: format!("tool.{}", info.name),
                        display_name: info.name.clone(),
                        description: info.description.clone(),
                        category: "tools".into(),
                        icon: "wrench".into(),
                        color: "#10b981".into(),
                        risk_class: format!("{:?}", tm.risk_class).to_lowercase(),
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
                            tool_id: info.name.clone(),
                            parameter_mapping: Default::default(),
                        },
                        ..Default::default()
                    },
                }
            })
            .collect()
    }
}

/// Best-effort conversion of a JSON Schema `properties` object into `NodeProperty` list.
fn json_schema_to_properties(schema: Option<&serde_json::Value>) -> Vec<NodeProperty> {
    let props = match schema
        .and_then(|s| s.get("properties"))
        .and_then(|p| p.as_object())
    {
        Some(p) => p,
        None => return vec![],
    };
    let required_fields: Vec<&str> = schema
        .and_then(|s| s.get("required"))
        .and_then(|r| r.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
        .unwrap_or_default();

    props
        .iter()
        .map(|(name, def)| {
            let description = def
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let prop_type = match def.get("type").and_then(|t| t.as_str()) {
                Some("number") | Some("integer") => PropertyType::Number,
                Some("boolean") => PropertyType::Boolean,
                Some("object") | Some("array") => PropertyType::Json,
                _ => PropertyType::String,
            };
            NodeProperty {
                name: name.clone(),
                display_name: to_display_name(name),
                property_type: prop_type,
                required: required_fields.contains(&name.as_str()),
                description,
                ..Default::default()
            }
        })
        .collect()
}

fn to_display_name(s: &str) -> String {
    s.replace('_', " ")
        .split_whitespace()
        .map(|word| {
            let mut c = word.chars();
            match c.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
