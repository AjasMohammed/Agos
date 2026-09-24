//! Operator-controlled routing matrix: which notification event kinds reach
//! which delivery channels.
//!
//! Rows are `(event, channel_key) -> mode`. An absent row means
//! [`RouteMode::Always`], so an upgraded install behaves exactly as it did
//! before — apart from the defaults seeded on the very first boot, which mute
//! the noisiest desktop banners.
//!
//! The matrix gates *outbound fan-out only*. `UserInbox` is written before the
//! gate runs, so muting a channel never loses the record: the panel's
//! notification bell and `GET /api/v1/notifications` stay complete.

use crate::state_store::KernelStateStore;
use agentos_types::NotificationEvent;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

/// Sentinel row recording that first-boot defaults have been seeded. Without
/// it, an operator who deliberately deletes every rule would have the defaults
/// pushed back on the next restart.
const SEED_SENTINEL: (&str, &str) = ("_seeded", "_");

/// What a `(event, channel)` cell does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteMode {
    /// Always deliver (the default for a cell with no rule).
    #[default]
    Always,
    /// Never deliver on this channel.
    Never,
    /// Deliver only while no control-panel WebSocket is connected.
    ///
    /// "Connected" means at least one live panel WebSocket connection, counted
    /// by the API's WS layer. A backgrounded panel tab still counts as
    /// connected — the open connection, not window focus, is the signal.
    WhenAway,
}

impl RouteMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            RouteMode::Always => "always",
            RouteMode::Never => "never",
            RouteMode::WhenAway => "when_away",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "always" => Some(RouteMode::Always),
            "never" => Some(RouteMode::Never),
            "when_away" => Some(RouteMode::WhenAway),
            _ => None,
        }
    }
}

impl std::fmt::Display for RouteMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Default rules written on first boot. Everything not listed stays
/// [`RouteMode::Always`].
const SEED_DEFAULTS: &[(NotificationEvent, &str, RouteMode)] = &[
    (NotificationEvent::Approval, "desktop", RouteMode::WhenAway),
    (NotificationEvent::TaskComplete, "desktop", RouteMode::Never),
    (NotificationEvent::StatusUpdate, "desktop", RouteMode::Never),
    (
        NotificationEvent::AgentMessage,
        "desktop",
        RouteMode::WhenAway,
    ),
];

/// In-memory routing matrix backed by the kernel state DB.
pub struct RouteMatrix {
    rules: RwLock<HashMap<(NotificationEvent, String), RouteMode>>,
    store: Arc<KernelStateStore>,
    /// Live control-panel WebSocket connections, maintained by the API layer.
    ///
    /// Deliberately *not* `realtime_event_sender.receiver_count()`: the API
    /// spawns a relay task that holds a receiver for the whole process
    /// lifetime, so that count is pinned above zero whenever the API is
    /// enabled and zero whenever it is not — a boot flag, not presence.
    panel_sessions: Arc<AtomicUsize>,
}

impl RouteMatrix {
    /// Load the persisted rules, seeding first-boot defaults if the table is
    /// empty and has never been seeded.
    pub async fn load(
        store: Arc<KernelStateStore>,
        panel_sessions: Arc<AtomicUsize>,
    ) -> anyhow::Result<Self> {
        let rows = store.load_notification_routes().await?;

        if rows.is_empty() {
            let mut seed: Vec<(String, String, String)> = SEED_DEFAULTS
                .iter()
                .map(|(e, c, m)| (e.to_string(), (*c).to_string(), m.to_string()))
                .collect();
            seed.push((
                SEED_SENTINEL.0.to_string(),
                SEED_SENTINEL.1.to_string(),
                RouteMode::Never.to_string(),
            ));
            store.upsert_notification_routes(seed).await?;
        }

        let rows = store.load_notification_routes().await?;
        let mut rules = HashMap::new();
        for (event, channel, mode) in rows {
            // The sentinel and any row written by a newer version with an
            // event/mode this build doesn't know are ignored, not fatal.
            let (Some(event), Some(mode)) =
                (NotificationEvent::parse(&event), RouteMode::parse(&mode))
            else {
                continue;
            };
            rules.insert((event, channel), mode);
        }

        Ok(Self {
            rules: RwLock::new(rules),
            store,
            panel_sessions,
        })
    }

    /// Hot path: pure in-memory lookup, no await, no I/O.
    pub fn mode(&self, event: NotificationEvent, channel_key: &str) -> RouteMode {
        // `into_inner` on poisoning: the critical sections are HashMap
        // reads/writes, so a poisoned lock still holds valid rules — and
        // falling back to "no rules" would silently un-mute every channel.
        self.rules
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(event, channel_key.to_string()))
            .copied()
            .unwrap_or_default()
    }

    /// Whether this event may be delivered on this channel right now.
    pub fn allows(&self, event: NotificationEvent, channel_key: &str) -> bool {
        match self.mode(event, channel_key) {
            RouteMode::Always => true,
            RouteMode::Never => false,
            RouteMode::WhenAway => !self.panel_connected(),
        }
    }

    /// True while at least one control-panel WebSocket connection is open.
    pub fn panel_connected(&self) -> bool {
        self.panel_sessions.load(Ordering::Relaxed) > 0
    }

    /// Persist a batch of rules, then update the cache. SQLite first: a failed
    /// write must never leave the cache claiming a rule that isn't stored.
    pub async fn set_many(
        &self,
        rules: Vec<(NotificationEvent, String, RouteMode)>,
    ) -> anyhow::Result<()> {
        if rules.is_empty() {
            return Ok(());
        }
        let rows = rules
            .iter()
            .map(|(e, c, m)| (e.to_string(), c.clone(), m.to_string()))
            .collect();
        self.store.upsert_notification_routes(rows).await?;
        let mut guard = self.rules.write().unwrap_or_else(|e| e.into_inner());
        for (event, channel, mode) in rules {
            guard.insert((event, channel), mode);
        }
        Ok(())
    }

    pub async fn set(
        &self,
        event: NotificationEvent,
        channel_key: &str,
        mode: RouteMode,
    ) -> anyhow::Result<()> {
        self.set_many(vec![(event, channel_key.to_string(), mode)])
            .await
    }

    /// Every rule, for the REST/CLI view.
    pub fn rules(&self) -> Vec<(NotificationEvent, String, RouteMode)> {
        let guard = self.rules.read().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<_> = guard
            .iter()
            .map(|((e, c), m)| (*e, c.clone(), *m))
            .collect();
        out.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()).then(a.1.cmp(&b.1)));
        out
    }

    /// Drop every rule for a channel that no longer exists.
    pub async fn forget_channel(&self, channel_key: &str) -> anyhow::Result<()> {
        self.store
            .delete_notification_routes_for_channel(channel_key.to_string())
            .await?;
        self.rules
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(_, c), _| c != channel_key);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn matrix(dir: &TempDir) -> (RouteMatrix, Arc<AtomicUsize>, Arc<KernelStateStore>) {
        let store = Arc::new(
            KernelStateStore::open(dir.path().join("state.db"))
                .await
                .expect("open state store"),
        );
        let panel = Arc::new(AtomicUsize::new(0));
        let m = RouteMatrix::load(Arc::clone(&store), Arc::clone(&panel))
            .await
            .expect("load matrix");
        (m, panel, store)
    }

    #[tokio::test]
    async fn absent_rule_defaults_to_always() {
        let dir = TempDir::new().unwrap();
        let (m, _panel, _s) = matrix(&dir).await;
        assert_eq!(
            m.mode(NotificationEvent::TaskFailed, "webhook"),
            RouteMode::Always
        );
        assert!(m.allows(NotificationEvent::TaskFailed, "webhook"));
    }

    #[tokio::test]
    async fn seeded_defaults_mute_the_noisy_desktop_rows() {
        let dir = TempDir::new().unwrap();
        let (m, _panel, _s) = matrix(&dir).await;
        assert_eq!(
            m.mode(NotificationEvent::TaskComplete, "desktop"),
            RouteMode::Never
        );
        assert_eq!(
            m.mode(NotificationEvent::Approval, "desktop"),
            RouteMode::WhenAway
        );
        // Untouched channels keep the old behaviour.
        assert_eq!(
            m.mode(NotificationEvent::TaskComplete, "telegram-main"),
            RouteMode::Always
        );
    }

    #[tokio::test]
    async fn set_persists_across_reload() {
        let dir = TempDir::new().unwrap();
        let (m, panel, store) = matrix(&dir).await;
        m.set(
            NotificationEvent::Approval,
            "telegram-main",
            RouteMode::Never,
        )
        .await
        .unwrap();
        assert_eq!(
            m.mode(NotificationEvent::Approval, "telegram-main"),
            RouteMode::Never
        );

        let reloaded = RouteMatrix::load(store, panel).await.unwrap();
        assert_eq!(
            reloaded.mode(NotificationEvent::Approval, "telegram-main"),
            RouteMode::Never
        );
    }

    #[tokio::test]
    async fn when_away_follows_panel_presence() {
        let dir = TempDir::new().unwrap();
        let (m, panel, _s) = matrix(&dir).await;
        m.set(NotificationEvent::Approval, "desktop", RouteMode::WhenAway)
            .await
            .unwrap();

        assert!(
            m.allows(NotificationEvent::Approval, "desktop"),
            "no panel session -> deliver"
        );

        panel.fetch_add(1, Ordering::Relaxed);
        assert!(
            !m.allows(NotificationEvent::Approval, "desktop"),
            "panel connected -> suppress"
        );

        panel.fetch_sub(1, Ordering::Relaxed);
        assert!(
            m.allows(NotificationEvent::Approval, "desktop"),
            "panel closed -> deliver again"
        );
    }

    #[tokio::test]
    async fn seed_runs_only_once() {
        let dir = TempDir::new().unwrap();
        let (m, panel, store) = matrix(&dir).await;
        // Operator deliberately un-mutes everything the seed set.
        for (event, channel, _) in SEED_DEFAULTS {
            m.set(*event, channel, RouteMode::Always).await.unwrap();
        }
        let reloaded = RouteMatrix::load(store, panel).await.unwrap();
        assert_eq!(
            reloaded.mode(NotificationEvent::TaskComplete, "desktop"),
            RouteMode::Always,
            "defaults must not be re-seeded over an operator's choice"
        );
    }

    #[tokio::test]
    async fn forget_channel_drops_only_that_channel() {
        let dir = TempDir::new().unwrap();
        let (m, _panel, _s) = matrix(&dir).await;
        m.set(
            NotificationEvent::Approval,
            "telegram-main",
            RouteMode::Never,
        )
        .await
        .unwrap();
        m.forget_channel("telegram-main").await.unwrap();
        assert_eq!(
            m.mode(NotificationEvent::Approval, "telegram-main"),
            RouteMode::Always
        );
        assert_eq!(
            m.mode(NotificationEvent::TaskComplete, "desktop"),
            RouteMode::Never,
            "other channels' rules survive"
        );
    }
}
