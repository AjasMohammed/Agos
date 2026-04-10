use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{interval, Duration};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::ChannelAdapter;

/// Health status of a single channel adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthStatus {
    Ok,
    Degraded(String),
    Down(String),
    Unconfigured,
}

/// Health snapshot for a single channel.
#[derive(Debug, Clone)]
pub struct ChannelHealthReport {
    pub channel_id: String,
    pub status: HealthStatus,
    pub latency_ms: Option<u64>,
    pub last_check: DateTime<Utc>,
}

/// Runs periodic health checks across all registered channel adapters.
pub struct ChannelHealthMonitor {
    reports: Arc<RwLock<HashMap<String, ChannelHealthReport>>>,
}

impl ChannelHealthMonitor {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            reports: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Spawn the background health check loop.
    /// Checks every `interval_secs` seconds until cancellation.
    pub fn start(
        self: Arc<Self>,
        adapters: Vec<Arc<dyn ChannelAdapter>>,
        interval_secs: u64,
        cancel: CancellationToken,
    ) {
        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(interval_secs));
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = ticker.tick() => {
                        self.run_checks(&adapters).await;
                    }
                }
            }
        });
    }

    async fn run_checks(&self, adapters: &[Arc<dyn ChannelAdapter>]) {
        for adapter in adapters {
            let id = adapter.name().to_string();
            let start = std::time::Instant::now();
            let health = adapter.health_check().await;
            let latency_ms = start.elapsed().as_millis() as u64;

            let status = match health {
                crate::ChannelHealth::Connected => HealthStatus::Ok,
                crate::ChannelHealth::Degraded(msg) => HealthStatus::Degraded(msg),
                crate::ChannelHealth::Disconnected(msg) => {
                    warn!(channel_id = %id, reason = %msg, "Channel health check failed");
                    HealthStatus::Down(msg)
                }
            };

            self.reports.write().await.insert(
                id.clone(),
                ChannelHealthReport {
                    channel_id: id,
                    status,
                    latency_ms: Some(latency_ms),
                    last_check: Utc::now(),
                },
            );
        }
    }

    /// Get the latest health report for all channels.
    pub async fn get_all(&self) -> Vec<ChannelHealthReport> {
        self.reports.read().await.values().cloned().collect()
    }

    /// Get the health report for a specific channel.
    pub async fn get(&self, channel_id: &str) -> Option<ChannelHealthReport> {
        self.reports.read().await.get(channel_id).cloned()
    }

    /// Force an immediate health check on all adapters.
    pub async fn probe(&self, adapters: &[Arc<dyn ChannelAdapter>]) {
        self.run_checks(adapters).await;
    }
}

impl Default for ChannelHealthMonitor {
    fn default() -> Self {
        Self {
            reports: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}
