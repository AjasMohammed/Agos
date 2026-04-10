use crate::runtime::ComputeRuntime;
use chrono::Utc;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Background service that destroys containers whose TTL has expired.
///
/// Prevents zombie containers from accumulating if agents forget to clean up.
pub struct ContainerReaper {
    runtime: Arc<dyn ComputeRuntime>,
    cancel: CancellationToken,
    /// How often to check for expired containers (default: 60s).
    check_interval: std::time::Duration,
}

impl ContainerReaper {
    pub fn new(runtime: Arc<dyn ComputeRuntime>, cancel: CancellationToken) -> Self {
        Self {
            runtime,
            cancel,
            check_interval: std::time::Duration::from_secs(60),
        }
    }

    /// Spawn the reaper as a background tokio task.
    pub fn start(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            tracing::info!("Container reaper started");
            loop {
                tokio::select! {
                    _ = self.cancel.cancelled() => {
                        tracing::info!("Container reaper shutting down");
                        break;
                    }
                    _ = tokio::time::sleep(self.check_interval) => {
                        self.sweep_expired().await;
                    }
                }
            }
        })
    }

    async fn sweep_expired(&self) {
        let containers = match self.runtime.list().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "Reaper failed to list containers");
                return;
            }
        };

        let now = Utc::now();
        for c in containers {
            if c.expires_at <= now {
                tracing::warn!(
                    container_id = %c.id,
                    image = %c.image,
                    agent_id = %c.agent_id,
                    "Reaping expired container"
                );
                if let Err(e) = self.runtime.destroy(&c.id).await {
                    tracing::error!(
                        container_id = %c.id,
                        error = %e,
                        "Failed to reap expired container"
                    );
                }
            }
        }
    }
}
