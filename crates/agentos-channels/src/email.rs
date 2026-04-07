use crate::types::*;
use crate::{ChannelAdapter, ChannelCapabilities, ChannelHealth};
use agentos_types::AgentOSError;
use async_trait::async_trait;
use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

pub struct EmailAdapter {
    pub smtp_host: String,
    pub smtp_port: u16,
    username: Zeroizing<String>,
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

    fn build_transport(&self) -> Result<AsyncSmtpTransport<Tokio1Executor>, AgentOSError> {
        let creds = Credentials::new(
            self.username.as_str().to_string(),
            self.password.as_str().to_string(),
        );

        let transport = AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&self.smtp_host)
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "email".to_string(),
                reason: format!("SMTP relay error: {e}"),
            })?
            .port(self.smtp_port)
            .credentials(creds)
            .build();

        Ok(transport)
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
        let text = msg.content.as_text();
        let subject = msg.thread_id.as_deref().unwrap_or("[AgentOS] Notification");

        let email = Message::builder()
            .from(
                self.from_address
                    .parse()
                    .map_err(|e| AgentOSError::ToolExecutionFailed {
                        tool_name: "email".to_string(),
                        reason: format!("Invalid from address: {e}"),
                    })?,
            )
            .to(self
                .to_address
                .parse()
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "email".to_string(),
                    reason: format!("Invalid to address: {e}"),
                })?)
            .subject(subject)
            .header(ContentType::TEXT_PLAIN)
            .body(text)
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "email".to_string(),
                reason: format!("Failed to build email: {e}"),
            })?;

        let transport = self.build_transport()?;

        let response =
            transport
                .send(email)
                .await
                .map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "email".to_string(),
                    reason: format!("SMTP send failed: {e}"),
                })?;

        let message_id = response.message().next().unwrap_or("unknown").to_string();

        Ok(DeliveryReceipt {
            message_id,
            delivered_at: chrono::Utc::now(),
        })
    }

    async fn start_listener(
        &self,
        _tx: mpsc::Sender<InboundMessage>,
        cancel: CancellationToken,
    ) -> Result<(), AgentOSError> {
        // IMAP IDLE listener is not yet implemented.
        // Unlike the old stub, this is clearly documented as a no-op — inbound
        // email is not supported. Callers should check capabilities or use the
        // REST webhook path to inject inbound email.
        cancel.cancelled().await;
        Ok(())
    }

    async fn health_check(&self) -> ChannelHealth {
        match self.build_transport() {
            Ok(transport) => match transport.test_connection().await {
                Ok(true) => ChannelHealth::Connected,
                Ok(false) => ChannelHealth::Degraded("SMTP handshake returned false".to_string()),
                Err(e) => ChannelHealth::Disconnected(format!("SMTP connection test failed: {e}")),
            },
            Err(e) => ChannelHealth::Disconnected(e.to_string()),
        }
    }
}
