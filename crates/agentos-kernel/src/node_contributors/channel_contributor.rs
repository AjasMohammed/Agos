use agentos_nodes::{
    NodeContributor, NodeExecute, NodeManifest, NodeManifestBody, NodePort, NodeProperty,
    PropertyType,
};
use std::collections::BTreeMap;
use std::sync::Arc;

use agentos_channels::manager::ChannelManager;

pub struct ChannelNodeContributor {
    channel_manager: Arc<ChannelManager>,
}

impl ChannelNodeContributor {
    pub fn new(channel_manager: Arc<ChannelManager>) -> Self {
        Self { channel_manager }
    }
}

#[async_trait::async_trait]
impl NodeContributor for ChannelNodeContributor {
    fn category_prefix(&self) -> &str {
        "channel"
    }

    fn category_display_name(&self) -> &str {
        "Channels"
    }

    fn sort_order(&self) -> u32 {
        30
    }

    async fn contribute_nodes(&self) -> Vec<NodeManifest> {
        self.channel_manager
            .list_channel_entries()
            .await
            .into_iter()
            .map(|(instance_id, adapter_name)| {
                let display = format!("Send via {}", adapter_name);
                let mut param_map = BTreeMap::new();
                param_map.insert(
                    "channel_id".to_string(),
                    format!("__fixed__:{}", instance_id),
                );
                NodeManifest {
                    node: NodeManifestBody {
                        id: format!("channel.{}", instance_id),
                        display_name: display,
                        description: format!(
                            "Send a message via the {} channel adapter.",
                            adapter_name
                        ),
                        category: "channels".into(),
                        icon: "message-circle".into(),
                        color: "#f59e0b".into(),
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
                        properties: vec![NodeProperty {
                            name: "message".into(),
                            display_name: "Message".into(),
                            property_type: PropertyType::Template,
                            required: true,
                            placeholder: Some("{{output}}".into()),
                            ..Default::default()
                        }],
                        execute: NodeExecute::KernelCommand {
                            command: "SendChannelMessage".into(),
                            parameter_mapping: param_map,
                        },
                        ..Default::default()
                    },
                }
            })
            .collect()
    }
}
