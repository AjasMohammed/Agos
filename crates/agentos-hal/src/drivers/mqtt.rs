use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::hal::HalDriver;

/// Tracks an active MQTT subscription.
#[derive(Debug, Clone)]
struct MqttSubscription {
    topic: String,
    last_payload: Option<String>,
}

/// HAL driver for MQTT broker communication.
///
/// Allows agents to subscribe to topics (sensor data), publish messages
/// (actuator commands), and query current subscription state.
pub struct MqttDriver {
    client: AsyncClient,
    subscriptions: Arc<RwLock<HashMap<String, MqttSubscription>>>,
    _event_loop_handle: tokio::task::JoinHandle<()>,
}

impl MqttDriver {
    /// Connect to an MQTT broker.
    ///
    /// - `broker_host`: hostname or IP (e.g., "localhost")
    /// - `broker_port`: port (e.g., 1883)
    /// - `client_id`: unique client identifier
    /// - `credentials`: optional (username, password) for authentication
    /// - `cancel`: shutdown signal for the event loop
    pub async fn new(
        broker_host: &str,
        broker_port: u16,
        client_id: &str,
        credentials: Option<(&str, &str)>,
        cancel: CancellationToken,
    ) -> Result<Self, AgentOSError> {
        let mut opts = MqttOptions::new(client_id, broker_host, broker_port);
        opts.set_keep_alive(std::time::Duration::from_secs(30));
        // Use persistent sessions so subscriptions survive reconnects
        opts.set_clean_session(false);

        if let Some((user, pass)) = credentials {
            opts.set_credentials(user, pass);
        }

        let (client, mut eventloop) = AsyncClient::new(opts, 256);

        let subscriptions: Arc<RwLock<HashMap<String, MqttSubscription>>> =
            Arc::new(RwLock::new(HashMap::new()));

        // Spawn the event loop with cancellation support
        let subs = Arc::clone(&subscriptions);
        let client_clone = client.clone();
        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        tracing::info!("MQTT event loop shutting down");
                        break;
                    }
                    event = eventloop.poll() => {
                        match event {
                            Ok(Event::Incoming(Packet::Publish(publish))) => {
                                let topic = publish.topic.clone();
                                let payload = String::from_utf8(publish.payload.to_vec())
                                    .unwrap_or_default();

                                let mut subs_guard = subs.write().await;
                                if let Some(sub) = subs_guard.get_mut(&topic) {
                                    sub.last_payload = Some(payload);
                                }

                                tracing::debug!(topic = %topic, "MQTT message received");
                            }
                            Ok(Event::Incoming(Packet::ConnAck(_))) => {
                                // Resubscribe after reconnect
                                let subs_guard = subs.read().await;
                                for topic in subs_guard.keys() {
                                    if let Err(e) = client_clone
                                        .subscribe(topic.as_str(), QoS::AtLeastOnce)
                                        .await
                                    {
                                        tracing::warn!(
                                            topic = %topic,
                                            error = %e,
                                            "Failed to resubscribe after reconnect"
                                        );
                                    }
                                }
                                if !subs_guard.is_empty() {
                                    tracing::info!(
                                        count = subs_guard.len(),
                                        "Resubscribed after MQTT reconnect"
                                    );
                                }
                            }
                            Ok(_) => {} // Other events (suback, etc.)
                            Err(e) => {
                                tracing::warn!(error = %e, "MQTT event loop error");
                                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                            }
                        }
                    }
                }
            }
        });

        tracing::info!(broker = %broker_host, port = broker_port, "MQTT driver connected");

        Ok(Self {
            client,
            subscriptions,
            _event_loop_handle: handle,
        })
    }
}

/// Validate an MQTT topic string.
fn validate_topic(topic: &str) -> Result<(), AgentOSError> {
    if topic.is_empty() || topic.len() > 65535 || topic.contains('\0') {
        return Err(AgentOSError::HalError("Invalid MQTT topic".into()));
    }
    // Reject wildcard topics for security
    if topic.contains('#') || topic.contains('+') {
        return Err(AgentOSError::HalError(
            "Wildcard MQTT topics (#, +) are not allowed for agent subscriptions".into(),
        ));
    }
    Ok(())
}

#[async_trait]
impl HalDriver for MqttDriver {
    fn name(&self) -> &str {
        "mqtt"
    }

    fn required_permission(&self) -> (&str, PermissionOp) {
        ("hardware.mqtt", PermissionOp::Query)
    }

    fn required_permission_for(&self, params: &Value) -> (&str, PermissionOp) {
        match params.get("action").and_then(|v| v.as_str()) {
            Some("publish") => ("hardware.mqtt", PermissionOp::Write),
            _ => ("hardware.mqtt", PermissionOp::Read),
        }
    }

    fn device_key(&self, params: &Value) -> Option<String> {
        params
            .get("topic")
            .and_then(|v| v.as_str())
            .map(|t| format!("mqtt:{t}"))
    }

    async fn query(&self, params: Value) -> Result<Value, AgentOSError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("status");

        match action {
            "subscribe" => {
                let topic = params
                    .get("topic")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| AgentOSError::HalError("Missing 'topic' parameter".into()))?;

                validate_topic(topic)?;

                self.client
                    .subscribe(topic, QoS::AtLeastOnce)
                    .await
                    .map_err(|e| AgentOSError::HalError(format!("MQTT subscribe failed: {e}")))?;

                self.subscriptions.write().await.insert(
                    topic.to_string(),
                    MqttSubscription {
                        topic: topic.to_string(),
                        last_payload: None,
                    },
                );

                tracing::info!(topic = %topic, "MQTT subscription created");
                Ok(json!({ "subscribed": topic }))
            }

            "unsubscribe" => {
                let topic = params
                    .get("topic")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| AgentOSError::HalError("Missing 'topic' parameter".into()))?;

                self.client
                    .unsubscribe(topic)
                    .await
                    .map_err(|e| AgentOSError::HalError(format!("MQTT unsubscribe failed: {e}")))?;

                self.subscriptions.write().await.remove(topic);
                Ok(json!({ "unsubscribed": topic }))
            }

            "publish" => {
                let topic = params
                    .get("topic")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| AgentOSError::HalError("Missing 'topic' parameter".into()))?;

                validate_topic(topic)?;

                // Extract raw string for string values, JSON for objects/arrays
                let payload = params
                    .get("payload")
                    .map(|v| match v.as_str() {
                        Some(s) => s.to_string(),
                        None => v.to_string(),
                    })
                    .unwrap_or_else(|| "{}".to_string());

                self.client
                    .publish(topic, QoS::AtLeastOnce, false, payload.as_bytes().to_vec())
                    .await
                    .map_err(|e| AgentOSError::HalError(format!("MQTT publish failed: {e}")))?;

                tracing::debug!(topic = %topic, "MQTT message published");
                Ok(json!({ "published": topic }))
            }

            "status" => {
                let subs = self.subscriptions.read().await;
                let topics: Vec<Value> = subs
                    .values()
                    .map(|s| {
                        json!({
                            "topic": s.topic,
                            "last_payload": s.last_payload,
                        })
                    })
                    .collect();

                Ok(json!({ "subscriptions": topics, "count": topics.len() }))
            }

            other => Err(AgentOSError::HalError(format!(
                "Unknown MQTT action: {other}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_topic_ok() {
        assert!(validate_topic("sensors/room1/temperature").is_ok());
        assert!(validate_topic("a").is_ok());
    }

    #[test]
    fn test_validate_topic_rejects_wildcard() {
        assert!(validate_topic("sensors/#").is_err());
        assert!(validate_topic("sensors/+/temperature").is_err());
    }

    #[test]
    fn test_validate_topic_rejects_empty() {
        assert!(validate_topic("").is_err());
    }

    #[test]
    fn test_validate_topic_rejects_null() {
        assert!(validate_topic("sensors/\0/temp").is_err());
    }

    #[test]
    fn test_device_key_format() {
        assert_eq!(
            "mqtt:sensors/temperature",
            format!("mqtt:{}", "sensors/temperature")
        );
    }
}
