use agentos_types::{AgentID, AgentMessage, AgentOSError, GroupID};
use std::collections::HashMap;
use tokio::sync::mpsc;
use tokio::sync::RwLock;

pub struct AgentMessageBus {
    /// Per-agent message channels. Each agent has an inbox.
    inboxes: RwLock<HashMap<AgentID, mpsc::UnboundedSender<AgentMessage>>>,
    /// Agent group memberships.
    groups: RwLock<HashMap<GroupID, Vec<AgentID>>>,
    /// Message history for audit and retrieval.
    history: RwLock<Vec<AgentMessage>>,
}

impl AgentMessageBus {
    pub fn new() -> Self {
        Self {
            inboxes: RwLock::new(HashMap::new()),
            groups: RwLock::new(HashMap::new()),
            history: RwLock::new(Vec::new()),
        }
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
        let agent_id = match message.to {
            agentos_types::MessageTarget::Direct(id) => id,
            _ => {
                return Err(AgentOSError::KernelError {
                    reason: "Invalid target for send_direct".into(),
                })
            }
        };

        let inboxes = self.inboxes.read().await;
        if let Some(tx) = inboxes.get(&agent_id) {
            let _ = tx.send(message.clone());
            self.history.write().await.push(message);
            Ok(())
        } else {
            Err(AgentOSError::AgentNotFound(agent_id.to_string()))
        }
    }

    /// Broadcast a message to all connected agents (except sender).
    pub async fn broadcast(&self, message: AgentMessage) -> Result<u32, AgentOSError> {
        let mut count = 0;
        let inboxes = self.inboxes.read().await;

        for (id, tx) in inboxes.iter() {
            if *id != message.from {
                let _ = tx.send(message.clone());
                count += 1;
            }
        }

        self.history.write().await.push(message);
        Ok(count)
    }

    /// Send to a group.
    pub async fn send_to_group(
        &self,
        group_id: &GroupID,
        message: AgentMessage,
    ) -> Result<u32, AgentOSError> {
        let groups = self.groups.read().await;
        let members = groups
            .get(group_id)
            .ok_or_else(|| AgentOSError::KernelError {
                reason: format!("Group {} not found", group_id),
            })?;

        let mut count = 0;
        let inboxes = self.inboxes.read().await;

        for &id in members {
            if id != message.from {
                if let Some(tx) = inboxes.get(&id) {
                    let _ = tx.send(message.clone());
                    count += 1;
                }
            }
        }

        self.history.write().await.push(message);
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
    use agentos_types::{MessageContent, MessageID, MessageTarget, TraceID};

    #[tokio::test]
    async fn test_direct_message_delivery() {
        let bus = AgentMessageBus::new();
        let agent_a = AgentID::new();
        let agent_b = AgentID::new();

        let mut inbox_b = bus.register_agent(agent_b).await;
        bus.register_agent(agent_a).await;

        let msg = AgentMessage {
            id: MessageID::new(),
            from: agent_a,
            to: MessageTarget::Direct(agent_b),
            content: MessageContent::Text("Hello from A".into()),
            reply_to: None,
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
        };

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

        bus.register_agent(a).await;
        let mut inbox_b = bus.register_agent(b).await;
        let mut inbox_c = bus.register_agent(c).await;

        let msg = AgentMessage {
            id: MessageID::new(),
            from: a,
            to: MessageTarget::Broadcast,
            content: MessageContent::Text("Hello all".into()),
            reply_to: None,
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
        };

        let count = bus.broadcast(msg).await.unwrap();
        assert_eq!(count, 2); // b and c, not a

        assert!(inbox_b.try_recv().is_ok());
        assert!(inbox_c.try_recv().is_ok());
    }

    #[tokio::test]
    async fn test_message_to_nonexistent_agent_fails() {
        let bus = AgentMessageBus::new();
        let msg = AgentMessage {
            id: MessageID::new(),
            from: AgentID::new(),
            to: MessageTarget::Direct(AgentID::new()),
            content: MessageContent::Text("ping".into()),
            reply_to: None,
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
        };
        assert!(bus.send_direct(msg).await.is_err());
    }
}
