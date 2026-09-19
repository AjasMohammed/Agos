use agentos_types::{
    AgentID, AgentMessage, AgentOSError, EventSeverity, EventType, GroupID, MessageContent,
};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use std::collections::{HashMap, VecDeque};
use tokio::sync::mpsc;
use tokio::sync::RwLock;

/// Maximum number of pending messages per agent inbox before new sends are rejected.
/// Prevents unbounded memory growth when a slow or stalled agent accumulates messages.
const INBOX_CAPACITY: usize = 256;

/// Maximum retained message history entries across all agents.
/// Oldest entries are discarded when this limit is exceeded.
const MAX_HISTORY: usize = 10_000;

/// Lightweight notification sent by AgentMessageBus to the kernel.
/// The kernel converts these into properly HMAC-signed EventMessages with audit trail.
#[derive(Debug, Clone)]
pub struct CommNotification {
    pub event_type: EventType,
    pub severity: EventSeverity,
    pub payload: serde_json::Value,
    /// Event-chain depth of the activity that produced this notification.
    /// Non-zero only for messages sent by a task that was itself event-triggered,
    /// so `max_chain_depth` can terminate an A→B→A reply loop.
    pub chain_depth: u32,
}

pub struct AgentMessageBus {
    /// Per-agent message channels. Each agent has a bounded inbox.
    /// Uses a bounded channel (INBOX_CAPACITY) to prevent unbounded memory growth
    /// when an agent is slow or stalled.
    inboxes: RwLock<HashMap<AgentID, mpsc::Sender<AgentMessage>>>,
    /// Agent group memberships.
    groups: RwLock<HashMap<GroupID, Vec<AgentID>>>,
    /// Group name -> id, so operator commands can address a group by its name.
    /// ponytail: in-memory like `groups` itself — groups are re-created after a
    /// kernel restart. Persist both maps together if that becomes a real cost.
    group_names: RwLock<HashMap<String, GroupID>>,
    /// Message history for audit and retrieval. `VecDeque` so evicting the oldest
    /// entry at `MAX_HISTORY` is O(1) rather than a memmove of the whole ring.
    history: RwLock<VecDeque<AgentMessage>>,
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
            group_names: RwLock::new(HashMap::new()),
            history: RwLock::new(VecDeque::new()),
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
        self.notify_at_depth(event_type, severity, payload, 0).await
    }

    /// `notify` carrying the sender's event-chain depth (see `CommNotification`).
    async fn notify_at_depth(
        &self,
        event_type: EventType,
        severity: EventSeverity,
        payload: serde_json::Value,
        chain_depth: u32,
    ) {
        let sender = self.notification_sender.read().await;
        if let Some(ref sender) = *sender {
            let notification = CommNotification {
                event_type,
                severity,
                payload,
                chain_depth,
            };
            if let Err(e) = sender.try_send(notification) {
                tracing::warn!(error = %e, "Failed to send communication notification (possibly full or closed)");
            }
        }
    }

    /// Register a public key for an agent during kernel-controlled agent registration.
    ///
    /// **Kernel-internal use only.** This method is `pub` solely to allow integration tests
    /// in `tests/` to set up bus state without going through the full `cmd_connect_agent`
    /// flow. Production callers must be `cmd_connect_agent` or the boot-time pre-population
    /// in `kernel.rs`. Calling this from any other context may create a security vulnerability.
    ///
    /// Enforces immutability: once a pubkey is stored for an agent, it cannot be changed —
    /// the agent must be deregistered and re-registered to rotate keys (which is audited).
    /// This closes the authentication bypass where any component could call the old public
    /// `register_pubkey()` with an attacker key and then forge messages as any agent.
    ///
    /// Returns:
    /// - `Ok(())` if the key was registered successfully, or the same key was already set
    ///   (idempotent — safe to call on reconnect).
    /// - `Err(PubkeyAlreadyRegistered)` if a **different** key is already registered.
    #[doc(hidden)]
    pub async fn register_pubkey_internal(
        &self,
        agent_id: AgentID,
        pubkey_hex: String,
    ) -> Result<(), AgentOSError> {
        let mut pub_keys = self.pub_keys.write().await;
        if let Some(existing) = pub_keys.get(&agent_id) {
            if existing == &pubkey_hex {
                // Same key — idempotent, safe to call on reconnect.
                return Ok(());
            }
            // A different key is already registered for this agent. Reject to prevent
            // an attacker from overwriting a legitimate agent's verification key.
            return Err(AgentOSError::PubkeyAlreadyRegistered {
                agent_id: agent_id.to_string(),
            });
        }
        pub_keys.insert(agent_id, pubkey_hex);
        Ok(())
    }

    /// Remove a registered pubkey. `pub(crate)` so only kernel internals can clear a slot.
    /// Making this `pub` would allow an external caller to deregister + re-register with an
    /// attacker key, re-opening the authentication bypass that `register_pubkey_internal`
    /// is designed to close.
    pub(crate) async fn deregister_pubkey(&self, agent_id: &AgentID) {
        self.pub_keys.write().await.remove(agent_id);
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
    /// Returns a bounded receiver; messages beyond INBOX_CAPACITY will be rejected
    /// with a MessageDeliveryFailed event rather than accumulating without bound.
    pub async fn register_agent(&self, agent_id: AgentID) -> mpsc::Receiver<AgentMessage> {
        let (tx, rx) = mpsc::channel(INBOX_CAPACITY);
        self.inboxes.write().await.insert(agent_id, tx);
        rx
    }

    /// Unregister an agent when they disconnect. Queued messages are lost.
    pub async fn unregister_agent(&self, agent_id: &AgentID) {
        self.inboxes.write().await.remove(agent_id);
    }

    /// Send a direct message to a specific agent.
    ///
    /// The caller owns the existence check: this method does not know the agent
    /// registry, so a message to an unknown `AgentID` is accepted and lands in
    /// history addressed to nobody. Both kernel callers resolve the target
    /// through `AgentRegistry` first.
    ///
    /// `chain_depth` is the event-chain depth of the sending task (0 for
    /// operator-initiated sends); it rides on the emitted `DirectMessageReceived`
    /// so a reply loop between two agents hits `max_chain_depth`.
    pub async fn send_direct(
        &self,
        message: AgentMessage,
        chain_depth: u32,
    ) -> Result<(), AgentOSError> {
        // Reject expired messages before delivery (Spec §10)
        if message.is_expired() {
            return Err(AgentOSError::KernelError {
                reason: format!("Message {} is expired (TTL exceeded)", message.id),
            });
        }

        // Enforce signature verification (Spec §10)
        if let Err(e) = self.verify_message_signature(&message).await {
            // Emit AgentImpersonationAttempt when signature verification fails
            self.notify(
                EventType::AgentImpersonationAttempt,
                EventSeverity::Critical,
                serde_json::json!({
                    "claimed_agent_id": message.from.to_string(),
                    "source": "direct_message",
                    "reason": e.to_string(),
                }),
            )
            .await;
            return Err(e);
        }

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
        let preview = content_preview(&message.content);

        let inboxes = self.inboxes.read().await;
        if let Some(tx) = inboxes.get(&agent_id) {
            match tx.try_send(message.clone()) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    // Inbox is full: the agent is alive but not keeping up.
                    // Reject the message and emit a delivery-failed event.
                    tracing::warn!(
                        agent_id = %agent_id,
                        "Agent inbox full — message rejected (backpressure)"
                    );
                    drop(inboxes);
                    self.notify(
                        EventType::MessageDeliveryFailed,
                        EventSeverity::Warning,
                        serde_json::json!({
                            "from_agent": from.to_string(),
                            "to_agent": agent_id.to_string(),
                            "message_id": msg_id.to_string(),
                            "reason": "inbox_full",
                        }),
                    )
                    .await;
                    return Err(AgentOSError::KernelError {
                        reason: format!("Agent {} inbox is full — message rejected", agent_id),
                    });
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    // Inbox closed: agent has disconnected but entry not yet cleaned up.
                    // Log and continue; message is silently dropped (same as pre-bound behavior).
                    tracing::warn!(
                        agent_id = %agent_id,
                        "Agent inbox closed (agent disconnected) — message dropped"
                    );
                }
            }
            drop(inboxes);
            self.push_history(message).await;
            self.notify_at_depth(
                EventType::DirectMessageReceived,
                EventSeverity::Info,
                serde_json::json!({
                    "from_agent": from.to_string(),
                    "to_agent": agent_id.to_string(),
                    "message_id": msg_id.to_string(),
                    "message_content": preview,
                    "live_listener": true,
                }),
                chain_depth,
            )
            .await;
            Ok(())
        } else {
            // No live mpsc listener. That is the normal case: durable delivery is the
            // SQLite inbox written by `AgentInboxWriter` before we are called, and the
            // recipient reads it via agent-messages-list/read. The mpsc inbox is only a
            // fast path for a process that is actively draining it.
            // ponytail: treat a missing inbox as "no live listener", not a failure.
            // Callers already verified the target exists in the agent registry.
            drop(inboxes);
            self.push_history(message).await;
            self.notify_at_depth(
                EventType::DirectMessageReceived,
                EventSeverity::Info,
                serde_json::json!({
                    "from_agent": from.to_string(),
                    "to_agent": agent_id.to_string(),
                    "message_id": msg_id.to_string(),
                    "message_content": preview,
                    "live_listener": false,
                }),
                chain_depth,
            )
            .await;
            Ok(())
        }
    }

    /// Broadcast a message to all agents with a live mpsc listener (except sender).
    ///
    /// ponytail: reaches only agents that registered an mpsc inbox — nothing does in
    /// production today, so this is test-only. Operator fan-out goes through
    /// `send_to_group`, whose caller writes the durable SQLite inbox per member. Wire
    /// a durable write here too before using this for a real all-agents announcement.
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
                EventType::AgentImpersonationAttempt,
                EventSeverity::Critical,
                serde_json::json!({
                    "claimed_agent_id": message.from.to_string(),
                    "source": "broadcast",
                    "reason": e.to_string(),
                }),
            )
            .await;
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
                match tx.try_send(message.clone()) {
                    Ok(()) => count += 1,
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        tracing::warn!(
                            agent_id = %id,
                            "Broadcast: agent inbox full — message skipped for this recipient"
                        );
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        // Agent disconnected but inbox entry not yet cleaned up; count as
                        // recipient anyway (inbox was registered, message logically targeted it).
                        count += 1;
                        tracing::debug!(
                            agent_id = %id,
                            "Broadcast: agent inbox closed — message dropped for disconnected agent"
                        );
                    }
                }
            }
        }

        drop(inboxes);
        self.push_history(message).await;
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

    /// Send to a group. `members` is the caller's already-resolved membership
    /// snapshot — the same list it wrote durable inbox entries for — so the two
    /// can never disagree if the group is replaced concurrently.
    pub async fn send_to_group(
        &self,
        group_id: &GroupID,
        members: &[AgentID],
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
                EventType::AgentImpersonationAttempt,
                EventSeverity::Critical,
                serde_json::json!({
                    "claimed_agent_id": message.from.to_string(),
                    "source": "group_message",
                    "group_id": group_id.to_string(),
                    "reason": e.to_string(),
                }),
            )
            .await;
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
        let preview = content_preview(&message.content);

        let mut count = 0u32;
        let inboxes = self.inboxes.read().await;

        for &id in members.iter() {
            if id != from {
                // Every member is a recipient: durable delivery is the SQLite inbox
                // written by the caller. The mpsc push below is only the live-listener
                // fast path, so its absence must not change the count.
                count += 1;
                if let Some(tx) = inboxes.get(&id) {
                    match tx.try_send(message.clone()) {
                        Ok(()) => {}
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            // No MessageDeliveryFailed: the durable inbox row is
                            // already written, so only the live push was lost.
                            tracing::warn!(
                                agent_id = %id,
                                "Group send: agent inbox full — live delivery skipped for this recipient"
                            );
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => {
                            tracing::debug!(
                                agent_id = %id,
                                "Group send: agent inbox closed — live delivery dropped for disconnected agent"
                            );
                        }
                    }
                }
            }
        }

        // Release locks before notifying
        drop(inboxes);

        self.push_history(message).await;
        self.notify(
            EventType::BroadcastReceived,
            EventSeverity::Info,
            serde_json::json!({
                "from_agent": from.to_string(),
                "group_id": group_id.to_string(),
                "recipient_count": count,
                // No message_id: one bus id covers N distinct inbox rows, so it
                // would resolve to "not found" via agent-messages-read. Carry
                // the body preview instead (same as DirectMessageReceived).
                "message_content": preview,
            }),
        )
        .await;
        Ok(count)
    }

    /// Create a named group of agents. Re-creating an existing name replaces it.
    ///
    /// Both maps are swapped under held guards: two concurrent creates of the same
    /// name would otherwise interleave and strand the loser's `GroupID` in `groups`
    /// with no name pointing at it and no way to delete it.
    pub async fn create_group(&self, group_id: GroupID, name: String, members: Vec<AgentID>) {
        let mut group_names = self.group_names.write().await;
        let mut groups = self.groups.write().await;
        if let Some(old_id) = group_names.insert(name, group_id) {
            groups.remove(&old_id);
        }
        groups.insert(group_id, members);
    }

    /// Resolve a group name to its id and members.
    pub async fn group_by_name(&self, name: &str) -> Option<(GroupID, Vec<AgentID>)> {
        let id = *self.group_names.read().await.get(name)?;
        let members = self.groups.read().await.get(&id).cloned()?;
        Some((id, members))
    }

    /// Get recent message history for an agent.
    pub async fn get_history(&self, agent_id: &AgentID, limit: usize) -> Vec<AgentMessage> {
        let groups = self.groups.read().await;
        let history = self.history.read().await;
        history
            .iter()
            .filter(|m| {
                m.from == *agent_id
                    || match m.to {
                        agentos_types::MessageTarget::Direct(to) => to == *agent_id,
                        agentos_types::MessageTarget::Broadcast => true,
                        agentos_types::MessageTarget::Group(gid) => groups
                            .get(&gid)
                            .is_some_and(|members| members.contains(agent_id)),
                        agentos_types::MessageTarget::DirectByName(_) => false,
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
    /// Returns the number of messages queued but not yet consumed by the agent.
    pub async fn pending_count(&self, agent_id: &AgentID) -> usize {
        let inboxes = self.inboxes.read().await;
        if let Some(tx) = inboxes.get(agent_id) {
            return tx.max_capacity() - tx.capacity();
        }
        0
    }

    /// Append a message to history, capping total retained entries at MAX_HISTORY.
    /// Oldest entries are discarded when the cap is exceeded.
    async fn push_history(&self, message: AgentMessage) {
        let mut history = self.history.write().await;
        history.push_back(message);
        while history.len() > MAX_HISTORY {
            history.pop_front();
        }
    }
}

// Ensure the bus matches the implementation
impl Default for AgentMessageBus {
    fn default() -> Self {
        Self::new()
    }
}

/// Body preview for `DirectMessageReceived`. `trigger_prompt` renders it under
/// `Message:` so the recipient answers in one inference instead of spending a
/// tool round-trip on `agent-messages-read`. Text only; structured payloads
/// stay behind the read. Capped so the event (and its inbox copy) stays small.
const CONTENT_PREVIEW_CHARS: usize = 400;

fn content_preview(content: &MessageContent) -> serde_json::Value {
    match content {
        MessageContent::Text(t) if t.chars().count() <= CONTENT_PREVIEW_CHARS => t.as_str().into(),
        MessageContent::Text(t) => {
            let head: String = t.chars().take(CONTENT_PREVIEW_CHARS).collect();
            format!("{head}...(truncated)").into()
        }
        _ => serde_json::Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::{EventSeverity, EventType};
    use agentos_types::{MessageID, MessageTarget, TraceID};
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
        bus.register_pubkey_internal(agent_a, pk_a).await.unwrap();

        let msg = make_signed_msg(
            &sk_a,
            agent_a,
            MessageTarget::Direct(agent_b),
            "Hello from A",
            60,
        );
        bus.send_direct(msg, 0).await.unwrap();

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
        bus.register_pubkey_internal(a, pk_a).await.unwrap();

        let msg = make_signed_msg(&sk_a, a, MessageTarget::Broadcast, "Hello all", 60);
        let count = bus.broadcast(msg).await.unwrap();
        assert_eq!(count, 2); // b and c, not a

        assert!(inbox_b.try_recv().is_ok());
        assert!(inbox_c.try_recv().is_ok());
    }

    #[tokio::test]
    async fn test_message_without_live_listener_still_delivers() {
        // Durable delivery is the SQLite inbox; the mpsc inbox is only a live-listener
        // fast path. No registered inbox must NOT fail the send, or agent-message is
        // broken for every agent (nothing registers an mpsc inbox in production).
        let bus = AgentMessageBus::new();
        let from = AgentID::new();
        let to = AgentID::new();
        let (sk, pk) = make_keypair();
        bus.register_pubkey_internal(from, pk).await.unwrap();

        let msg = make_signed_msg(&sk, from, MessageTarget::Direct(to), "ping", 60);
        assert!(bus.send_direct(msg, 0).await.is_ok());
        assert_eq!(bus.get_history(&to, 10).await.len(), 1);
    }

    #[tokio::test]
    async fn test_group_send_by_name_counts_every_member() {
        let bus = AgentMessageBus::new();
        let from = AgentID::new();
        let member = AgentID::new();
        let outsider = AgentID::new();
        let (sk, pk) = make_keypair();
        bus.register_pubkey_internal(from, pk).await.unwrap();

        let gid = GroupID::new();
        bus.create_group(gid, "team".to_string(), vec![from, member])
            .await;
        let (resolved, members) = bus.group_by_name("team").await.expect("group by name");
        assert_eq!(resolved, gid);
        assert_eq!(members.len(), 2);

        // No mpsc listener registered: the count must still be the member count
        // minus the sender, because durable delivery is the SQLite inbox.
        let msg = make_signed_msg(&sk, from, MessageTarget::Group(gid), "standup", 60);
        assert_eq!(bus.send_to_group(&gid, &members, msg).await.unwrap(), 1);

        assert_eq!(bus.get_history(&member, 10).await.len(), 1);
        assert!(bus.get_history(&outsider, 10).await.is_empty());
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

        let result = bus.send_direct(msg, 0).await;
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

        let result = bus.send_direct(msg, 0).await;
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
        bus.register_pubkey_internal(agent_a, pk_a).await.unwrap();

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

        let result = bus.send_direct(msg, 0).await;
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
        bus.register_pubkey_internal(agent_a, pk_a).await.unwrap();

        let msg = make_signed_msg(
            &sk_a,
            agent_a,
            MessageTarget::Direct(agent_b),
            "signed msg",
            60,
        );
        bus.send_direct(msg, 0).await.unwrap();

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
        bus.register_pubkey_internal(agent_a, pk_a).await.unwrap();

        let msg = make_signed_msg(
            &sk_a,
            agent_a,
            MessageTarget::Direct(agent_b),
            "event test",
            60,
        );
        bus.send_direct(msg, 0).await.unwrap();

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
        assert_eq!(notif.payload["message_content"], "event test");
    }

    #[test]
    fn content_preview_truncates_text_and_skips_structured() {
        let short = MessageContent::Text("hi".into());
        assert_eq!(content_preview(&short), "hi");

        let long = MessageContent::Text("é".repeat(CONTENT_PREVIEW_CHARS + 5));
        let v = content_preview(&long);
        let s = v.as_str().unwrap();
        assert!(s.ends_with("...(truncated)"));
        assert_eq!(
            s.chars().count(),
            CONTENT_PREVIEW_CHARS + "...(truncated)".chars().count()
        );

        let structured = MessageContent::Structured(serde_json::json!({"k": "v"}));
        assert!(content_preview(&structured).is_null());
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
        bus.register_pubkey_internal(a, pk_a).await.unwrap();

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
    async fn test_delivery_without_listener_emits_received_event() {
        let bus = AgentMessageBus::new();
        let (notif_tx, mut notif_rx) = mpsc::channel(64);
        bus.set_notification_sender(notif_tx).await;

        let from = AgentID::new();
        let (sk, pk) = make_keypair();
        bus.register_pubkey_internal(from, pk).await.unwrap();

        let msg = make_signed_msg(&sk, from, MessageTarget::Direct(AgentID::new()), "fail", 60);
        assert!(bus.send_direct(msg, 0).await.is_ok());

        let notif = notif_rx
            .try_recv()
            .expect("should receive DirectMessageReceived notification");
        assert_eq!(notif.event_type, EventType::DirectMessageReceived);
        assert_eq!(notif.severity, EventSeverity::Info);
        assert!(!notif.payload["live_listener"].as_bool().unwrap());
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
        bus.register_pubkey_internal(agent_a, pk_a.clone())
            .await
            .unwrap();

        let msg = make_signed_msg(
            &sk_a,
            agent_a,
            MessageTarget::Direct(agent_b),
            "no sender",
            60,
        );
        bus.send_direct(msg, 0).await.unwrap();
        assert!(inbox_b.try_recv().is_ok());

        let msg = make_signed_msg(&sk_a, agent_a, MessageTarget::Broadcast, "no sender bc", 60);
        bus.broadcast(msg).await.unwrap();
    }

    // ── register_pubkey_internal immutability tests ──────────────────────────

    #[tokio::test]
    async fn test_register_pubkey_internal_first_time_succeeds() {
        let bus = AgentMessageBus::new();
        let agent_a = AgentID::new();
        let (_sk, pk) = make_keypair();

        let result = bus.register_pubkey_internal(agent_a, pk).await;
        assert!(result.is_ok(), "First registration must succeed");
    }

    #[tokio::test]
    async fn test_register_pubkey_internal_same_key_idempotent() {
        let bus = AgentMessageBus::new();
        let agent_a = AgentID::new();
        let (_sk, pk) = make_keypair();

        bus.register_pubkey_internal(agent_a, pk.clone())
            .await
            .unwrap();
        // Second call with the same key must succeed (reconnect scenario).
        let result = bus.register_pubkey_internal(agent_a, pk).await;
        assert!(
            result.is_ok(),
            "Re-registration with same key must be idempotent"
        );
    }

    #[tokio::test]
    async fn test_register_pubkey_internal_different_key_rejected() {
        let bus = AgentMessageBus::new();
        let agent_a = AgentID::new();
        let (_sk1, pk1) = make_keypair();
        let (_sk2, pk2) = make_keypair();

        bus.register_pubkey_internal(agent_a, pk1).await.unwrap();
        let result = bus.register_pubkey_internal(agent_a, pk2).await;
        assert!(
            result.is_err(),
            "Re-registration with a DIFFERENT key must be rejected"
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("already registered"),
            "Error message must mention 'already registered'"
        );
    }

    #[tokio::test]
    async fn test_deregister_pubkey_allows_reregistration() {
        let bus = AgentMessageBus::new();
        let agent_a = AgentID::new();
        let (_sk1, pk1) = make_keypair();
        let (_sk2, pk2) = make_keypair();

        bus.register_pubkey_internal(agent_a, pk1).await.unwrap();
        // After deregistering, a new key can be registered.
        bus.deregister_pubkey(&agent_a).await;
        let result = bus.register_pubkey_internal(agent_a, pk2).await;
        assert!(
            result.is_ok(),
            "Re-registration must succeed after deregister"
        );
    }
}
