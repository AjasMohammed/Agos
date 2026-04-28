use agentos_nodes::{
    NodeContributor, NodeExecute, NodeManifest, NodeManifestBody, NodePort, NodeProperty,
    PropertyType,
};
use std::collections::BTreeMap;

pub struct MemoryNodeContributor;

impl MemoryNodeContributor {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MemoryNodeContributor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl NodeContributor for MemoryNodeContributor {
    fn category_prefix(&self) -> &str {
        "memory"
    }

    fn category_display_name(&self) -> &str {
        "Memory"
    }

    fn sort_order(&self) -> u32 {
        50
    }

    async fn contribute_nodes(&self) -> Vec<NodeManifest> {
        vec![
            memory_node(
                "memory.episodic-write",
                "Episodic Write",
                "Write an event to episodic memory",
                "episodic-write",
                vec![("content", "Content", PropertyType::Template, true)],
            ),
            memory_node(
                "memory.semantic-search",
                "Semantic Search",
                "Search semantic memory for relevant facts",
                "semantic-search",
                vec![
                    ("query", "Query", PropertyType::String, true),
                    ("limit", "Max Results", PropertyType::Number, false),
                ],
            ),
            memory_node(
                "memory.semantic-write",
                "Semantic Write",
                "Write a fact to semantic memory",
                "semantic-write",
                vec![("content", "Fact", PropertyType::Template, true)],
            ),
            memory_node(
                "memory.recall",
                "Memory Recall",
                "Retrieve relevant memories for a query across all tiers",
                "memory-recall",
                vec![
                    ("query", "Query", PropertyType::String, true),
                    ("tiers", "Tiers", PropertyType::String, false),
                ],
            ),
        ]
    }
}

fn memory_node(
    id: &str,
    display_name: &str,
    description: &str,
    tool_id: &str,
    props: Vec<(&str, &str, PropertyType, bool)>,
) -> NodeManifest {
    NodeManifest {
        node: NodeManifestBody {
            id: id.to_string(),
            display_name: display_name.to_string(),
            description: description.to_string(),
            category: "memory".into(),
            icon: "database".into(),
            color: "#06b6d4".into(),
            risk_class: "write_scoped".into(),
            inputs: vec![NodePort {
                kind: "main".into(),
                required: true,
                ..Default::default()
            }],
            outputs: vec![NodePort {
                kind: "main".into(),
                ..Default::default()
            }],
            properties: props
                .into_iter()
                .map(|(name, display, ptype, required)| NodeProperty {
                    name: name.to_string(),
                    display_name: display.to_string(),
                    property_type: ptype,
                    required,
                    ..Default::default()
                })
                .collect(),
            execute: NodeExecute::Tool {
                tool_id: tool_id.to_string(),
                parameter_mapping: BTreeMap::new(),
            },
            ..Default::default()
        },
    }
}
