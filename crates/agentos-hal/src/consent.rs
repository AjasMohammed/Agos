use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

/// In-memory consent grants keyed by `(agent_id, resource)`.
///
/// This store is intentionally lightweight and process-local. It is used by
/// privacy-sensitive HAL drivers (webcam, audio) to enforce short-lived access.
pub struct ConsentStore {
    grants: RwLock<HashMap<(String, String), Instant>>,
}

impl Default for ConsentStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ConsentStore {
    pub fn new() -> Self {
        Self {
            grants: RwLock::new(HashMap::new()),
        }
    }

    pub fn check(&self, agent_id: &str, resource: &str) -> bool {
        self.prune_expired();
        self.grants
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&(agent_id.to_string(), resource.to_string()))
            .map(|expires_at| Instant::now() < *expires_at)
            .unwrap_or(false)
    }

    pub fn grant(&self, agent_id: &str, resource: &str, ttl: Duration) {
        self.grants
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                (agent_id.to_string(), resource.to_string()),
                Instant::now() + ttl,
            );
    }

    pub fn revoke(&self, agent_id: &str, resource: &str) -> bool {
        self.grants
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&(agent_id.to_string(), resource.to_string()))
            .is_some()
    }

    pub fn list(&self) -> Vec<(String, String, u64)> {
        self.prune_expired();
        let now = Instant::now();
        self.grants
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .map(|((agent_id, resource), expires_at)| {
                let remaining = expires_at.saturating_duration_since(now).as_secs();
                (agent_id.clone(), resource.clone(), remaining)
            })
            .collect()
    }

    fn prune_expired(&self) {
        let now = Instant::now();
        self.grants
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|_, expires_at| *expires_at > now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consent_ttl_allows_then_expires() {
        let store = ConsentStore::new();
        store.grant(
            "agent-a",
            "hardware.webcam.capture",
            Duration::from_millis(50),
        );
        assert!(store.check("agent-a", "hardware.webcam.capture"));
        std::thread::sleep(Duration::from_millis(65));
        assert!(!store.check("agent-a", "hardware.webcam.capture"));
    }
}
