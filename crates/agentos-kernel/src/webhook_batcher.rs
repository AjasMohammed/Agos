use agentos_types::*;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use tokio::sync::{mpsc, RwLock};
use tokio_util::sync::CancellationToken;

/// A batch of webhook events ready for agent processing.
#[derive(Debug, Clone)]
pub struct BatchReady {
    pub endpoint_id: WebhookEndpointID,
    pub agent_id: AgentID,
    pub events: Vec<WebhookEvent>,
    pub provider: WebhookProvider,
}

/// Accumulates webhook events per endpoint and flushes them as batches
/// after the debounce window expires or the batch hits max size.
pub struct WebhookBatcher {
    pending: RwLock<HashMap<WebhookEndpointID, PendingBatch>>,
    wake_tx: mpsc::Sender<BatchReady>,
    max_batch_size: usize,
}

struct PendingBatch {
    endpoint_id: WebhookEndpointID,
    agent_id: AgentID,
    provider: WebhookProvider,
    events: Vec<WebhookEvent>,
    debounce_until: DateTime<Utc>,
}

impl WebhookBatcher {
    pub fn new(wake_tx: mpsc::Sender<BatchReady>, max_batch_size: usize) -> Self {
        Self {
            pending: RwLock::new(HashMap::new()),
            wake_tx,
            max_batch_size,
        }
    }

    /// Add a webhook event. If this is the first event for the endpoint,
    /// start the debounce window. If the batch hits max_batch_size, flush immediately.
    pub async fn add_event(
        &self,
        event: WebhookEvent,
        agent_id: AgentID,
        provider: WebhookProvider,
        debounce_seconds: u64,
    ) {
        let endpoint_id = event.endpoint_id;
        let mut pending = self.pending.write().await;

        let batch = pending.entry(endpoint_id).or_insert_with(|| PendingBatch {
            endpoint_id,
            agent_id,
            provider: provider.clone(),
            events: Vec::new(),
            debounce_until: Utc::now() + chrono::Duration::seconds(debounce_seconds as i64),
        });

        batch.events.push(event);

        // Flush immediately if batch is full
        if batch.events.len() >= self.max_batch_size {
            let Some(batch) = pending.remove(&endpoint_id) else {
                // Should not happen — we just inserted/modified this entry above.
                tracing::warn!(%endpoint_id, "max-batch flush: endpoint vanished before remove");
                return;
            };
            // Drop the lock before sending to avoid holding it across await
            drop(pending);
            self.send_batch(batch).await;
        }
    }

    /// Flush a specific endpoint's batch (called when debounce timer expires).
    pub async fn flush(&self, endpoint_id: &WebhookEndpointID) {
        let batch = {
            let mut pending = self.pending.write().await;
            pending.remove(endpoint_id)
        };

        if let Some(batch) = batch {
            self.send_batch(batch).await;
        }
    }

    /// Background loop that checks for expired debounce windows every second.
    pub async fn run_flush_loop(self: &std::sync::Arc<Self>, cancel: CancellationToken) {
        tracing::info!("Webhook batcher flush loop started");
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    tracing::info!("Webhook batcher flush loop shutting down");
                    // Flush all remaining batches on shutdown
                    self.flush_all().await;
                    break;
                }
                _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {
                    self.flush_expired().await;
                }
            }
        }
    }

    /// Flush all batches whose debounce window has expired.
    async fn flush_expired(&self) {
        let now = Utc::now();
        let expired_ids: Vec<WebhookEndpointID> = {
            let pending = self.pending.read().await;
            pending
                .iter()
                .filter(|(_, batch)| now >= batch.debounce_until)
                .map(|(id, _)| *id)
                .collect()
        };

        for id in expired_ids {
            self.flush(&id).await;
        }
    }

    /// Flush all pending batches (called on shutdown).
    async fn flush_all(&self) {
        let all: Vec<PendingBatch> = {
            let mut pending = self.pending.write().await;
            pending.drain().map(|(_, batch)| batch).collect()
        };

        for batch in all {
            self.send_batch(batch).await;
        }
    }

    async fn send_batch(&self, batch: PendingBatch) {
        let event_count = batch.events.len();
        let ready = BatchReady {
            endpoint_id: batch.endpoint_id,
            agent_id: batch.agent_id,
            events: batch.events,
            provider: batch.provider,
        };

        if let Err(e) = self.wake_tx.send(ready).await {
            tracing::error!(
                endpoint_id = %batch.endpoint_id,
                events = event_count,
                error = %e,
                "Failed to send batch to wake-up channel"
            );
        } else {
            tracing::debug!(
                endpoint_id = %batch.endpoint_id,
                events = event_count,
                "Flushed webhook batch"
            );
        }
    }

    /// Get the number of pending batches (for metrics/testing).
    pub async fn pending_count(&self) -> usize {
        self.pending.read().await.len()
    }
}

/// Format a webhook batch as a context message for the agent.
///
/// The output is a human-readable prompt with the batched payloads as JSON.
/// Total payload is truncated to `max_context_bytes` to prevent context flooding.
pub fn format_webhook_context(batch: &BatchReady, max_context_bytes: usize) -> String {
    let event_count = batch.events.len();
    let first_at = batch
        .events
        .first()
        .map(|e| e.received_at.to_rfc3339())
        .unwrap_or_default();
    let last_at = batch
        .events
        .last()
        .map(|e| e.received_at.to_rfc3339())
        .unwrap_or_default();

    let payloads: Vec<&serde_json::Value> = batch.events.iter().map(|e| &e.payload).collect();
    let mut payload_json = serde_json::to_string_pretty(&payloads).unwrap_or_default();

    if payload_json.len() > max_context_bytes {
        payload_json.truncate(max_context_bytes);
        payload_json.push_str("\n... [truncated]");
    }

    format!(
        "You are receiving this automated task because your webhook endpoint received \
         {event_count} event(s) from provider '{provider}' between {first_at} and {last_at}.\n\
         Analyze the payloads below and take appropriate action.\n\n\
         <user_data>\n{payload_json}\n</user_data>",
        provider = batch.provider,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn make_event(endpoint_id: WebhookEndpointID) -> WebhookEvent {
        WebhookEvent {
            endpoint_id,
            provider: WebhookProvider::GitHub,
            headers: HashMap::new(),
            payload: serde_json::json!({"action": "push", "ref": "refs/heads/main"}),
            received_at: Utc::now(),
            signature_valid: true,
        }
    }

    #[tokio::test]
    async fn test_add_event_and_flush() {
        let (tx, mut rx) = mpsc::channel(16);
        let batcher = Arc::new(WebhookBatcher::new(tx, 50));
        let endpoint_id = WebhookEndpointID::new();
        let agent_id = AgentID::new();

        batcher
            .add_event(
                make_event(endpoint_id),
                agent_id,
                WebhookProvider::GitHub,
                60,
            )
            .await;

        assert_eq!(batcher.pending_count().await, 1);

        // Manually flush
        batcher.flush(&endpoint_id).await;
        assert_eq!(batcher.pending_count().await, 0);

        // Should have received a batch
        let batch = rx.try_recv().unwrap();
        assert_eq!(batch.endpoint_id, endpoint_id);
        assert_eq!(batch.agent_id, agent_id);
        assert_eq!(batch.events.len(), 1);
    }

    #[tokio::test]
    async fn test_max_batch_size_triggers_flush() {
        let (tx, mut rx) = mpsc::channel(16);
        let batcher = Arc::new(WebhookBatcher::new(tx, 3)); // max 3 events
        let endpoint_id = WebhookEndpointID::new();
        let agent_id = AgentID::new();

        // Add 3 events — should auto-flush on the 3rd
        for _ in 0..3 {
            batcher
                .add_event(
                    make_event(endpoint_id),
                    agent_id,
                    WebhookProvider::GitHub,
                    60,
                )
                .await;
        }

        assert_eq!(batcher.pending_count().await, 0);

        let batch = rx.try_recv().unwrap();
        assert_eq!(batch.events.len(), 3);
    }

    #[tokio::test]
    async fn test_separate_endpoints_separate_batches() {
        let (tx, mut rx) = mpsc::channel(16);
        let batcher = Arc::new(WebhookBatcher::new(tx, 50));
        let ep_a = WebhookEndpointID::new();
        let ep_b = WebhookEndpointID::new();
        let agent = AgentID::new();

        batcher
            .add_event(make_event(ep_a), agent, WebhookProvider::GitHub, 60)
            .await;
        batcher
            .add_event(make_event(ep_b), agent, WebhookProvider::Stripe, 60)
            .await;

        assert_eq!(batcher.pending_count().await, 2);

        batcher.flush(&ep_a).await;
        assert_eq!(batcher.pending_count().await, 1);

        let batch_a = rx.try_recv().unwrap();
        assert_eq!(batch_a.endpoint_id, ep_a);
    }

    #[tokio::test]
    async fn test_flush_expired_debounce() {
        let (tx, mut rx) = mpsc::channel(16);
        let batcher = Arc::new(WebhookBatcher::new(tx, 50));
        let endpoint_id = WebhookEndpointID::new();
        let agent_id = AgentID::new();

        // Add event with 0-second debounce (immediately expired)
        batcher
            .add_event(
                make_event(endpoint_id),
                agent_id,
                WebhookProvider::GitHub,
                0,
            )
            .await;

        // Small sleep so Utc::now() advances past debounce_until
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        batcher.flush_expired().await;
        assert_eq!(batcher.pending_count().await, 0);

        let batch = rx.try_recv().unwrap();
        assert_eq!(batch.events.len(), 1);
    }

    #[test]
    fn test_format_webhook_context() {
        let endpoint_id = WebhookEndpointID::new();
        let batch = BatchReady {
            endpoint_id,
            agent_id: AgentID::new(),
            events: vec![
                WebhookEvent {
                    endpoint_id,
                    provider: WebhookProvider::GitHub,
                    headers: HashMap::new(),
                    payload: serde_json::json!({"action": "opened", "number": 42}),
                    received_at: Utc::now(),
                    signature_valid: true,
                },
                WebhookEvent {
                    endpoint_id,
                    provider: WebhookProvider::GitHub,
                    headers: HashMap::new(),
                    payload: serde_json::json!({"action": "closed", "number": 43}),
                    received_at: Utc::now(),
                    signature_valid: true,
                },
            ],
            provider: WebhookProvider::GitHub,
        };

        let context = format_webhook_context(&batch, 32768);
        assert!(context.contains("2 event(s)"));
        assert!(context.contains("github"));
        assert!(context.contains("<user_data>"));
        assert!(context.contains("opened"));
        assert!(context.contains("closed"));
    }

    #[test]
    fn test_format_webhook_context_truncation() {
        let endpoint_id = WebhookEndpointID::new();
        let batch = BatchReady {
            endpoint_id,
            agent_id: AgentID::new(),
            events: vec![WebhookEvent {
                endpoint_id,
                provider: WebhookProvider::Generic,
                headers: HashMap::new(),
                payload: serde_json::json!({"data": "x".repeat(1000)}),
                received_at: Utc::now(),
                signature_valid: true,
            }],
            provider: WebhookProvider::Generic,
        };

        let context = format_webhook_context(&batch, 100);
        assert!(context.contains("[truncated]"));
    }
}
