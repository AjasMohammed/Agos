use chrono::{DateTime, Duration, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

/// Policy for handling DMs from unknown senders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DmPolicy {
    /// Unknown senders receive a pairing code (default, recommended).
    Pairing,
    /// All senders are accepted without pairing.
    Open,
    /// No DMs accepted from any sender.
    Blocked,
}

/// An allowlisted sender on a channel.
#[derive(Debug, Clone)]
pub struct AllowedSender {
    pub channel_id: String,
    pub sender_id: String,
    pub approved_at: DateTime<Utc>,
    pub label: Option<String>,
}

/// A pending pairing request (not yet approved).
#[derive(Debug, Clone)]
struct PendingPairing {
    pub channel_id: String,
    pub sender_id: String,
    #[allow(dead_code)]
    pub code: String,
    pub expires_at: DateTime<Utc>,
}

/// Manages the DM pairing allowlist across all channels.
///
/// Unknown senders receive a one-time pairing code; approved senders are
/// added to the persistent allowlist. This prevents unsolicited agents from
/// consuming compute.
pub struct PairingManager {
    allowed: RwLock<Vec<AllowedSender>>,
    pending: RwLock<HashMap<String, PendingPairing>>, // code → pairing
    code_ttl: Duration,
}

impl PairingManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            allowed: RwLock::new(Vec::new()),
            pending: RwLock::new(HashMap::new()),
            code_ttl: Duration::minutes(10),
        })
    }

    /// Returns `true` if the sender is on the allowlist for the given channel.
    pub async fn is_allowed(&self, channel_id: &str, sender_id: &str) -> bool {
        self.allowed
            .read()
            .await
            .iter()
            .any(|a| a.channel_id == channel_id && a.sender_id == sender_id)
    }

    /// Generate a 6-character alphanumeric pairing code for an unknown sender.
    /// Returns the code to send back to the user.
    ///
    /// Uses the full `[A-Z0-9]` charset (36^6 ≈ 2.18B possibilities) rather than
    /// UUID hex slices (16^6 ≈ 16.7M), making brute-force 130x harder.
    pub async fn generate_code(&self, channel_id: &str, sender_id: &str) -> String {
        // Scope `rand::thread_rng()` (which is `Rc<UnsafeCell<...>>` and
        // therefore !Send) inside its own block so the resulting future
        // does NOT hold a non-Send guard across the `.await` below.
        // Without this, callers from any `tokio::spawn` (e.g. the
        // approval inbound router) fail to compile.
        let code: String = {
            use rand::Rng;
            const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
            let mut rng = rand::thread_rng();
            (0..6)
                .map(|_| CHARSET[rng.gen_range(0..CHARSET.len())] as char)
                .collect()
        };
        let pairing = PendingPairing {
            channel_id: channel_id.to_string(),
            sender_id: sender_id.to_string(),
            code: code.clone(),
            expires_at: Utc::now() + self.code_ttl,
        };
        self.pending.write().await.insert(code.clone(), pairing);
        code
    }

    /// Approve a pairing code. Returns the approved sender on success.
    ///
    /// Returns a uniform error message whether the code is wrong, used, or expired,
    /// to prevent attackers from distinguishing between these states.
    pub async fn approve_code(&self, code: &str) -> Result<AllowedSender, String> {
        let mut pending = self.pending.write().await;
        let pairing = pending
            .remove(code)
            .ok_or_else(|| "Invalid or expired pairing code".to_string())?;

        if Utc::now() > pairing.expires_at {
            // Code existed but is expired — return same uniform error.
            return Err("Invalid or expired pairing code".to_string());
        }

        let sender = AllowedSender {
            channel_id: pairing.channel_id.clone(),
            sender_id: pairing.sender_id.clone(),
            approved_at: Utc::now(),
            label: None,
        };
        self.allowed.write().await.push(sender.clone());
        info!(
            channel_id = %pairing.channel_id,
            sender_id = %pairing.sender_id,
            "Pairing approved"
        );
        Ok(sender)
    }

    /// List all approved senders.
    pub async fn list_approved(&self) -> Vec<AllowedSender> {
        self.allowed.read().await.clone()
    }

    /// Revoke an approved sender.
    pub async fn revoke(&self, channel_id: &str, sender_id: &str) -> bool {
        let mut allowed = self.allowed.write().await;
        let before = allowed.len();
        allowed.retain(|a| !(a.channel_id == channel_id && a.sender_id == sender_id));
        let removed = before != allowed.len();
        if removed {
            info!(%channel_id, %sender_id, "Pairing revoked");
        }
        removed
    }

    /// Remove expired pending codes to prevent unbounded memory growth.
    /// Returns the number of codes swept.
    pub async fn sweep_expired(&self) -> usize {
        let now = Utc::now();
        let mut pending = self.pending.write().await;
        let before = pending.len();
        pending.retain(|_, p| p.expires_at > now);
        before - pending.len()
    }

    /// Build the reply message to send to an unknown sender.
    pub fn pairing_prompt(code: &str) -> String {
        format!(
            "Hi! This agent requires pairing. Your code is: **{}**\n\
             Reply with `/pair {}` to connect (expires in 10 minutes).",
            code, code
        )
    }

    /// Parse a `/pair <code>` command from a message. Returns `Some(code)` if matched.
    pub fn parse_pair_command(text: &str) -> Option<String> {
        let trimmed = text.trim();
        if let Some(rest) = trimmed.strip_prefix("/pair ") {
            let code = rest.trim().to_uppercase();
            if !code.is_empty() {
                return Some(code);
            }
        }
        None
    }
}

impl Default for PairingManager {
    fn default() -> Self {
        Self {
            allowed: RwLock::new(Vec::new()),
            pending: RwLock::new(HashMap::new()),
            code_ttl: Duration::minutes(10),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_unknown_sender_not_allowed() {
        let pm = PairingManager::new();
        assert!(!pm.is_allowed("discord", "user123").await);
    }

    #[tokio::test]
    async fn test_pairing_flow() {
        let pm = PairingManager::new();
        let code = pm.generate_code("discord", "user123").await;
        assert_eq!(code.len(), 6);

        let sender = pm.approve_code(&code).await.unwrap();
        assert_eq!(sender.sender_id, "user123");
        assert!(pm.is_allowed("discord", "user123").await);
    }

    #[tokio::test]
    async fn test_duplicate_code_rejected() {
        let pm = PairingManager::new();
        let code = pm.generate_code("discord", "user123").await;
        pm.approve_code(&code).await.unwrap();
        // Second use should fail.
        assert!(pm.approve_code(&code).await.is_err());
    }

    #[tokio::test]
    async fn test_revoke_removes_sender() {
        let pm = PairingManager::new();
        let code = pm.generate_code("discord", "user123").await;
        pm.approve_code(&code).await.unwrap();
        assert!(pm.revoke("discord", "user123").await);
        assert!(!pm.is_allowed("discord", "user123").await);
    }

    #[test]
    fn test_parse_pair_command() {
        assert_eq!(
            PairingManager::parse_pair_command("/pair ABC123"),
            Some("ABC123".to_string())
        );
        assert_eq!(PairingManager::parse_pair_command("hello"), None);
        assert_eq!(PairingManager::parse_pair_command("/pair "), None);
    }
}
