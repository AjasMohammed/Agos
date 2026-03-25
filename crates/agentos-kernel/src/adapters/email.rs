use crate::notification_router::{DeliveryAdapter, DeliveryError, InboundMessage};
use agentos_types::{DeliveryChannel, UserMessage};
use async_trait::async_trait;
use tokio::sync::mpsc;

/// Email delivery adapter (stub — SMTP/IMAP not yet wired).
///
/// Full implementation requires `lettre` (SMTP) and `async-imap` (IMAP IDLE)
/// dependencies.  This stub marks the adapter as unavailable so the kernel
/// can register it without breaking existing behaviour when email is enabled
/// in config.
///
/// A future PR will add the SMTP `deliver()` implementation and IMAP IDLE
/// `start_listening()` for reply detection.
pub struct EmailDeliveryAdapter;

#[async_trait]
impl DeliveryAdapter for EmailDeliveryAdapter {
    fn channel_id(&self) -> DeliveryChannel {
        DeliveryChannel::custom(DeliveryChannel::EMAIL)
    }

    async fn deliver(&self, _msg: &UserMessage) -> Result<(), DeliveryError> {
        Err(DeliveryError(
            "Email adapter is not yet implemented. \
             Use the webhook or Slack adapter for external notifications."
                .to_string(),
        ))
    }

    async fn is_available(&self) -> bool {
        false
    }

    fn supports_inbound(&self) -> bool {
        false
    }

    async fn start_listening(
        &self,
        _tx: mpsc::Sender<InboundMessage>,
    ) -> Result<tokio::task::JoinHandle<()>, DeliveryError> {
        Err(DeliveryError("Email inbound not yet implemented".into()))
    }
}
