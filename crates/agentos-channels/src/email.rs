use crate::types::*;
use crate::{ChannelAdapter, ChannelCapabilities, ChannelHealth};
use agentos_types::AgentOSError;
use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

pub struct EmailAdapter {
    pub smtp_host: String,
    pub smtp_port: u16,
    #[allow(dead_code)]
    username: Zeroizing<String>,
    #[allow(dead_code)]
    password: Zeroizing<String>,
    pub from_address: String,
    pub to_address: String,
    pub instance_id: String,
}

impl EmailAdapter {
    pub fn new(
        smtp_host: String,
        smtp_port: u16,
        username: String,
        password: String,
        from_address: String,
        to_address: String,
        instance_id: String,
    ) -> Self {
        Self {
            smtp_host,
            smtp_port,
            username: Zeroizing::new(username),
            password: Zeroizing::new(password),
            from_address,
            to_address,
            instance_id,
        }
    }
}

#[async_trait]
impl ChannelAdapter for EmailAdapter {
    fn name(&self) -> &str {
        "email"
    }

    fn capabilities(&self) -> ChannelCapabilities {
        ChannelCapabilities {
            threads: true,
            reactions: false,
            media: true,
            rich_formatting: true,
            max_message_length: 1_000_000,
        }
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, AgentOSError> {
        // Stub: full SMTP implementation requires the `lettre` crate (not in workspace).
        // Returns a stub receipt; real send would use tokio::task::spawn_blocking + SMTP.
        let _ = msg;
        Ok(DeliveryReceipt {
            message_id: uuid::Uuid::new_v4().to_string(),
            delivered_at: chrono::Utc::now(),
        })
    }

    async fn start_listener(
        &self,
        _tx: mpsc::Sender<InboundMessage>,
        cancel: CancellationToken,
    ) -> Result<(), AgentOSError> {
        // IMAP IDLE listener would go here.
        // Full implementation requires async-imap crate.
        cancel.cancelled().await;
        Ok(())
    }

    async fn health_check(&self) -> ChannelHealth {
        // TCP connect to SMTP host to verify reachability.
        match tokio::net::TcpStream::connect(format!("{}:{}", self.smtp_host, self.smtp_port)).await
        {
            Ok(_) => ChannelHealth::Connected,
            Err(e) => ChannelHealth::Disconnected(e.to_string()),
        }
    }
}
