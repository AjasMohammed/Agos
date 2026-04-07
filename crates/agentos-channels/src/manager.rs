use crate::types::{InboundMessage, OutboundMessage};
use crate::{ChannelAdapter, ChannelHealth};
use agentos_types::AgentOSError;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

struct ManagedChannel {
    adapter: Arc<dyn ChannelAdapter>,
    listener_handle: JoinHandle<()>,
    cancel: CancellationToken,
}

pub struct ChannelManager {
    channels: RwLock<HashMap<String, ManagedChannel>>,
    pub inbound_tx: mpsc::Sender<InboundMessage>,
    cancel: CancellationToken,
}

impl ChannelManager {
    pub fn new(inbound_tx: mpsc::Sender<InboundMessage>, cancel: CancellationToken) -> Self {
        Self {
            channels: RwLock::new(HashMap::new()),
            inbound_tx,
            cancel,
        }
    }

    pub async fn register(
        &self,
        instance_id: &str,
        adapter: Arc<dyn ChannelAdapter>,
    ) -> Result<(), AgentOSError> {
        let child_cancel = self.cancel.child_token();
        let handle = Self::spawn_listener(
            adapter.clone(),
            self.inbound_tx.clone(),
            child_cancel.clone(),
        );

        self.channels.write().await.insert(
            instance_id.to_string(),
            ManagedChannel {
                adapter,
                listener_handle: handle,
                cancel: child_cancel,
            },
        );
        info!("Registered channel adapter: {}", instance_id);
        Ok(())
    }

    fn spawn_listener(
        adapter: Arc<dyn ChannelAdapter>,
        tx: mpsc::Sender<InboundMessage>,
        cancel: CancellationToken,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            if let Err(e) = adapter.start_listener(tx, cancel).await {
                tracing::error!("Channel listener failed: {}", e);
            }
        })
    }

    pub async fn send(&self, instance_id: &str, msg: OutboundMessage) -> Result<(), AgentOSError> {
        let channels = self.channels.read().await;
        let managed =
            channels
                .get(instance_id)
                .ok_or_else(|| AgentOSError::ToolExecutionFailed {
                    tool_name: "channel_manager".to_string(),
                    reason: format!("channel {} not found", instance_id),
                })?;
        managed.adapter.send(msg).await?;
        Ok(())
    }

    pub async fn health(&self) -> HashMap<String, ChannelHealth> {
        let channels = self.channels.read().await;
        let mut results = HashMap::new();
        for (id, managed) in channels.iter() {
            results.insert(id.clone(), managed.adapter.health_check().await);
        }
        results
    }

    /// Supervision loop: checks listener health every 30 seconds and restarts
    /// any that have exited unexpectedly.
    pub async fn supervise(&self, cancel: CancellationToken) {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = interval.tick() => {
                    self.check_and_restart_listeners().await;
                }
            }
        }
    }

    async fn check_and_restart_listeners(&self) {
        let mut channels = self.channels.write().await;
        for (id, managed) in channels.iter_mut() {
            if managed.listener_handle.is_finished() {
                warn!(channel = %id, "Listener died, restarting");
                // Create a new child cancel token
                let child_cancel = self.cancel.child_token();
                let handle = Self::spawn_listener(
                    managed.adapter.clone(),
                    self.inbound_tx.clone(),
                    child_cancel.clone(),
                );
                managed.listener_handle = handle;
                managed.cancel = child_cancel;
            }
        }
    }

    pub async fn deregister(&self, instance_id: &str) {
        if let Some(managed) = self.channels.write().await.remove(instance_id) {
            managed.cancel.cancel();
        }
    }

    pub async fn adapter_count(&self) -> usize {
        self.channels.read().await.len()
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
