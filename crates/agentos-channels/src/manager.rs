use crate::types::{InboundMessage, OutboundMessage};
use crate::{ChannelAdapter, ChannelHealth};
use agentos_types::AgentOSError;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio_util::sync::CancellationToken;
use tracing::info;

pub struct ChannelManager {
    adapters: RwLock<HashMap<String, Arc<dyn ChannelAdapter>>>,
    pub inbound_tx: mpsc::Sender<InboundMessage>,
    cancel: CancellationToken,
}

impl ChannelManager {
    pub fn new(inbound_tx: mpsc::Sender<InboundMessage>, cancel: CancellationToken) -> Self {
        Self {
            adapters: RwLock::new(HashMap::new()),
            inbound_tx,
            cancel,
        }
    }

    pub async fn register(
        &self,
        instance_id: &str,
        adapter: Arc<dyn ChannelAdapter>,
    ) -> Result<(), AgentOSError> {
        let tx = self.inbound_tx.clone();
        let cancel = self.cancel.child_token();
        let adapter_clone = adapter.clone();

        tokio::spawn(async move {
            if let Err(e) = adapter_clone.start_listener(tx, cancel).await {
                tracing::error!("Channel listener failed: {}", e);
            }
        });

        self.adapters
            .write()
            .await
            .insert(instance_id.to_string(), adapter);
        info!("Registered channel adapter: {}", instance_id);
        Ok(())
    }

    pub async fn send(&self, instance_id: &str, msg: OutboundMessage) -> Result<(), AgentOSError> {
        let adapters = self.adapters.read().await;
        let adapter =
            adapters
                .get(instance_id)
                .ok_or_else(|| AgentOSError::ToolExecutionFailed {
                    tool_name: "channel_manager".to_string(),
                    reason: format!("channel {} not found", instance_id),
                })?;
        adapter.send(msg).await?;
        Ok(())
    }

    pub async fn health(&self) -> HashMap<String, ChannelHealth> {
        let adapters = self.adapters.read().await;
        let mut results = HashMap::new();
        for (id, adapter) in adapters.iter() {
            results.insert(id.clone(), adapter.health_check().await);
        }
        results
    }

    pub async fn deregister(&self, instance_id: &str) {
        self.adapters.write().await.remove(instance_id);
    }

    pub async fn adapter_count(&self) -> usize {
        self.adapters.read().await.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ChannelCapabilities, DeliveryReceipt, InboundMessage, OutboundMessage};
    use crate::{ChannelAdapter, ChannelHealth};
    use async_trait::async_trait;

    struct MockAdapter;

    #[async_trait]
    impl ChannelAdapter for MockAdapter {
        fn name(&self) -> &str {
            "mock"
        }
        fn capabilities(&self) -> ChannelCapabilities {
            ChannelCapabilities {
                threads: false,
                reactions: false,
                media: false,
                rich_formatting: false,
                max_message_length: 1000,
            }
        }
        async fn send(&self, _msg: OutboundMessage) -> Result<DeliveryReceipt, AgentOSError> {
            Ok(DeliveryReceipt {
                message_id: "test".to_string(),
                delivered_at: chrono::Utc::now(),
            })
        }
        async fn start_listener(
            &self,
            _tx: mpsc::Sender<InboundMessage>,
            cancel: CancellationToken,
        ) -> Result<(), AgentOSError> {
            cancel.cancelled().await;
            Ok(())
        }
        async fn health_check(&self) -> ChannelHealth {
            ChannelHealth::Connected
        }
    }

    #[tokio::test]
    async fn test_register_and_send() {
        let (tx, _rx) = mpsc::channel(16);
        let cancel = CancellationToken::new();
        let manager = ChannelManager::new(tx, cancel);

        manager
            .register("test1", Arc::new(MockAdapter))
            .await
            .unwrap();
        let msg = OutboundMessage {
            channel_instance_id: "test1".to_string(),
            content: crate::types::MessageContent::Text("hello".to_string()),
            thread_id: None,
        };
        manager.send("test1", msg).await.unwrap();
    }

    #[tokio::test]
    async fn test_send_to_unknown_channel() {
        let (tx, _rx) = mpsc::channel(16);
        let cancel = CancellationToken::new();
        let manager = ChannelManager::new(tx, cancel);
        let msg = OutboundMessage {
            channel_instance_id: "nonexistent".to_string(),
            content: crate::types::MessageContent::Text("hello".to_string()),
            thread_id: None,
        };
        assert!(manager.send("nonexistent", msg).await.is_err());
    }

    #[tokio::test]
    async fn test_deregister() {
        let (tx, _rx) = mpsc::channel(16);
        let cancel = CancellationToken::new();
        let manager = ChannelManager::new(tx, cancel);

        manager
            .register("inst1", Arc::new(MockAdapter))
            .await
            .unwrap();
        assert_eq!(manager.adapter_count().await, 1);
        manager.deregister("inst1").await;
        assert_eq!(manager.adapter_count().await, 0);
    }
}
