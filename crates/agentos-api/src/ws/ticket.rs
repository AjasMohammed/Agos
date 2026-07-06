//! Short-lived, single-use WebSocket auth tickets.
//!
//! A browser `WebSocket` cannot send an `Authorization` header, so the upgrade
//! historically carried the long-lived `agos_` key in the URL query string —
//! where it lands in proxy/access logs and browser history. Tickets close that
//! hole: `POST /api/v1/ws/ticket` (bearer-authed) mints an opaque single-use
//! ticket that inherits the presenting key's scopes and expires in seconds; the
//! socket then connects with `?ticket=…`, which is worthless once redeemed or
//! expired. The raw `?token=` path stays accepted for script clients.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rand::Rng;
use tokio::sync::Mutex;

/// Ticket lifetime. Long enough for the client to turn around and connect,
/// short enough that a logged URL is stale by the time anyone reads the log.
pub const WS_TICKET_TTL_SECS: u64 = 30;

struct TicketEntry {
    permissions: Vec<String>,
    expires_at: Instant,
}

/// In-memory mint/redeem store for WS auth tickets.
///
/// Process-local by design: a kernel restart only forces clients through their
/// normal reconnect path, which mints a fresh ticket. Expired entries are swept
/// on each mint, so the map is bounded by mint rate × TTL.
#[derive(Clone, Default)]
pub struct WsTicketStore {
    inner: Arc<Mutex<HashMap<String, TicketEntry>>>,
}

impl WsTicketStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mint a single-use ticket carrying `permissions`.
    pub async fn mint(&self, permissions: Vec<String>) -> String {
        self.mint_with_ttl(permissions, Duration::from_secs(WS_TICKET_TTL_SECS))
            .await
    }

    async fn mint_with_ttl(&self, permissions: Vec<String>, ttl: Duration) -> String {
        // 32 random bytes → 64 hex chars, same entropy as an API key secret.
        let mut raw = [0u8; 32];
        rand::thread_rng().fill(&mut raw[..]);
        let ticket = hex::encode(raw);

        let now = Instant::now();
        let mut map = self.inner.lock().await;
        map.retain(|_, e| e.expires_at > now);
        map.insert(
            ticket.clone(),
            TicketEntry {
                permissions,
                expires_at: now + ttl,
            },
        );
        ticket
    }

    /// Redeem a ticket, consuming it. Returns the scopes it carries, or `None`
    /// if the ticket is unknown, already used, or expired.
    pub async fn redeem(&self, ticket: &str) -> Option<Vec<String>> {
        let entry = self.inner.lock().await.remove(ticket)?;
        (entry.expires_at > Instant::now()).then_some(entry.permissions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mint_then_redeem_returns_scopes() {
        let store = WsTicketStore::new();
        let t = store.mint(vec!["tasks:r".into()]).await;
        assert_eq!(store.redeem(&t).await, Some(vec!["tasks:r".into()]));
    }

    #[tokio::test]
    async fn redeem_is_single_use() {
        let store = WsTicketStore::new();
        let t = store.mint(vec![]).await;
        assert!(store.redeem(&t).await.is_some());
        assert!(store.redeem(&t).await.is_none());
    }

    #[tokio::test]
    async fn expired_ticket_is_rejected() {
        let store = WsTicketStore::new();
        let t = store.mint_with_ttl(vec![], Duration::from_secs(0)).await;
        assert!(store.redeem(&t).await.is_none());
    }

    #[tokio::test]
    async fn unknown_ticket_is_rejected() {
        let store = WsTicketStore::new();
        assert!(store.redeem("no-such-ticket").await.is_none());
    }

    #[tokio::test]
    async fn mint_sweeps_expired_entries() {
        let store = WsTicketStore::new();
        let dead = store.mint_with_ttl(vec![], Duration::from_secs(0)).await;
        let _live = store.mint(vec![]).await; // sweep runs here
        assert_eq!(store.inner.lock().await.len(), 1);
        assert!(store.redeem(&dead).await.is_none());
    }
}
