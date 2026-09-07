use chrono::{DateTime, Duration, Utc};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use tokio::sync::RwLock;
use tracing::info;

/// Policy for handling DMs from unknown senders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DmPolicy {
    /// Unknown senders must be paired by an operator (default, recommended).
    Pairing,
    /// All senders are accepted without pairing.
    Open,
    /// No DMs accepted from any sender.
    Blocked,
}

/// Durable storage for the approved-sender allowlist.
///
/// The allowlist gates escalation delivery *and* inbound `/approve`, so losing
/// it on restart leaves approval prompts undeliverable and unanswerable. The
/// manager hands over the full snapshot after every mutation — the list is
/// small and rarely changes, so there is nothing to gain from diffing.
#[async_trait::async_trait]
pub trait PairingPersistence: Send + Sync {
    async fn save(&self, senders: &[AllowedSender]);
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

/// Public snapshot of a pending pairing request. The 6-char code is
/// intentionally omitted: introspection (CLI list, REST) is readable by more
/// callers than may approve, so live codes are not handed out through it. The
/// code reaches the operator through the kernel log instead.
#[derive(Debug, Clone)]
pub struct PendingPairingInfo {
    pub channel_id: String,
    pub sender_id: String,
    pub expires_at: DateTime<Utc>,
}

/// Manages the DM pairing allowlist across all channels.
///
/// An unknown sender's request mints a one-time code that an operator must
/// act on; approved senders are added to the allowlist. This prevents
/// unsolicited agents from consuming compute — and, because the allowlist also
/// gates `/approve` and `/deny`, the code must never be delivered to the
/// requester, or pairing becomes self-service.
pub struct PairingManager {
    allowed: RwLock<Vec<AllowedSender>>,
    pending: RwLock<HashMap<String, PendingPairing>>, // code → pairing
    code_ttl: Duration,
    /// Sliding-window failed-guess counter `(count, window_start)`. Defense in
    /// depth against pairing-code brute force on top of the large keyspace.
    failed_guesses: RwLock<(u32, DateTime<Utc>)>,
    /// Set once at kernel boot. Absent in unit tests and in any embedding that
    /// does not care whether pairings outlive the process.
    persistence: OnceLock<Arc<dyn PairingPersistence>>,
}

/// Max failed `approve_code` guesses allowed per [`GUESS_WINDOW`] before the
/// guess path is briefly locked out. Generous enough never to impede a real
/// operator (who has the exact code) while throttling automated guessing.
const MAX_FAILED_GUESSES_PER_WINDOW: u32 = 20;

impl PairingManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            allowed: RwLock::new(Vec::new()),
            pending: RwLock::new(HashMap::new()),
            code_ttl: Duration::minutes(10),
            failed_guesses: RwLock::new((0, Utc::now())),
            persistence: OnceLock::new(),
        })
    }

    /// Attach durable storage. Call once, at boot, before `restore`.
    pub fn set_persistence(&self, store: Arc<dyn PairingPersistence>) {
        let _ = self.persistence.set(store);
    }

    /// Seed the allowlist from storage at boot. Does not write back — these
    /// senders are what storage already holds.
    pub async fn restore(&self, senders: Vec<AllowedSender>) {
        if senders.is_empty() {
            return;
        }
        info!(
            count = senders.len(),
            "Restored paired senders from storage"
        );
        *self.allowed.write().await = senders;
    }

    /// Push the current allowlist to storage. No-op when none is attached.
    /// Callers must not hold the `allowed` lock.
    async fn persist(&self) {
        let Some(store) = self.persistence.get() else {
            return;
        };
        let snapshot = self.allowed.read().await.clone();
        store.save(&snapshot).await;
    }

    /// Duration of the failed-guess rate-limit window.
    fn guess_window() -> Duration {
        Duration::minutes(1)
    }

    /// Returns `true` if the failed-guess budget for the current window is
    /// exhausted (caller should reject without consulting the code map).
    async fn guess_budget_exhausted(&self) -> bool {
        let mut g = self.failed_guesses.write().await;
        let now = Utc::now();
        if now - g.1 > Self::guess_window() {
            *g = (0, now); // new window
        }
        g.0 >= MAX_FAILED_GUESSES_PER_WINDOW
    }

    /// Record one failed guess against the current window.
    async fn record_failed_guess(&self) {
        let mut g = self.failed_guesses.write().await;
        let now = Utc::now();
        if now - g.1 > Self::guess_window() {
            *g = (1, now);
        } else {
            g.0 += 1;
        }
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
    /// Returns the code — deliver it **out of band** to an operator; never
    /// reply with it on the channel the request came from, or the requester
    /// can approve themselves.
    ///
    /// Uses the full `[A-Z0-9]` charset (36^6 ≈ 2.18B possibilities) rather than
    /// UUID hex slices (16^6 ≈ 16.7M), making brute-force 130x harder.
    ///
    /// Idempotent per `(channel_id, sender_id)`: a sender with a live request
    /// gets that same code back. Every rejected inbound message files a
    /// request, so minting a fresh entry each time let one unpaired remote
    /// sender grow this map without bound, fill `pair list` with duplicate
    /// rows for one sender, and multiply the brute-force hit rate by the
    /// number of simultaneously-live codes.
    pub async fn generate_code(&self, channel_id: &str, sender_id: &str) -> String {
        let now = Utc::now();
        let mut pending = self.pending.write().await;
        // Drop expired entries here too, so the map stays bounded even between
        // runs of the periodic `sweep_expired`.
        pending.retain(|_, p| p.expires_at > now);
        if let Some((code, _)) = pending
            .iter()
            .find(|(_, p)| p.channel_id == channel_id && p.sender_id == sender_id)
        {
            return code.clone();
        }

        // Scope `rand::thread_rng()` (which is `Rc<UnsafeCell<...>>` and
        // therefore !Send) inside its own block so the resulting future
        // does NOT hold a non-Send guard across an `.await`.
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
            expires_at: now + self.code_ttl,
        };
        pending.insert(code.clone(), pairing);
        code
    }

    /// Approve a pairing code. Returns the approved sender on success.
    ///
    /// Returns a uniform error message whether the code is wrong, used, or expired,
    /// to prevent attackers from distinguishing between these states.
    pub async fn approve_code(&self, code: &str) -> Result<AllowedSender, String> {
        self.approve_inner(code, true).await
    }

    /// Approve a pairing code from an **already-authenticated operator** path
    /// (`agentos channel pair approve <code>` over the kernel bus, or the REST
    /// equivalent). Identical to [`approve_code`](Self::approve_code) except
    /// that it neither consults nor consumes the failed-guess budget.
    ///
    /// That budget is a single global counter, and `/pair` is open to unpaired
    /// channel senders: a stranger spamming wrong codes kept it exhausted and
    /// so locked the operator out of pairing — which, with pairing now the only
    /// door to chat, `/stop`, `/agent` and question answering, was a full
    /// lockout of channel control by an unauthenticated party.
    ///
    /// ponytail: global bucket, not per-channel. Per-channel buckets on the
    /// untrusted `/pair` path are the fuller fix; this exemption removes the
    /// lockout.
    pub async fn approve_code_trusted(&self, code: &str) -> Result<AllowedSender, String> {
        self.approve_inner(code, false).await
    }

    /// Approve a pending request by `(channel_id, sender_id)` rather than by
    /// code, for an **already-authenticated operator** path (kernel bus / REST
    /// with `channels:w`).
    ///
    /// The code is deliberately absent from every introspection surface
    /// (`list_pending`, `pair list`, the REST pairing list), which makes the
    /// self-pair case circular: the operator *is* the sender, and the only
    /// place the code appears is the kernel log. Both fields of a pending row
    /// are already exposed, so approving by row closes that gap without
    /// widening what introspection reveals.
    ///
    /// Takes no secret, so it MUST NOT be reachable from the inbound-channel
    /// path — `/pair` stays code-only. Like [`approve_code_trusted`], it
    /// neither consults nor consumes the failed-guess budget: there is no code
    /// to guess, and the caller is already authenticated.
    pub async fn approve_pending(
        &self,
        channel_id: &str,
        sender_id: &str,
    ) -> Result<AllowedSender, String> {
        let now = Utc::now();
        let mut pending = self.pending.write().await;
        let Some(code) = pending
            .iter()
            .find(|(_, p)| p.channel_id == channel_id && p.sender_id == sender_id)
            .map(|(code, _)| code.clone())
        else {
            return Err(format!(
                "No pending pairing request for sender '{sender_id}' on channel '{channel_id}'"
            ));
        };
        // `find` above ignores expiry so an expired row reports as expired
        // rather than as missing — the operator sees a row in the panel either
        // way, and "ask them to message again" is the actionable answer.
        let pairing = pending.remove(&code).expect("code just located");
        if now > pairing.expires_at {
            return Err(format!(
                "Pairing request for sender '{sender_id}' has expired — ask them to message again"
            ));
        }
        drop(pending);

        let sender = AllowedSender {
            channel_id: pairing.channel_id.clone(),
            sender_id: pairing.sender_id.clone(),
            approved_at: now,
            label: None,
        };
        let mut allowed = self.allowed.write().await;
        // Idempotent: a sender already on the allowlist must not gain a second
        // row (it would render twice in `pair list` and the panel).
        if !allowed
            .iter()
            .any(|a| a.channel_id == channel_id && a.sender_id == sender_id)
        {
            allowed.push(sender.clone());
        }
        drop(allowed);
        self.persist().await;
        info!(
            channel_id = %pairing.channel_id,
            sender_id = %pairing.sender_id,
            "Pairing approved by operator (by sender)"
        );
        Ok(sender)
    }

    async fn approve_inner(&self, code: &str, throttled: bool) -> Result<AllowedSender, String> {
        // Throttle brute-force guessing: once the per-window failed-guess
        // budget is spent, reject with the same uniform error without even
        // consulting the code map.
        if throttled && self.guess_budget_exhausted().await {
            return Err("Invalid or expired pairing code".to_string());
        }

        let mut pending = self.pending.write().await;
        let Some(pairing) = pending.remove(code) else {
            drop(pending);
            if throttled {
                self.record_failed_guess().await;
            }
            return Err("Invalid or expired pairing code".to_string());
        };

        if Utc::now() > pairing.expires_at {
            // Code existed but is expired — uniform error, and count it as a
            // failed guess so expired-code spamming is throttled too.
            drop(pending);
            if throttled {
                self.record_failed_guess().await;
            }
            return Err("Invalid or expired pairing code".to_string());
        }

        let sender = AllowedSender {
            channel_id: pairing.channel_id.clone(),
            sender_id: pairing.sender_id.clone(),
            approved_at: Utc::now(),
            label: None,
        };
        self.allowed.write().await.push(sender.clone());
        self.persist().await;
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

    /// List pending (unapproved, unexpired) pairing requests. Codes are omitted.
    pub async fn list_pending(&self) -> Vec<PendingPairingInfo> {
        let now = Utc::now();
        self.pending
            .read()
            .await
            .values()
            .filter(|p| p.expires_at > now)
            .map(|p| PendingPairingInfo {
                channel_id: p.channel_id.clone(),
                sender_id: p.sender_id.clone(),
                expires_at: p.expires_at,
            })
            .collect()
    }

    /// Revoke an approved sender.
    pub async fn revoke(&self, channel_id: &str, sender_id: &str) -> bool {
        let mut allowed = self.allowed.write().await;
        let before = allowed.len();
        allowed.retain(|a| !(a.channel_id == channel_id && a.sender_id == sender_id));
        let removed = before != allowed.len();
        drop(allowed);
        if removed {
            self.persist().await;
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
            failed_guesses: RwLock::new((0, Utc::now())),
            persistence: OnceLock::new(),
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
    async fn test_brute_force_guessing_is_throttled() {
        let pm = PairingManager::new();
        // Burn the per-window failed-guess budget with wrong codes.
        for _ in 0..MAX_FAILED_GUESSES_PER_WINDOW {
            assert!(pm.approve_code("WRONG0").await.is_err());
        }
        // Now even the CORRECT code is rejected until the window resets —
        // the guess path is locked out.
        let code = pm.generate_code("discord", "user123").await;
        assert!(
            pm.approve_code(&code).await.is_err(),
            "guess path should be locked out after exhausting the budget"
        );
    }

    #[tokio::test]
    async fn test_successful_pair_not_penalized_by_throttle() {
        let pm = PairingManager::new();
        // A handful of correct pairings in a row must all succeed — success
        // does not consume the failed-guess budget.
        for i in 0..5 {
            let code = pm.generate_code("discord", &format!("user{i}")).await;
            assert!(pm.approve_code(&code).await.is_ok());
        }
    }

    /// Every rejected inbound message files a pairing request. Minting a fresh
    /// code each time let one unpaired remote sender grow `pending` without
    /// bound (~43k/day at the inbound limiter's 30 msg/min), show N duplicate
    /// rows for one sender in `channel pair list`, and keep N codes live at
    /// once against the brute-force budget.
    #[tokio::test]
    async fn test_repeat_requests_reuse_the_live_code() {
        let pm = PairingManager::new();
        let first = pm.generate_code("discord", "user123").await;
        for _ in 0..50 {
            assert_eq!(pm.generate_code("discord", "user123").await, first);
        }
        assert_eq!(pm.list_pending().await.len(), 1);

        // Scoping is per (channel, sender): a different sender, and the same
        // sender on a different channel, each still get their own code.
        assert_ne!(pm.generate_code("discord", "user999").await, first);
        assert_ne!(pm.generate_code("telegram", "user123").await, first);
        assert_eq!(pm.list_pending().await.len(), 3);
    }

    /// The failed-guess budget is global and `/pair` is open to unpaired
    /// senders, so a stranger spamming wrong codes could keep it exhausted.
    /// The operator's already-authenticated bus path must not share it.
    #[tokio::test]
    async fn test_operator_approval_ignores_the_guess_budget() {
        let pm = PairingManager::new();
        let code = pm.generate_code("discord", "user123").await;
        // A stranger burns the whole window on the untrusted path.
        for _ in 0..MAX_FAILED_GUESSES_PER_WINDOW {
            assert!(pm.approve_code("WRONG0").await.is_err());
        }
        assert!(
            pm.approve_code(&code).await.is_err(),
            "untrusted path is locked out"
        );
        // The operator, already authenticated by the bus, still pairs.
        let sender = pm.approve_code_trusted(&code).await.unwrap();
        assert_eq!(sender.sender_id, "user123");
        assert!(pm.is_allowed("discord", "user123").await);
    }

    /// An operator typo must not spend the anonymous budget either — that is
    /// how they would lock themselves out.
    #[tokio::test]
    async fn test_trusted_failures_do_not_consume_the_budget() {
        let pm = PairingManager::new();
        for _ in 0..MAX_FAILED_GUESSES_PER_WINDOW * 2 {
            assert!(pm.approve_code_trusted("WRONG0").await.is_err());
        }
        let code = pm.generate_code("discord", "user123").await;
        assert!(pm.approve_code(&code).await.is_ok());
    }

    /// The `/pair` arm normalises before handing the code over: codes are
    /// minted from `[A-Z0-9]`, so a lower-cased paste must pair rather than
    /// fail generically and burn a shared guess slot.
    #[tokio::test]
    async fn test_lowercase_typed_code_pairs_after_normalisation() {
        let pm = PairingManager::new();
        let code = pm.generate_code("telegram", "user7").await;
        let typed = code.to_lowercase();
        assert!(
            pm.approve_code(&typed).await.is_err(),
            "raw lower case must not match"
        );
        assert!(pm.approve_code(&typed.trim().to_uppercase()).await.is_ok());
    }

    /// The self-pair case: the operator is the sender, so the code only ever
    /// reaches the kernel log. Approving by the `(channel, sender)` pair that
    /// `list_pending` already exposes must allowlist them and clear the row.
    #[tokio::test]
    async fn test_approve_pending_by_sender() {
        let pm = PairingManager::new();
        pm.generate_code("telegram", "1130156019").await;

        let sender = pm
            .approve_pending("telegram", "1130156019")
            .await
            .expect("pending row should approve");
        assert_eq!(sender.sender_id, "1130156019");
        assert!(pm.is_allowed("telegram", "1130156019").await);
        assert!(pm.list_pending().await.is_empty());
    }

    /// Scoped to one `(channel, sender)`: approving one row must not approve
    /// a different sender, nor the same sender on another channel.
    #[tokio::test]
    async fn test_approve_pending_is_scoped() {
        let pm = PairingManager::new();
        pm.generate_code("telegram", "userA").await;
        pm.generate_code("telegram", "userB").await;
        pm.generate_code("discord", "userA").await;

        pm.approve_pending("telegram", "userA").await.unwrap();
        assert!(pm.is_allowed("telegram", "userA").await);
        assert!(!pm.is_allowed("telegram", "userB").await);
        assert!(!pm.is_allowed("discord", "userA").await);
        assert_eq!(pm.list_pending().await.len(), 2);

        assert!(pm.approve_pending("telegram", "nobody").await.is_err());
        assert!(pm.approve_pending("slack", "userA").await.is_err());
    }

    /// An expired row reports as expired, not as missing — the operator can
    /// still see it in the panel, so "ask them to message again" is the useful
    /// answer. It must not allowlist.
    #[tokio::test]
    async fn test_approve_pending_rejects_expired() {
        let pm = PairingManager::new();
        pm.generate_code("telegram", "user7").await;
        pm.pending
            .write()
            .await
            .values_mut()
            .for_each(|p| p.expires_at = Utc::now() - Duration::minutes(1));

        let err = pm.approve_pending("telegram", "user7").await.unwrap_err();
        assert!(err.contains("expired"), "got: {err}");
        assert!(!pm.is_allowed("telegram", "user7").await);
    }

    /// Takes no secret, so it must never spend or be blocked by the global
    /// failed-guess budget that an unpaired stranger can exhaust via `/pair`.
    #[tokio::test]
    async fn test_approve_pending_ignores_the_guess_budget() {
        let pm = PairingManager::new();
        pm.generate_code("telegram", "user7").await;
        for _ in 0..MAX_FAILED_GUESSES_PER_WINDOW {
            assert!(pm.approve_code("WRONG0").await.is_err());
        }
        assert!(pm.approve_pending("telegram", "user7").await.is_ok());

        // And its own failures do not consume the budget either.
        let code = pm.generate_code("telegram", "user8").await;
        for _ in 0..MAX_FAILED_GUESSES_PER_WINDOW * 2 {
            assert!(pm.approve_pending("telegram", "ghost").await.is_err());
        }
        assert!(pm.approve_code_trusted(&code).await.is_ok());
    }

    /// Re-approving must not push a duplicate allowlist row — `revoke` uses
    /// `retain`, but a dupe would render twice in `pair list` and the panel.
    #[tokio::test]
    async fn test_approve_pending_does_not_duplicate_allowlist_rows() {
        let pm = PairingManager::new();
        pm.generate_code("telegram", "user7").await;
        pm.approve_pending("telegram", "user7").await.unwrap();
        // A second request from an already-approved sender, approved again.
        pm.generate_code("telegram", "user7").await;
        pm.approve_pending("telegram", "user7").await.unwrap();
        assert_eq!(pm.list_approved().await.len(), 1);
    }

    #[tokio::test]
    async fn test_sweep_expired_removes_stale_codes() {
        let pm = PairingManager::new();
        pm.generate_code("discord", "user123").await;
        assert_eq!(pm.sweep_expired().await, 0, "live codes are kept");
        // Force expiry, then sweep.
        pm.pending
            .write()
            .await
            .values_mut()
            .for_each(|p| p.expires_at = Utc::now() - Duration::minutes(1));
        assert_eq!(pm.sweep_expired().await, 1);
        assert!(pm.list_pending().await.is_empty());
    }

    #[tokio::test]
    async fn test_list_pending_tracks_unapproved() {
        let pm = PairingManager::new();
        let code = pm.generate_code("telegram", "user42").await;
        let pending = pm.list_pending().await;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].sender_id, "user42");
        assert_eq!(pending[0].channel_id, "telegram");

        // Approving removes it from pending.
        pm.approve_code(&code).await.unwrap();
        assert!(pm.list_pending().await.is_empty());
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
