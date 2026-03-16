use agentos_types::{AgentID, AgentMessage, AgentOSError, EventSeverity, EventType, GroupID};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use std::collections::HashMap;
use tokio::sync::mpsc;
use tokio::sync::RwLock;

/// Lightweight notification sent by AgentMessageBus to the kernel.
/// The kernel converts these into properly HMAC-signed EventMessages with audit trail.
#[derive(Debug, Clone)]
pub struct CommNotification {
    pub event_type: EventType,
    pub severity: EventSeverity,
    pub payload: serde_json::Value,
}

pub struct AgentMessageBus {
    /// Per-agent message channels. Each agent has an inbox.
    inboxes: RwLock<HashMap<AgentID, mpsc::UnboundedSender<AgentMessage>>>,
    /// Agent group memberships.
    groups: RwLock<HashMap<GroupID, Vec<AgentID>>>,
    /// Message history for audit and retrieval.
    history: RwLock<Vec<AgentMessage>>,
    /// Agent public keys for signature verification (hex-encoded).
    pub_keys: RwLock<HashMap<AgentID, String>>,
    /// Optional channel for notifying the kernel of communication events.
    /// The kernel converts these into properly signed EventMessages.
    notification_sender: RwLock<Option<mpsc::Sender<CommNotification>>>,
}

impl AgentMessageBus {
    pub fn new() -> Self {
        Self {
            inboxes: RwLock::new(HashMap::new()),
            groups: RwLock::new(HashMap::new()),
            history: RwLock::new(Vec::new()),
            pub_keys: RwLock::new(HashMap::new()),
            notification_sender: RwLock::new(None),
        }
    }

    /// Inject the notification sender so the kernel receives communication events
    /// and converts them into properly HMAC-signed EventMessages.
    pub async fn set_notification_sender(&self, sender: mpsc::Sender<CommNotification>) {
        *self.notification_sender.write().await = Some(sender);
    }

    /// Send a lightweight notification to the kernel for signing and dispatch.
    async fn notify(
        &self,
        event_type: EventType,
        severity: EventSeverity,
        payload: serde_json::Value,
    ) {
        let sender = self.notification_sender.read().await;
        if let Some(ref sender) = *sender {
            let notification = CommNotification {
                event_type,
                severity,
                payload,
            };
            if let Err(e) = sender.try_send(notification) {
                tracing::warn!(error = %e, "Failed to send communication notification (possibly full or closed)");
            }
        }
    }

    /// Register a public key for signature verification.
    pub async fn register_pubkey(&self, agent_id: AgentID, pubkey_hex: String) {
        self.pub_keys.write().await.insert(agent_id, pubkey_hex);
    }

    /// Verify an Ed25519 message signature against the sender's registered public key.
    /// Returns Ok(()) if the signature is valid, or an error if missing/invalid.
    async fn verify_message_signature(&self, message: &AgentMessage) -> Result<(), AgentOSError> {
        let sig_hex = message
            .signature
            .as_deref()
            .ok_or_else(|| AgentOSError::KernelError {
                reason: format!(
                    "Message {} from agent {} has no signature — rejected",
                    message.id, message.from
                ),
            })?;

        let pub_keys = self.pub_keys.read().await;
        let pubkey_hex = pub_keys
            .get(&message.from)
            .ok_or_else(|| AgentOSError::KernelError {
                reason: format!(
                    "No public key registered for agent {} — cannot verify message {}",
                    message.from, message.id
                ),
            })?;

        // Decode and verify
        let pub_bytes = hex::decode(pubkey_hex).map_err(|e| AgentOSError::KernelError {
            reason: format!("Invalid public key hex for agent {}: {}", message.from, e),
        })?;
        let pub_array: [u8; 32] = pub_bytes
            .try_into()
            .map_err(|_| AgentOSError::KernelError {
                reason: format!("Invalid public key length for agent {}", message.from),
            })?;
        let verifying_key =
            VerifyingKey::from_bytes(&pub_array).map_err(|e| AgentOSError::KernelError {
                reason: format!("Invalid public key for agent {}: {}", message.from, e),
            })?;

        let sig_bytes = hex::decode(sig_hex).map_err(|e| AgentOSError::KernelError {
            reason: format!("Invalid signature hex in message {}: {}", message.id, e),
        })?;
        let sig_array: [u8; 64] = sig_bytes
            .try_into()
            .map_err(|_| AgentOSError::KernelError {
                reason: format!("Invalid signature length in message {}", message.id),
            })?;
        let signature = Signature::from_bytes(&sig_array);

        let payload = message.signing_payload();
        verifying_key
            .verify(&payload, &signature)
            .map_err(|_| AgentOSError::KernelError {
                reason: format!(
                    "Signature verification failed for message {} from agent {}",
                    message.id, message.from
                ),
            })
    }

    /// Register an agent's inbox when they connect.
    pub async fn register_agent(&self, agent_id: AgentID) -> mpsc::UnboundedReceiver<AgentMessage> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.inboxes.write().await.insert(agent_id, tx);
        rx
    }

    /// Unregister an agent when they disconnect. Queued messages are lost.
    pub async fn unregister_agent(&self, agent_id: &AgentID) {
        self.inboxes.write().await.remove(agent_id);
    }

    /// Send a direct message to a specific agent.
    pub async fn send_direct(&self, message: AgentMessage) -> Result<(), AgentOSError> {
        // Reject expired messages before delivery (Spec §10)
        if message.is_expired() {
            return Err(AgentOSError::KernelError {
                reason: format!("Message {} is expired (TTL exceeded)", message.id),
            });
        }

        // Enforce signature verification (Spec §10)
        self.verify_message_signature(&message).await?;

        let agent_id = match message.to {
            agentos_types::MessageTarget::Direct(id) => id,
            _ => {
                return Err(AgentOSError::KernelError {
                    reason: "Invalid target for send_direct".into(),
                })
            }
        };

        let from = message.from;
        let msg_id = message.id;

        let inboxes = self.inboxes.read().await;
        if let Some(tx) = inboxes.get(&agent_id) {
            let _ = tx.send(message.clone());
            self.history.write().await.push(message);
            self.notify(
                EventType::DirectMessageReceived,
                EventSeverity::Info,
                serde_json::json!({
                    "from_agent": from.to_string(),
                    "to_agent": agent_id.to_string(),
                    "message_id": msg_id.to_string(),
                }),
            )
            .await;
            Ok(())
        } else {
            self.notify(
                EventType::MessageDeliveryFailed,
                EventSeverity::Warning,
                serde_json::json!({
                    "from_agent": from.to_string(),
                    "to_agent": agent_id.to_string(),
                    "error": format!("Agent {} not found", agent_id),
                }),
            )
            .await;
            Err(AgentOSError::AgentNotFound(agent_id.to_string()))
        }
    }

    /// Broadcast a message to all connected agents (except sender).
    pub async fn broadcast(&self, message: AgentMessage) -> Result<u32, AgentOSError> {
        // Reject expired messages before delivery (Spec §10)
        if message.is_expired() {
            let reason = format!("Message {} is expired (TTL exceeded)", message.id);
            self.notify(
                EventType::MessageDeliveryFailed,
                EventSeverity::Warning,
                serde_json::json!({
                    "from_agent": message.from.to_string(),
                    "error": reason.clone(),
                }),
            )
            .await;
            return Err(AgentOSError::KernelError { reason });
        }

        // Enforce signature verification (Spec §10)
        if let Err(e) = self.verify_message_signature(&message).await {
            self.notify(
                EventType::MessageDeliveryFailed,
                EventSeverity::Warning,
                serde_json::json!({
                    "from_agent": message.from.to_string(),
                    "error": e.to_string(),
                }),
            )
            .await;
            return Err(e);
        }

        let from = message.from;
        let msg_id = message.id;
        let mut count = 0u32;
        let inboxes = self.inboxes.read().await;

        for (id, tx) in inboxes.iter() {
            if *id != from {
                let _ = tx.send(message.clone());
                count += 1;
            }
        }

        self.history.write().await.push(message);
        self.notify(
            EventType::BroadcastReceived,
            EventSeverity::Info,
            serde_json::json!({
                "from_agent": from.to_string(),
                "recipient_count": count,
                "message_id": msg_id.to_string(),
            }),
        )
        .await;
        Ok(count)
    }

    /// Send to a group.
    pub async fn send_to_group(
        &self,
        group_id: &GroupID,
        message: AgentMessage,
    ) -> Result<u32, AgentOSError> {
        // Reject expired messages before delivery (Spec §10)
        if message.is_expired() {
            self.notify(
                EventType::MessageDeliveryFailed,
                EventSeverity::Warning,
                serde_json::json!({
                    "from_agent": message.from.to_string(),
                    "group_id": group_id.to_string(),
                    "error": format!("Message {} is expired (TTL exceeded)", message.id),
                }),
            )
            .await;
            return Err(AgentOSError::KernelError {
                reason: format!("Message {} is expired (TTL exceeded)", message.id),
            });
        }

        // Enforce signature verification (Spec §10)
        if let Err(e) = self.verify_message_signature(&message).await {
            self.notify(
                EventType::MessageDeliveryFailed,
                EventSeverity::Warning,
                serde_json::json!({
                    "from_agent": message.from.to_string(),
                    "group_id": group_id.to_string(),
                    "error": e.to_string(),
                }),
            )
            .await;
            return Err(e);
        }

        let from = message.from;
        let msg_id = message.id;

        let groups = self.groups.read().await;
        let members = groups
            .get(group_id)
            .ok_or_else(|| AgentOSError::KernelError {
                reason: format!("Group {} not found", group_id),
            })?;

        let mut count = 0u32;
        let inboxes = self.inboxes.read().await;

        for &id in members {
            if id != from {
                if let Some(tx) = inboxes.get(&id) {
                    let _ = tx.send(message.clone());
                    count += 1;
                }
            }
        }

        // Release locks before notifying
        drop(inboxes);
        drop(groups);

        self.history.write().await.push(message);
        self.notify(
            EventType::BroadcastReceived,
            EventSeverity::Info,
            serde_json::json!({
                "from_agent": from.to_string(),
                "group_id": group_id.to_string(),
                "recipient_count": count,
                "message_id": msg_id.to_string(),
            }),
        )
        .await;
        Ok(count)
    }

    /// Create a named group of agents.
    pub async fn create_group(&self, group_id: GroupID, members: Vec<AgentID>) {
        self.groups.write().await.insert(group_id, members);
    }

    /// Get recent message history for an agent.
    pub async fn get_history(&self, agent_id: &AgentID, limit: usize) -> Vec<AgentMessage> {
        let history = self.history.read().await;
        history
            .iter()
            .filter(|m| {
                m.from == *agent_id
                    || match m.to {
                        agentos_types::MessageTarget::Direct(to) => to == *agent_id,
                        agentos_types::MessageTarget::Broadcast => true,
                        _ => false, // Can handle groups later if needed
                    }
            })
            .rev()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }

    /// Get pending (undelivered) message count for an agent.
    pub async fn pending_count(&self, agent_id: &AgentID) -> usize {
        let inboxes = self.inboxes.read().await;
        if let Some(_tx) = inboxes.get(agent_id) {
            // Unbounded channels do not expose pending queue easily,
            // we'd need to adapt, returning 0 for now
            return 0; // rx handles consumption immediately if polled
        }
        0
    }
}

// Ensure the bus matches the implementation
impl Default for AgentMessageBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::{EventSeverity, EventType};
    use agentos_types::{MessageContent, MessageID, MessageTarget, TraceID};
    use ed25519_dalek::{Signer, SigningKey};

    /// Helper: create a signed message from a given signing key.
    fn make_signed_msg(
        signing_key: &SigningKey,
        from: AgentID,
        to: MessageTarget,
        content: &str,
        ttl_seconds: u64,
    ) -> AgentMessage {
        let now = chrono::Utc::now();
        let mut msg = AgentMessage {
            id: MessageID::new(),
            from,
            to,
            content: MessageContent::Text(content.into()),
            reply_to: None,
            timestamp: now,
            trace_id: TraceID::new(),
            signature: None,
            ttl_seconds,
            expires_at: Some(now + chrono::Duration::seconds(ttl_seconds as i64)),
        };
        let payload = msg.signing_payload();
        let sig = signing_key.sign(&payload);
        msg.signature = Some(hex::encode(sig.to_bytes()));
        msg
    }

    fn make_keypair() -> (SigningKey, String) {
        let mut csprng = rand::rngs::OsRng;
        let sk = SigningKey::generate(&mut csprng);
        let pk_hex = hex::encode(sk.verifying_key().to_bytes());
        (sk, pk_hex)
    }

    #[tokio::test]
    async fn test_direct_message_delivery() {
        let bus = AgentMessageBus::new();
        let agent_a = AgentID::new();
        let agent_b = AgentID::new();
        let (sk_a, pk_a) = make_keypair();

        let mut inbox_b = bus.register_agent(agent_b).await;
        bus.register_agent(agent_a).await;
        bus.register_pubkey(agent_a, pk_a).await;

        let msg = make_signed_msg(
            &sk_a,
            agent_a,
            MessageTarget::Direct(agent_b),
            "Hello from A",
            60,
        );
        bus.send_direct(msg).await.unwrap();

        let received = inbox_b.recv().await.unwrap();
        assert_eq!(received.from, agent_a);
    }

    #[tokio::test]
    async fn test_broadcast_reaches_all_except_sender() {
        let bus = AgentMessageBus::new();
        let a = AgentID::new();
        let b = AgentID::new();
        let c = AgentID::new();
        let (sk_a, pk_a) = make_keypair();

        bus.register_agent(a).await;
        let mut inbox_b = bus.register_agent(b).await;
        let mut inbox_c = bus.register_agent(c).await;
        bus.register_pubkey(a, pk_a).await;

        let msg = make_signed_msg(&sk_a, a, MessageTarget::Broadcast, "Hello all", 60);
        let count = bus.broadcast(msg).await.unwrap();
        assert_eq!(count, 2); // b and c, not a

        assert!(inbox_b.try_recv().is_ok());
        assert!(inbox_c.try_recv().is_ok());
    }

    #[tokio::test]
    async fn test_message_to_nonexistent_agent_fails() {
        let bus = AgentMessageBus::new();
        let from = AgentID::new();
        let (sk, pk) = make_keypair();
        bus.register_pubkey(from, pk).await;

        let msg = make_signed_msg(&sk, from, MessageTarget::Direct(AgentID::new()), "ping", 60);
        assert!(bus.send_direct(msg).await.is_err());
    }

    #[tokio::test]
    async fn test_expired_message_rejected() {
        let bus = AgentMessageBus::new();
        let agent_a = AgentID::new();
        let agent_b = AgentID::new();
        bus.register_agent(agent_b).await;
        bus.register_agent(agent_a).await;

        // Message whose TTL expired 10 seconds ago
        let past = chrono::Utc::now() - chrono::Duration::seconds(10);
        let msg = AgentMessage {
            id: MessageID::new(),
            from: agent_a,
            to: MessageTarget::Direct(agent_b),
            content: MessageContent::Text("stale".into()),
            reply_to: None,
            timestamp: past,
            trace_id: TraceID::new(),
            signature: None,
            ttl_seconds: 5,
            expires_at: Some(past + chrono::Duration::seconds(5)),
        };

        let result = bus.send_direct(msg).await;
        assert!(result.is_err(), "expired message should be rejected");
        assert!(result.unwrap_err().to_string().contains("expired"));
    }

    #[tokio::test]
    async fn test_unsigned_message_rejected() {
        let bus = AgentMessageBus::new();
        let agent_a = AgentID::new();
        let agent_b = AgentID::new();
        bus.register_agent(agent_b).await;
        bus.register_agent(agent_a).await;

        let now = chrono::Utc::now();
        let msg = AgentMessage {
            id: MessageID::new(),
            from: agent_a,
            to: MessageTarget::Direct(agent_b),
            content: MessageContent::Text("unsigned message".into()),
            reply_to: None,
            timestamp: now,
            trace_id: TraceID::new(),
            signature: None,
            ttl_seconds: 60,
            expires_at: Some(now + chrono::Duration::seconds(60)),
        };

        let result = bus.send_direct(msg).await;
        assert!(result.is_err(), "unsigned message should be rejected");
        assert!(result.unwrap_err().to_string().contains("no signature"));
    }

    #[tokio::test]
    async fn test_invalid_signature_rejected() {
        let bus = AgentMessageBus::new();
        let agent_a = AgentID::new();
        let agent_b = AgentID::new();
        let (_sk_a, pk_a) = make_keypair();

        bus.register_agent(agent_b).await;
        bus.register_agent(agent_a).await;
        bus.register_pubkey(agent_a, pk_a).await;

        let now = chrono::Utc::now();
        let msg = AgentMessage {
            id: MessageID::new(),
            from: agent_a,
            to: MessageTarget::Direct(agent_b),
            content: MessageContent::Text("bad sig".into()),
            reply_to: None,
            timestamp: now,
            trace_id: TraceID::new(),
            // Invalid signature (all zeros)
            signature: Some(hex::encode([0u8; 64])),
            ttl_seconds: 60,
            expires_at: Some(now + chrono::Duration::seconds(60)),
        };

        let result = bus.send_direct(msg).await;
        assert!(
            result.is_err(),
            "message with invalid signature should be rejected"
        );
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("verification failed"));
    }

    #[tokio::test]
    async fn test_valid_signed_message_delivered() {
        let bus = AgentMessageBus::new();
        let agent_a = AgentID::new();
        let agent_b = AgentID::new();
        let (sk_a, pk_a) = make_keypair();

        let mut inbox_b = bus.register_agent(agent_b).await;
        bus.register_agent(agent_a).await;
        bus.register_pubkey(agent_a, pk_a).await;

        let msg = make_signed_msg(
            &sk_a,
            agent_a,
            MessageTarget::Direct(agent_b),
            "signed msg",
            60,
        );
        bus.send_direct(msg).await.unwrap();

        let received = inbox_b.recv().await.unwrap();
        assert_eq!(received.from, agent_a);
        if let MessageContent::Text(t) = &received.content {
            assert_eq!(t, "signed msg");
        } else {
            panic!("Expected text message");
        }
    }

    #[tokio::test]
    async fn test_direct_message_emits_event() {
        let bus = AgentMessageBus::new();
        let (notif_tx, mut notif_rx) = mpsc::channel(64);
        bus.set_notification_sender(notif_tx).await;

        let agent_a = AgentID::new();
        let agent_b = AgentID::new();
        let (sk_a, pk_a) = make_keypair();

        bus.register_agent(agent_b).await;
        bus.register_agent(agent_a).await;
        bus.register_pubkey(agent_a, pk_a).await;

        let msg = make_signed_msg(
            &sk_a,
            agent_a,
            MessageTarget::Direct(agent_b),
            "event test",
            60,
        );
        bus.send_direct(msg).await.unwrap();

        let notif = notif_rx
            .try_recv()
            .expect("should receive DirectMessageReceived notification");
        assert_eq!(notif.event_type, EventType::DirectMessageReceived);
        assert_eq!(notif.severity, EventSeverity::Info);
        assert_eq!(
            notif.payload["from_agent"].as_str().unwrap(),
            agent_a.to_string()
        );
        assert_eq!(
            notif.payload["to_agent"].as_str().unwrap(),
            agent_b.to_string()
        );
    }

    #[tokio::test]
    async fn test_broadcast_emits_event() {
        let bus = AgentMessageBus::new();
        let (notif_tx, mut notif_rx) = mpsc::channel(64);
        bus.set_notification_sender(notif_tx).await;

        let a = AgentID::new();
        let b = AgentID::new();
        let c = AgentID::new();
        let (sk_a, pk_a) = make_keypair();

        bus.register_agent(a).await;
        bus.register_agent(b).await;
        bus.register_agent(c).await;
        bus.register_pubkey(a, pk_a).await;

        let msg = make_signed_msg(&sk_a, a, MessageTarget::Broadcast, "broadcast event", 60);
        let count = bus.broadcast(msg).await.unwrap();
        assert_eq!(count, 2);

        let notif = notif_rx
            .try_recv()
            .expect("should receive BroadcastReceived notification");
        assert_eq!(notif.event_type, EventType::BroadcastReceived);
        assert_eq!(notif.payload["recipient_count"].as_u64().unwrap(), 2);
    }

    #[tokio::test]
    async fn test_delivery_failure_emits_event() {
        let bus = AgentMessageBus::new();
        let (notif_tx, mut notif_rx) = mpsc::channel(64);
        bus.set_notification_sender(notif_tx).await;

        let from = AgentID::new();
        let (sk, pk) = make_keypair();
        bus.register_pubkey(from, pk).await;

        let msg = make_signed_msg(&sk, from, MessageTarget::Direct(AgentID::new()), "fail", 60);
        assert!(bus.send_direct(msg).await.is_err());

        let notif = notif_rx
            .try_recv()
            .expect("should receive MessageDeliveryFailed notification");
        assert_eq!(notif.event_type, EventType::MessageDeliveryFailed);
        assert_eq!(notif.severity, EventSeverity::Warning);
        assert!(notif.payload["error"]
            .as_str()
            .unwrap()
            .contains("not found"));
    }

    #[tokio::test]
    async fn test_bus_works_without_notification_sender() {
        // Verify the bus works correctly when notification_sender is None
        let bus = AgentMessageBus::new();
        let agent_a = AgentID::new();
        let agent_b = AgentID::new();
        let (sk_a, pk_a) = make_keypair();

        let mut inbox_b = bus.register_agent(agent_b).await;
        bus.register_agent(agent_a).await;
        bus.register_pubkey(agent_a, pk_a.clone()).await;

        let msg = make_signed_msg(
            &sk_a,
            agent_a,
            MessageTarget::Direct(agent_b),
            "no sender",
            60,
        );
        bus.send_direct(msg).await.unwrap();
        assert!(inbox_b.try_recv().is_ok());

        let msg = make_signed_msg(&sk_a, agent_a, MessageTarget::Broadcast, "no sender bc", 60);
        bus.broadcast(msg).await.unwrap();
    }
}
