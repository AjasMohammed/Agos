use chrono::{DateTime, Utc};
use std::collections::HashMap;

/// Rolling behaviour statistics for a single agent, used by `AnomalyScorer`.
pub struct AgentBehaviorStats {
    pub avg_tools_per_task: f64,
    pub permission_denial_count: u64,
    pub tool_call_count: u64,
    pub last_updated: DateTime<Utc>,
}

impl AgentBehaviorStats {
    fn new() -> Self {
        Self {
            avg_tools_per_task: 0.0,
            permission_denial_count: 0,
            tool_call_count: 0,
            last_updated: Utc::now(),
        }
    }
}

/// An anomaly alert generated when an agent's behaviour score crosses the threshold.
#[derive(Debug, Clone)]
pub struct AnomalyAlert {
    pub agent_id: String,
    /// Normalised score in `[0.0, 1.0]`. Values >= `ALERT_THRESHOLD` indicate
    /// suspicious behaviour.
    pub score: f64,
    pub reason: String,
}

/// Threshold above which an anomaly alert is emitted.
const ALERT_THRESHOLD: f64 = 0.6;

/// Lightweight in-process anomaly scorer.
///
/// Tracks per-agent tool-call counts and permission-denial counts.
/// Scores are heuristic: a high denial ratio (>5 denials) lifts the score
/// significantly.  Callers should persist their own audit events separately;
/// this struct only accumulates counters for the lifetime of the process.
pub struct AnomalyScorer {
    agent_stats: HashMap<String, AgentBehaviorStats>,
}

impl Default for AnomalyScorer {
    fn default() -> Self {
        Self::new()
    }
}

impl AnomalyScorer {
    /// Creates a new empty scorer.
    pub fn new() -> Self {
        Self {
            agent_stats: HashMap::new(),
        }
    }

    /// Records a successful tool call for `agent_id`.
    pub fn record_tool_call(&mut self, agent_id: &str) {
        let stats = self
            .agent_stats
            .entry(agent_id.to_owned())
            .or_insert_with(AgentBehaviorStats::new);
        stats.tool_call_count += 1;
        stats.last_updated = Utc::now();
        // Recompute a simple rolling average (treat every call as its own "task").
        stats.avg_tools_per_task = stats.tool_call_count as f64;
    }

    /// Records a permission denial for `agent_id`.
    ///
    /// Returns `Some(AnomalyAlert)` if the agent's anomaly score now equals or
    /// exceeds `ALERT_THRESHOLD` (0.6), `None` otherwise.
    pub fn record_denial(&mut self, agent_id: &str) -> Option<AnomalyAlert> {
        let stats = self
            .agent_stats
            .entry(agent_id.to_owned())
            .or_insert_with(AgentBehaviorStats::new);
        stats.permission_denial_count += 1;
        stats.last_updated = Utc::now();

        let score = compute_score(stats);
        if score >= ALERT_THRESHOLD {
            Some(AnomalyAlert {
                agent_id: agent_id.to_owned(),
                score,
                reason: format!(
                    "permission denial count ({}) exceeded threshold",
                    stats.permission_denial_count
                ),
            })
        } else {
            None
        }
    }

    /// Returns the current anomaly score for `agent_id`, or `0.0` if unknown.
    pub fn score(&self, agent_id: &str) -> f64 {
        self.agent_stats
            .get(agent_id)
            .map(compute_score)
            .unwrap_or(0.0)
    }

    /// Remove entries that have not been updated within `max_age`.
    ///
    /// Call periodically (e.g. every 10 minutes from the kernel's timeout
    /// checker) to prevent the HashMap from growing unboundedly as short-lived
    /// sub-agents accumulate entries.
    pub fn prune_stale(&mut self, max_age: std::time::Duration) {
        let cutoff =
            Utc::now() - chrono::Duration::from_std(max_age).unwrap_or(chrono::Duration::hours(1));
        self.agent_stats.retain(|_, s| s.last_updated >= cutoff);
    }

    /// Return the number of agents currently tracked.
    pub fn len(&self) -> usize {
        self.agent_stats.len()
    }

    /// Returns `true` if no agents are tracked.
    pub fn is_empty(&self) -> bool {
        self.agent_stats.is_empty()
    }
}

/// Heuristic score computation.
///
/// - Each permission denial contributes `0.1` to the score, capped at `1.0`.
/// - More than 5 rolling denials makes the score exceed the 0.6 alert threshold.
fn compute_score(stats: &AgentBehaviorStats) -> f64 {
    (stats.permission_denial_count as f64 * 0.1).min(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_agent_has_zero_score() {
        let scorer = AnomalyScorer::new();
        assert_eq!(scorer.score("agent-1"), 0.0);
    }

    #[test]
    fn tool_calls_recorded_do_not_raise_alert() {
        let mut scorer = AnomalyScorer::new();
        for _ in 0..20 {
            scorer.record_tool_call("agent-2");
        }
        assert!(scorer.score("agent-2") < ALERT_THRESHOLD);
    }

    #[test]
    fn six_denials_triggers_alert() {
        let mut scorer = AnomalyScorer::new();
        let mut alert = None;
        for _ in 0..6 {
            alert = scorer.record_denial("agent-3");
        }
        assert!(alert.is_some(), "expected alert after 6 denials");
        let a = alert.unwrap();
        assert_eq!(a.agent_id, "agent-3");
        assert!(a.score >= ALERT_THRESHOLD);
    }

    #[test]
    fn five_denials_no_alert() {
        let mut scorer = AnomalyScorer::new();
        let mut alert = None;
        for _ in 0..5 {
            alert = scorer.record_denial("agent-4");
        }
        // Five denials produce 0.5 which is below 0.6
        assert!(alert.is_none(), "should not alert at exactly 5 denials");
    }

    #[test]
    fn score_caps_at_one() {
        let mut scorer = AnomalyScorer::new();
        for _ in 0..100 {
            scorer.record_denial("agent-5");
        }
        assert!(scorer.score("agent-5") <= 1.0);
    }

    #[test]
    fn alert_contains_reason_text() {
        let mut scorer = AnomalyScorer::new();
        let mut last = None;
        for _ in 0..7 {
            last = scorer.record_denial("agent-6");
        }
        let alert = last.expect("expected alert");
        assert!(!alert.reason.is_empty());
        assert!(alert.reason.contains("permission denial count"));
    }

    #[test]
    fn multiple_agents_tracked_independently() {
        let mut scorer = AnomalyScorer::new();
        for _ in 0..10 {
            scorer.record_denial("noisy-agent");
        }
        scorer.record_tool_call("quiet-agent");
        assert!(scorer.score("noisy-agent") >= ALERT_THRESHOLD);
        assert!(scorer.score("quiet-agent") < ALERT_THRESHOLD);
    }

    #[test]
    fn prune_stale_removes_old_entries() {
        let mut scorer = AnomalyScorer::new();
        scorer.record_tool_call("agent-a");
        scorer.record_tool_call("agent-b");
        assert_eq!(scorer.len(), 2);

        // Prune with zero duration — everything is stale
        scorer.prune_stale(std::time::Duration::from_secs(0));
        // After prune with 0s, all entries are stale
        assert_eq!(scorer.len(), 0);
    }

    #[test]
    fn prune_stale_retains_recent_entries() {
        let mut scorer = AnomalyScorer::new();
        scorer.record_tool_call("fresh-agent");
        assert_eq!(scorer.len(), 1);

        // Prune with 1-hour max age — the fresh entry should survive
        scorer.prune_stale(std::time::Duration::from_secs(3600));
        assert_eq!(scorer.len(), 1);
    }
}
