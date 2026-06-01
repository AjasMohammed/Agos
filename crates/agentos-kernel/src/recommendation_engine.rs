//! Background proactive recommendation engine (Phase 4).
//!
//! Mirrors the [`crate::consolidation::ConsolidationEngine`] lifecycle:
//! a periodic tick trigger, a `cycle_lock` that serializes cycles so two never
//! overlap, and all SQLite work delegated to [`crate::recommendations_store`].
//!
//! **Zero task-context cost**: this engine is never referenced from
//! `task_executor.rs` or `context_compiler.rs` and nothing here is injected
//! into a `ContextWindow` belonging to a running task. It generates a short
//! proactive tip from the top-ranked interest topics (no LLM call in the MVP)
//! and delivers it out-of-loop via `NotificationRouter::deliver` → `UserInbox`.
//!
//! The engine is a no-op when `personalization.proactive_enabled` is `false`
//! (the default), so it is safe to construct at every kernel boot.

use crate::recommendations_store::{Recommendation, RecommendationKind, RecommendationsStore};
use agentos_types::{
    NotificationID, NotificationPriority, NotificationSource, TraceID, UserMessage, UserMessageKind,
};
use chrono::Utc;
use std::collections::HashMap;
use std::sync::Arc;

/// Number of top interest topics fetched per cycle.
const TOP_N: u32 = 5;

/// Background engine that generates and delivers one proactive recommendation
/// per cycle (when rate-limit and dedup guards allow).
pub struct RecommendationEngine {
    store: Arc<RecommendationsStore>,
    interests: Arc<crate::interest_model::InterestModel>,
    /// Held for Phase 5 active-learning: profile entries inform the generation
    /// prompt once an LLM call is added. Unused in the heuristic MVP.
    #[allow(dead_code)]
    profile: Arc<crate::user_profile_store::UserProfileStore>,
    notification_router: Arc<crate::notification_router::NotificationRouter>,
    enabled: bool,
    proactive_enabled: bool,
    max_per_day: u32,
    dedup_cooldown_hours: f64,
    min_confidence: f64,
    /// Serializes concurrent `run_cycle` callers — overlapping cycles are
    /// skipped via `try_lock`, never queued.
    cycle_lock: tokio::sync::Mutex<()>,
}

impl RecommendationEngine {
    /// Construct from the already-built stores and a snapshot of the
    /// personalization config.
    pub fn new(
        store: Arc<RecommendationsStore>,
        interests: Arc<crate::interest_model::InterestModel>,
        profile: Arc<crate::user_profile_store::UserProfileStore>,
        notification_router: Arc<crate::notification_router::NotificationRouter>,
        config: &crate::config::PersonalizationConfig,
    ) -> Self {
        Self {
            store,
            interests,
            profile,
            notification_router,
            enabled: config.enabled,
            proactive_enabled: config.proactive_enabled,
            max_per_day: config.max_recommendations_per_day,
            dedup_cooldown_hours: config.recommendation_dedup_cooldown_hours,
            min_confidence: config.recommendation_min_confidence as f64,
            cycle_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// Returns true when at least the base personalization flag is on (used by
    /// the `run_loop.rs` periodic ticker to decide whether to idle or run).
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Run one recommendation cycle.
    ///
    /// Returns `true` if a recommendation was generated and delivered,
    /// `false` if skipped for any reason (disabled, rate-limited, deduped,
    /// no interests, concurrent cycle already running).
    pub async fn run_cycle(&self) -> anyhow::Result<bool> {
        // ── 1. Opt-in gate ────────────────────────────────────────────────────
        if !self.enabled || !self.proactive_enabled {
            return Ok(false);
        }

        // ── 2. Concurrency guard ──────────────────────────────────────────────
        // Skip rather than queue if a cycle is already in flight.
        let _guard = match self.cycle_lock.try_lock() {
            Ok(g) => g,
            Err(_) => {
                tracing::debug!("RecommendationEngine: skipping cycle — already running");
                return Ok(false);
            }
        };

        let now = Utc::now().timestamp();

        // ── 3. Rate-limit pre-check ───────────────────────────────────────────
        let since_24h = now - 86_400;
        let delivered_today = self.store.count_delivered_since(since_24h).await?;
        if delivered_today >= self.max_per_day {
            tracing::debug!(
                delivered_today,
                max_per_day = self.max_per_day,
                "RecommendationEngine: daily rate-limit reached"
            );
            return Ok(false);
        }

        // ── 4. Gather signal ──────────────────────────────────────────────────
        let top_interests = self.interests.top_interests(TOP_N).await;
        if top_interests.is_empty() {
            tracing::debug!("RecommendationEngine: no interest signal — skipping cycle");
            return Ok(false);
        }

        // ── 5. Generate tip (no LLM call in MVP — formatted string) ──────────
        let topics: Vec<&str> = top_interests
            .iter()
            .map(|t| t.topic.as_str())
            .take(3)
            .collect();
        if topics.is_empty() {
            return Ok(false);
        }

        let content = format!(
            "Based on your recent activity, you might find these topics useful: {}.",
            topics.join(", ")
        );
        let basis_topics: Vec<String> = topics.iter().map(|s| s.to_string()).collect();
        let basis_json = serde_json::to_string(&basis_topics).unwrap_or_else(|_| "[]".to_string());

        // Fixed confidence for the simple heuristic tip (above default floor of 0.5).
        let confidence = 0.7_f64;

        // ── 6. Confidence floor ───────────────────────────────────────────────
        if confidence < self.min_confidence {
            tracing::debug!(
                confidence,
                min_confidence = self.min_confidence,
                "RecommendationEngine: confidence below floor — dropping"
            );
            return Ok(false);
        }

        // ── 7. Dedup + cooldown ───────────────────────────────────────────────
        let dedup_hash = Recommendation::compute_dedup_hash(RecommendationKind::Tip, &content);
        let cooldown_secs = (self.dedup_cooldown_hours * 3600.0) as i64;
        if self
            .store
            .is_on_cooldown(&dedup_hash, now, cooldown_secs)
            .await?
        {
            tracing::debug!(
                dedup_hash,
                "RecommendationEngine: recommendation on cooldown — skipping"
            );
            return Ok(false);
        }

        // ── 8. Persist (Pending) ──────────────────────────────────────────────
        let rec = Recommendation {
            id: uuid::Uuid::new_v4().to_string(),
            kind: RecommendationKind::Tip,
            content: content.clone(),
            basis: basis_json,
            confidence,
            status: crate::recommendations_store::RecommendationStatus::Pending,
            dedup_hash: dedup_hash.clone(),
            created_at: now,
            delivered_at: None,
            feedback_at: None,
        };

        let inserted = self.store.insert(rec.clone(), self.min_confidence).await?;
        if !inserted {
            // Dedup collision at insert time (race between is_on_cooldown and insert).
            tracing::debug!("RecommendationEngine: dedup collision on insert — skipping");
            return Ok(false);
        }

        // ── 9. Out-of-loop delivery ───────────────────────────────────────────
        let msg = UserMessage {
            id: NotificationID::new(),
            from: NotificationSource::Kernel,
            task_id: None,
            trace_id: TraceID::new(),
            kind: UserMessageKind::Notification,
            priority: NotificationPriority::Info,
            subject: "Proactive tip".to_string(),
            body: rec.content.clone(),
            interaction: None,
            delivery_status: HashMap::new(),
            response: None,
            created_at: Utc::now(),
            expires_at: None,
            read: false,
            thread_id: Some(rec.id.clone()),
            reply_to_external_id: None,
            attachment: None,
        };

        // ── 10. Deliver, then mark only on success ───────────────────────────
        // Do NOT mark delivered on failure: burning the dedup slot and daily
        // rate-limit quota for a tip the user never received would permanently
        // silence the engine (especially with max_per_day = 1).
        if let Err(e) = self.notification_router.deliver(msg).await {
            tracing::warn!(
                error = %e,
                id = rec.id,
                "RecommendationEngine: delivery failed — skipping mark_delivered"
            );
            return Ok(false);
        }

        self.store
            .mark_delivered(&rec.id, Utc::now().timestamp())
            .await?;

        tracing::info!(
            id = %rec.id,
            topics = ?topics,
            "RecommendationEngine: proactive tip delivered"
        );

        Ok(true)
    }

    /// Convenience accessor for the store — used by `run_loop.rs` to prune old
    /// recommendations on the periodic sweep.
    pub fn store(&self) -> &Arc<RecommendationsStore> {
        &self.store
    }

    /// Pass-through: record user feedback for a recommendation.
    ///
    /// `accepted = true` → status becomes `accepted`
    /// `accepted = false` → status becomes `dismissed`
    pub async fn feedback(&self, id: &str, accepted: bool) -> anyhow::Result<()> {
        self.store
            .record_feedback(id, accepted, Utc::now().timestamp())
            .await
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PersonalizationConfig;
    use crate::interest_model::InterestModel;
    use crate::tool_usage_store::ToolUsageStore;
    use crate::user_interests_store::UserInterestsStore;
    use crate::user_profile_store::UserProfileStore;
    use std::sync::Arc;

    fn disabled_config() -> PersonalizationConfig {
        PersonalizationConfig {
            enabled: false,
            proactive_enabled: false,
            ..PersonalizationConfig::default()
        }
    }

    fn enabled_config(max_per_day: u32, min_confidence: f32) -> PersonalizationConfig {
        PersonalizationConfig {
            enabled: true,
            proactive_enabled: true,
            max_recommendations_per_day: max_per_day,
            recommendation_min_confidence: min_confidence,
            recommendation_dedup_cooldown_hours: 168.0,
            ..PersonalizationConfig::default()
        }
    }

    /// Returns (engine, inbox, interests_store) — the caller can use
    /// `interests_store.reinforce(...)` directly to seed test signals.
    async fn build_engine(
        config: &PersonalizationConfig,
    ) -> (
        RecommendationEngine,
        Arc<crate::user_inbox::UserInbox>,
        Arc<UserInterestsStore>,
    ) {
        let dir = tempfile::tempdir().unwrap();

        let store = Arc::new(RecommendationsStore::open_in_memory().await.unwrap());

        let interests_store = Arc::new(
            UserInterestsStore::open(dir.path().join("user_interests.db"))
                .await
                .unwrap(),
        );
        let tool_usage =
            Arc::new(ToolUsageStore::open(&dir.path().join("agent_tool_usage.db")).unwrap());
        let episodic =
            Arc::new(agentos_memory::EpisodicStore::open(dir.path()).expect("open episodic"));
        let interest_model = Arc::new(InterestModel::new(
            Arc::clone(&interests_store),
            episodic,
            tool_usage,
            config,
        ));

        let profile_store = Arc::new(
            UserProfileStore::open(dir.path().join("user_profile.db"))
                .await
                .unwrap(),
        );

        let audit = Arc::new(
            agentos_audit::AuditLog::open(&dir.path().join("audit.db")).expect("open audit log"),
        );

        let inbox_path = dir.path().join("user_inbox.db");
        let inbox = Arc::new(
            crate::user_inbox::UserInbox::new(&inbox_path, 1000).expect("open user inbox"),
        );
        let router = Arc::new(crate::notification_router::NotificationRouter::new(
            Arc::clone(&inbox),
            audit,
        ));

        // Leak tempdir so SQLite files remain alive for the test duration.
        Box::leak(Box::new(dir));

        let engine =
            RecommendationEngine::new(store, interest_model, profile_store, router, config);
        (engine, inbox, interests_store)
    }

    #[tokio::test]
    async fn disabled_is_noop() {
        let config = disabled_config();
        let (engine, inbox, _interests) = build_engine(&config).await;

        let result = engine.run_cycle().await.unwrap();
        assert!(!result, "disabled engine should return false");

        // No messages should reach the inbox.
        let msgs = inbox.list(false, 10).await.unwrap_or_default();
        assert!(
            msgs.is_empty(),
            "no messages should be delivered when disabled"
        );
    }

    #[tokio::test]
    async fn rate_limit_enforced() {
        let config = enabled_config(0, 0.5); // max_per_day = 0
        let (engine, _inbox, _interests) = build_engine(&config).await;

        // max_per_day = 0, so the rate limit is immediately hit.
        let result = engine.run_cycle().await.unwrap();
        assert!(!result, "rate limit of 0/day should prevent delivery");
    }

    #[tokio::test]
    async fn concurrent_cycle_skipped() {
        let config = enabled_config(10, 0.5);
        let (engine, _inbox, _interests) = build_engine(&config).await;

        // Hold the cycle lock to simulate an in-flight cycle.
        let held = engine.cycle_lock.lock().await;
        // A concurrent run_cycle must NOT block — it try_locks and returns false.
        let result = engine.run_cycle().await.unwrap();
        assert!(!result, "concurrent cycle should be skipped");
        drop(held);
    }

    #[tokio::test]
    async fn dedup_suppresses_repeat() {
        let config = enabled_config(10, 0.5);
        let (engine, _inbox, interests) = build_engine(&config).await;

        // Seed an interest so run_cycle has something to work with.
        interests
            .reinforce(
                "rust",
                crate::user_interests_store::SignalType::TaskTopic,
                1.0,
                168.0,
            )
            .await
            .unwrap();

        // First cycle — should insert + deliver.
        let first = engine.run_cycle().await.unwrap();
        assert!(first, "first cycle should deliver when interests exist");

        // Second cycle with same interests — dedup_hash collision, should skip.
        let second = engine.run_cycle().await.unwrap();
        assert!(
            !second,
            "second cycle should be suppressed by dedup cooldown"
        );
    }

    #[tokio::test]
    async fn feedback_recorded() {
        let config = enabled_config(10, 0.5);
        let (engine, _inbox, interests) = build_engine(&config).await;

        // Seed interest for delivery.
        interests
            .reinforce(
                "python",
                crate::user_interests_store::SignalType::TaskTopic,
                1.0,
                168.0,
            )
            .await
            .unwrap();
        engine.run_cycle().await.unwrap();

        let recs = engine.store.list(10).await.unwrap();
        assert!(!recs.is_empty(), "at least one recommendation should exist");

        let id = recs[0].id.clone();
        engine.feedback(&id, false).await.unwrap(); // dismiss

        let fetched = engine.store.get(&id).await.unwrap().unwrap();
        assert_eq!(fetched.status.as_str(), "dismissed");
        assert!(fetched.feedback_at.is_some());
    }
}
