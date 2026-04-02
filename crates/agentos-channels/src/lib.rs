pub mod discord;
pub mod email;
pub mod manager;
pub mod slack;
pub mod telegram;
pub mod types;
pub mod webhook;
pub mod whatsapp;

use agentos_types::AgentOSError;
use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use types::{ChannelCapabilities, DeliveryReceipt, InboundMessage, OutboundMessage};

#[derive(Debug, Clone)]
pub enum ChannelHealth {
    Connected,
    Degraded(String),
    Disconnected(String),
}

#[async_trait]
pub trait ChannelAdapter: Send + Sync {
    fn name(&self) -> &str;
    fn capabilities(&self) -> ChannelCapabilities;
    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, AgentOSError>;
    async fn start_listener(
        &self,
        tx: mpsc::Sender<InboundMessage>,
        cancel: CancellationToken,
    ) -> Result<(), AgentOSError>;
    async fn health_check(&self) -> ChannelHealth;
}
