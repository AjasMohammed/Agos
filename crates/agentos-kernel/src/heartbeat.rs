//! Unified agent heartbeat — periodic, opt-in wakeups for idle agents.
//!
//! Inspired by paperclip's heartbeat orchestrator, but built by COMPOSING the
//! existing primitives, not adding a parallel scheduler: the [`HeartbeatRunner`]
//! is a pure selector of "which agents are due to wake", and the kernel's
//! existing `TimeoutChecker` tick (run_loop) calls it, checks each due agent's
//! inbox / owned schedules, and — only if there is work — enqueues a lightweight
//! check turn through the normal background-task path.
//!
//! This module holds only the pure selection logic so it is unit-testable
//! without a live kernel. Disabled by default (`default_interval_secs = 0`).

use agentos_types::{AgentID, AgentProfile, AgentStatus};
use chrono::{DateTime, Utc};
use std::hash::{Hash, Hasher};

/// Pure selector for heartbeat wakeups.
pub struct HeartbeatRunner;

impl HeartbeatRunner {
    /// Return the agents whose heartbeat interval has elapsed and that are in a
    /// wakeable state, **most-overdue first**. Pure: no I/O, deterministic given
    /// its inputs.
    ///
    /// An agent is due when:
    /// - the effective interval is non-zero (`default_interval == 0` ⇒ disabled),
    /// - its status is `Online` or `Idle` (never `Offline`), and
    /// - `now - last_active >= jittered_interval`.
    ///
    /// Results are sorted by elapsed-since-`last_active` descending so that when
    /// the caller caps wakes per tick (`max_wakes_per_tick`), the most-overdue
    /// agents win and no agent is starved by unspecified registry iteration order.
    ///
    /// Note: there is no in-flight-task signal to filter on (agent status is not
    /// flipped to `Busy` during execution), so the re-fire guard is the caller
    /// resetting `last_active` after a wake — see run_loop. Jitter (0.0–1.0)
    /// lengthens each agent's interval by a deterministic per-agent fraction so a
    /// large idle fleet doesn't wake in lockstep.
    pub fn due_agents<'a, I>(
        profiles: I,
        now: DateTime<Utc>,
        default_interval_secs: u64,
        jitter: f64,
    ) -> Vec<AgentID>
    where
        I: IntoIterator<Item = &'a AgentProfile>,
    {
        if default_interval_secs == 0 {
            return Vec::new();
        }
        let mut due: Vec<(chrono::Duration, AgentID)> = profiles
            .into_iter()
            .filter(|p| matches!(p.status, AgentStatus::Online | AgentStatus::Idle))
            .filter_map(|p| {
                let interval = Self::jittered_interval(&p.id, default_interval_secs, jitter);
                let elapsed = now.signed_duration_since(p.last_active);
                // Clamp the cast so an absurd config can't wrap to a negative
                // Duration (which would make every agent perpetually "due").
                let interval = chrono::Duration::seconds(interval.min(i64::MAX as u64) as i64);
                (elapsed >= interval).then_some((elapsed, p.id))
            })
            .collect();
        // Most-overdue first; tie-break by id for deterministic ordering.
        due.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        due.into_iter().map(|(_, id)| id).collect()
    }

    /// Deterministic per-agent interval: `base * (1 + jitter * frac)` where
    /// `frac ∈ [0,1)` is derived from a stable hash of the `AgentID`. Same id ⇒
    /// same interval across calls (tests rely on this).
    pub fn jittered_interval(id: &AgentID, base_secs: u64, jitter: f64) -> u64 {
        if jitter <= 0.0 {
            return base_secs;
        }
        let jitter = jitter.clamp(0.0, 1.0);
        let mut h = std::collections::hash_map::DefaultHasher::new();
        id.hash(&mut h);
        // Map the hash to a fraction in [0,1).
        let frac = (h.finish() % 10_000) as f64 / 10_000.0;
        (base_secs as f64 * (1.0 + jitter * frac)) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::{LLMProvider, PermissionSet, ThinkingLevel};

    fn agent(status: AgentStatus, last_active: DateTime<Utc>) -> AgentProfile {
        AgentProfile {
            id: AgentID::new(),
            name: "a".into(),
            provider: LLMProvider::Ollama,
            model: "m".into(),
            status,
            permissions: PermissionSet::default(),
            roles: Vec::new(),
            current_task: None,
            description: String::new(),
            created_at: Utc::now(),
            last_active,
            public_key_hex: None,
            base_url: None,
            default_thinking_level: ThinkingLevel::default(),
            system_prompt: None,
            manually_offline: false,
        }
    }

    #[test]
    fn due_agents_respects_interval() {
        let now = Utc::now();
        // No jitter so the interval is exactly `base`.
        let stale = agent(AgentStatus::Online, now - chrono::Duration::seconds(120));
        let fresh = agent(AgentStatus::Online, now - chrono::Duration::seconds(10));
        let profiles = vec![stale.clone(), fresh.clone()];
        let due = HeartbeatRunner::due_agents(&profiles, now, 60, 0.0);
        assert_eq!(due, vec![stale.id]);
    }

    #[test]
    fn due_agents_skips_busy() {
        let now = Utc::now();
        let busy = agent(AgentStatus::Busy, now - chrono::Duration::seconds(600));
        let offline = agent(AgentStatus::Offline, now - chrono::Duration::seconds(600));
        let profiles = vec![busy, offline];
        let due = HeartbeatRunner::due_agents(&profiles, now, 60, 0.0);
        assert!(due.is_empty(), "busy/offline agents must never be woken");
    }

    #[test]
    fn due_agents_zero_interval_disabled() {
        let now = Utc::now();
        let stale = agent(AgentStatus::Online, now - chrono::Duration::seconds(99_999));
        let profiles = vec![stale];
        assert!(HeartbeatRunner::due_agents(&profiles, now, 0, 0.2).is_empty());
    }

    #[test]
    fn due_agents_sorted_most_overdue_first() {
        let now = Utc::now();
        let recent = agent(AgentStatus::Online, now - chrono::Duration::seconds(70));
        let ancient = agent(AgentStatus::Online, now - chrono::Duration::seconds(9000));
        let middle = agent(AgentStatus::Idle, now - chrono::Duration::seconds(300));
        // Insertion order deliberately not staleness order.
        let profiles = vec![recent.clone(), ancient.clone(), middle.clone()];
        let due = HeartbeatRunner::due_agents(&profiles, now, 60, 0.0);
        // Most-overdue first so a per-tick cap wakes the neediest agents.
        assert_eq!(due, vec![ancient.id, middle.id, recent.id]);
    }

    #[test]
    fn due_agents_jitter_is_deterministic() {
        let id = AgentID::new();
        let a = HeartbeatRunner::jittered_interval(&id, 100, 0.5);
        let b = HeartbeatRunner::jittered_interval(&id, 100, 0.5);
        assert_eq!(a, b, "same id must yield the same jittered interval");
        assert!(
            (100..=150).contains(&a),
            "jittered interval within [base, base*1.5]"
        );
    }
}
