//! Background interest aggregator for proactive personalization (Phase 3).
//!
//! Mirrors the [`crate::consolidation::ConsolidationEngine`] lifecycle: a dual
//! trigger (>= N task completions OR >= T hours elapsed), an `AtomicU64` completion
//! counter, a `RwLock<DateTime<Utc>>` `last_run`, and a `cycle_lock` that serializes
//! aggregation cycles so two never overlap.
//!
//! It mines two signal sources that the system already records:
//!   (a) keyword topics from successful episode summaries (`EpisodicStore`), and
//!   (b) decayed tool-usage ranks per agent (`ToolUsageStore::rank_snapshot`).
//!
//! **Zero task-context cost**: this type is only ever driven by the background tick
//! and `on_task_completed`. It is never referenced from `task_executor.rs` or
//! `context_compiler.rs` and nothing here is injected into a `ContextWindow`.

use crate::config::PersonalizationConfig;
use crate::tool_usage_store::ToolUsageStore;
use crate::user_interests_store::{InterestScore, SignalType, UserInterestsStore};
use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Max episodes mined per aggregation cycle (bounded, like consolidation).
const MAX_EPISODES_PER_CYCLE: u32 = 500;
/// Max keyword topics extracted per episode summary.
const MAX_TOPICS_PER_EPISODE: usize = 5;

pub struct InterestModel {
    store: Arc<UserInterestsStore>,
    episodic_store: Arc<agentos_memory::EpisodicStore>,
    tool_usage: Arc<ToolUsageStore>,
    // Trigger thresholds + decay params, copied from config at construction.
    enabled: bool,
    trigger_tasks: u64,
    trigger_hours: f64,
    half_life_hours: f64,
    min_score: f64,
    // Background-job state (mirrors ConsolidationEngine).
    task_completions_since_last: AtomicU64,
    last_run: RwLock<DateTime<Utc>>,
    cycle_lock: tokio::sync::Mutex<()>,
}

impl InterestModel {
    pub fn new(
        store: Arc<UserInterestsStore>,
        episodic_store: Arc<agentos_memory::EpisodicStore>,
        tool_usage: Arc<ToolUsageStore>,
        config: &PersonalizationConfig,
    ) -> Self {
        Self {
            store,
            episodic_store,
            tool_usage,
            enabled: config.enabled,
            trigger_tasks: config.interest_aggregation_trigger_tasks,
            trigger_hours: config.interest_aggregation_trigger_hours,
            half_life_hours: config.interest_decay_half_life_hours,
            min_score: config.interest_min_score,
            task_completions_since_last: AtomicU64::new(0),
            last_run: RwLock::new(Utc::now()),
            cycle_lock: tokio::sync::Mutex::new(()),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Returns true when the time trigger has elapsed (used by the periodic tick).
    pub async fn should_run_time_trigger(&self) -> bool {
        if !self.enabled {
            return false;
        }
        let last = *self.last_run.read().await;
        let hours_since = (Utc::now() - last).num_seconds().max(0) as f64 / 3600.0;
        hours_since >= self.trigger_hours
    }

    /// Called from `complete_task_success` (mirror `consolidation.on_task_completed`).
    ///
    /// Increments the completion counter; if the task-count trigger is hit, spawns a
    /// detached aggregation cycle (overlapping cycles are skipped via `cycle_lock`).
    pub async fn on_task_completed(self: &Arc<Self>) {
        if !self.enabled {
            return;
        }
        let count = self
            .task_completions_since_last
            .fetch_add(1, Ordering::Relaxed)
            + 1;
        if count >= self.trigger_tasks {
            let me = Arc::clone(self);
            tokio::spawn(async move {
                if let Err(e) = me.run_cycle().await {
                    tracing::warn!(error = %e, "Interest aggregation cycle (task-trigger) failed");
                }
            });
        }
    }

    /// Run one aggregation cycle. Serializes via `cycle_lock` (try_lock so overlapping
    /// cycles are skipped, never queued). Reads successful episodes since `last_run`,
    /// mines task-topic + tool-usage signals, prunes negligible scores, then resets
    /// the counter and advances `last_run`.
    pub async fn run_cycle(&self) -> anyhow::Result<()> {
        if !self.enabled {
            return Ok(());
        }
        // Skip rather than queue if a cycle is already in flight.
        let _guard = match self.cycle_lock.try_lock() {
            Ok(g) => g,
            Err(_) => return Ok(()),
        };

        let since = *self.last_run.read().await;

        // (a) Task topics from episodic memory — same call consolidation uses.
        let episodes = self
            .episodic_store
            .find_successful_episodes(Some(since), MAX_EPISODES_PER_CYCLE)
            .await
            .unwrap_or_default();

        let mut seen_agents: HashSet<String> = HashSet::new();
        for ep in &episodes {
            seen_agents.insert(ep.agent_id.to_string());
            let text = ep.summary.clone().unwrap_or_else(|| ep.content.clone());
            for (topic, freq) in extract_topics(&text) {
                self.store
                    .reinforce(
                        &topic,
                        SignalType::TaskTopic,
                        freq as f64,
                        self.half_life_hours,
                    )
                    .await?;
            }
        }

        // (b) Tool usage frequencies — union per-agent ranks for agents seen in
        //     this cycle's episodes. We apply a small delta (0.1 × decayed_rank)
        //     rather than the full rank so that re-reinforcing on every cycle does
        //     not ratchet old tool scores indefinitely — the decay-then-add
        //     accumulator in `reinforce` handles the rest. This keeps tool interests
        //     proportional to recent usage without pinning rarely-used tools high.
        for agent_id in &seen_agents {
            let ranks: HashMap<String, f64> = self.tool_usage.rank_snapshot(agent_id).await;
            for (tool_name, score) in ranks {
                // Skip negligible scores (already decayed to near-zero by tool_usage).
                if score < 0.01 {
                    continue;
                }
                let topic = format!("tool:{tool_name}");
                // Small delta: accumulate interest proportionally to tool use, not the
                // raw rank, so re-reinforcement on each cycle decays toward equilibrium.
                let delta = score * 0.1;
                self.store
                    .reinforce(&topic, SignalType::ToolUsage, delta, self.half_life_hours)
                    .await?;
            }
        }

        // Prune negligible scores so the store self-bounds.
        let pruned = self.store.prune_below(self.min_score).await.unwrap_or(0);
        if pruned > 0 {
            tracing::debug!(pruned, "Interest model pruned negligible signals");
        }

        // Reset triggers.
        self.task_completions_since_last.store(0, Ordering::Relaxed);
        *self.last_run.write().await = Utc::now();
        Ok(())
    }

    /// Query API for Phase 4 — ZERO task-context cost. Never injected into a
    /// `ContextWindow`.
    pub async fn top_interests(&self, limit: u32) -> Vec<InterestScore> {
        self.store.top_interests(limit).await.unwrap_or_default()
    }

    /// Phase 6 right-to-forget: delete all interest rows from `user_interests.db`.
    /// Returns the number of rows removed.
    pub async fn clear_interests(&self) -> anyhow::Result<usize> {
        self.store.clear_all().await
    }

    /// Exact row count for `personalization status` — includes rows with zero
    /// or negative decayed scores that `top_interests` would silently drop.
    pub async fn count_interests(&self) -> anyhow::Result<usize> {
        self.store.count_topics().await
    }

    /// Full raw dump for `personalization export` — returns all rows regardless
    /// of decay state so the export is complete.
    pub async fn load_all_interests(
        &self,
    ) -> anyhow::Result<Vec<crate::user_interests_store::InterestTopic>> {
        self.store.load_all_raw().await
    }
}

/// A small English stop-word set — enough to drop obvious filler from task summaries.
fn is_stopword(w: &str) -> bool {
    matches!(
        w,
        "the"
            | "and"
            | "for"
            | "with"
            | "that"
            | "this"
            | "from"
            | "into"
            | "your"
            | "you"
            | "are"
            | "was"
            | "were"
            | "has"
            | "have"
            | "had"
            | "will"
            | "would"
            | "should"
            | "could"
            | "can"
            | "but"
            | "not"
            | "all"
            | "any"
            | "out"
            | "use"
            | "using"
            | "used"
            | "get"
            | "got"
            | "set"
            | "run"
            | "ran"
            | "new"
            | "now"
            | "via"
            | "per"
            | "its"
            | "their"
            | "them"
            | "they"
            | "what"
            | "when"
            | "which"
            | "where"
            | "who"
            | "how"
            | "task"
            | "user"
            | "agent"
            | "please"
            | "make"
            | "made"
            | "need"
            | "want"
            | "let"
            | "than"
            | "then"
            | "also"
            | "some"
            | "more"
            | "most"
            | "such"
            | "each"
            | "about"
    )
}

/// Tokenize `text` into candidate topics with their per-episode frequency.
///
/// Lowercase, split on non-alphanumeric, drop tokens shorter than 3 chars and
/// stop-words, count frequency, then take the top `MAX_TOPICS_PER_EPISODE` by
/// frequency. Deterministic for a given input (stable tie-break by topic name).
pub(crate) fn extract_topics(text: &str) -> Vec<(String, usize)> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for raw in text.split(|c: char| !c.is_alphanumeric()) {
        if raw.len() < 3 {
            continue;
        }
        let token = raw.to_lowercase();
        if token.len() < 3 || is_stopword(&token) {
            continue;
        }
        *counts.entry(token).or_insert(0) += 1;
    }
    let mut pairs: Vec<(String, usize)> = counts.into_iter().collect();
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    pairs.truncate(MAX_TOPICS_PER_EPISODE);
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_topics_filters_and_ranks() {
        let text = "Refactor the kernel scheduler. The scheduler scheduler kernel test test test.";
        let topics = extract_topics(text);
        let map: HashMap<String, usize> = topics.iter().cloned().collect();
        // "the" is a stopword, "test" appears 3x -> top, "scheduler" 3x, "kernel" 2x.
        assert!(!map.contains_key("the"));
        assert_eq!(map.get("test").copied(), Some(3));
        assert_eq!(map.get("scheduler").copied(), Some(3));
        assert_eq!(map.get("kernel").copied(), Some(2));
        // "refactor" appears once.
        assert_eq!(map.get("refactor").copied(), Some(1));
    }

    #[test]
    fn extract_topics_caps_at_max() {
        let text = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda";
        let topics = extract_topics(text);
        assert!(topics.len() <= MAX_TOPICS_PER_EPISODE);
    }

    #[test]
    fn extract_topics_is_deterministic() {
        let text = "one one two two three three four";
        let a = extract_topics(text);
        let b = extract_topics(text);
        assert_eq!(a, b);
    }

    fn cfg(enabled: bool, trigger_tasks: u64, trigger_hours: f64) -> PersonalizationConfig {
        PersonalizationConfig {
            enabled,
            interest_aggregation_trigger_tasks: trigger_tasks,
            interest_aggregation_trigger_hours: trigger_hours,
            interest_decay_half_life_hours: 168.0,
            interest_min_score: 0.05,
            ..PersonalizationConfig::default()
        }
    }

    /// Build an InterestModel over in-memory / temp stores. Episodic + tool-usage
    /// are real (opened against temp/in-memory dbs) but left empty — run_cycle over
    /// no episodes is a no-op that still exercises the trigger + lock machinery.
    async fn model(config: &PersonalizationConfig) -> Arc<InterestModel> {
        let dir = tempfile::tempdir().unwrap();
        let interests = Arc::new(
            UserInterestsStore::open(dir.path().join("user_interests.db"))
                .await
                .unwrap(),
        );
        // ToolUsageStore::open is sync and takes &Path.
        let tool_usage =
            Arc::new(ToolUsageStore::open(&dir.path().join("agent_tool_usage.db")).unwrap());
        // EpisodicStore::open is sync and takes the data dir (it appends the db name).
        let episodic =
            Arc::new(agentos_memory::EpisodicStore::open(dir.path()).expect("open episodic store"));
        Box::leak(Box::new(dir));
        Arc::new(InterestModel::new(interests, episodic, tool_usage, config))
    }

    #[tokio::test]
    async fn dual_trigger_fires() {
        // Task-count trigger: 3 completions -> a cycle spawns and resets the counter.
        let c = cfg(true, 3, 24.0);
        let m = model(&c).await;
        m.on_task_completed().await;
        m.on_task_completed().await;
        m.on_task_completed().await;
        // Allow the spawned cycle to run and reset the counter.
        for _ in 0..50 {
            if m.task_completions_since_last.load(Ordering::Relaxed) == 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(m.task_completions_since_last.load(Ordering::Relaxed), 0);

        // Time trigger: force last_run into the past, should_run_time_trigger -> true.
        *m.last_run.write().await = Utc::now() - chrono::Duration::hours(25);
        assert!(m.should_run_time_trigger().await);
        // A recent last_run -> false.
        *m.last_run.write().await = Utc::now();
        assert!(!m.should_run_time_trigger().await);
    }

    #[tokio::test]
    async fn cycle_lock_serializes() {
        let c = cfg(true, 1000, 24.0);
        let m = model(&c).await;
        // Hold the cycle lock to simulate an in-flight cycle.
        let held = m.cycle_lock.lock().await;
        // A concurrent run_cycle must NOT block — it try_locks and returns Ok early.
        let res = m.run_cycle().await;
        assert!(res.is_ok());
        // Because the cycle was skipped, last_run was not advanced past `held`-time.
        drop(held);
        // Now a real run can proceed.
        assert!(m.run_cycle().await.is_ok());
    }

    #[tokio::test]
    async fn disabled_is_noop() {
        let c = cfg(false, 1, 0.0);
        let m = model(&c).await;
        assert!(!m.is_enabled());
        m.on_task_completed().await;
        assert!(m.run_cycle().await.is_ok());
        assert!(!m.should_run_time_trigger().await);
        assert!(m.top_interests(10).await.is_empty());
    }
}
