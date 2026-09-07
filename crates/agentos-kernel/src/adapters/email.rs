use crate::notification_router::{DeliveryAdapter, DeliveryError, InboundMessage};
use agentos_types::{ChannelInstanceID, DeliveryChannel, UserMessage};
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
pub struct EmailDeliveryAdapter {
    /// `build_channel_adapter` classifies `ChannelKind::Email` as a
    /// delivery-stack kind and registers this adapter on the
    /// `NotificationRouter`, so it must claim its instance id like the
    /// Telegram and Ntfy adapters do. Without it `adapter_for` found nothing,
    /// `send_to_channel` fell through to the `ChannelManager` — which has no
    /// such instance — and the caller got "channel <uuid> not found", naming
    /// the wrong subsystem.
    channel_instance_id: ChannelInstanceID,
}

impl EmailDeliveryAdapter {
    pub fn new(channel_instance_id: ChannelInstanceID) -> Self {
        Self {
            channel_instance_id,
        }
    }
}

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

    /// Always `false` — and that is now load-bearing: `deliver_via` turns an
    /// unavailable adapter into an error, so `channel-send` reports the failure
    /// instead of answering "delivered" and writing a `ChannelMessageSent`
    /// audit row. `deliver()` above is consequently unreachable through the
    /// router; it stays as the fallback for any direct caller.
    async fn is_available(&self) -> bool {
        false
    }

    fn adapter_instance_id(&self) -> Option<String> {
        Some(self.channel_instance_id.to_string())
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
