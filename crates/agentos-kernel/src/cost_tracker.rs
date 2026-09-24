use crate::state_store::{KernelStateStore, PersistedCostSnapshot};
use agentos_llm::{
    calculate_inference_cost, default_pricing_table, InferenceCost, ModelPricing, TokenUsage,
};
use agentos_types::*;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Per-agent cost accumulation state.
struct AgentCostState {
    /// Input prompt tokens consumed in current period.
    input_tokens: AtomicU64,
    /// Output/completion tokens consumed in current period.
    output_tokens: AtomicU64,
    /// Tokens consumed (input + output) in current period.
    tokens_used: AtomicU64,
    /// Cost in micro-USD (millionths of a dollar) to avoid floating point.
    cost_micro_usd: AtomicU64,
    /// Tool calls in current period.
    tool_calls: AtomicU64,
    /// Unix timestamp (seconds) of the start of the current budget period.
    /// Stored as `AtomicI64` so it can be updated without a write lock on the
    /// agents map — `compare_exchange` ensures exactly one thread performs the
    /// daily reset even under concurrent inference calls.
    period_start_unix: AtomicI64,
    /// Highest operator-alert tier already delivered for the current period
    /// (0 = none, 1 = warn, 2 = pause/downgrade, 3 = hard limit). Latched so a
    /// budget crossing notifies once per period instead of on every inference.
    /// Reset with the counters when the period rolls.
    alert_tier: AtomicU8,
    /// Separate latch for the `max_tool_calls_per_day` notification. Tool calls
    /// and inference spend are different resources; sharing `alert_tier` meant
    /// a tripped tool-call cap (tier 3) silenced every later cost warning/pause
    /// for the rest of the period.
    tool_limit_notified: std::sync::atomic::AtomicBool,
    /// Budget configuration.
    budget: AgentBudget,
    /// Agent display name (for reporting).
    agent_name: String,
    /// Monotonic version used for out-of-order-safe DB upserts.
    persist_version: AtomicU64,
    /// Serializes the period rollover so `period_start_unix` and the counters
    /// are never observed out of step. A plain `std::sync::Mutex` on purpose:
    /// the critical section is a handful of atomic stores with no `.await`.
    roll_lock: std::sync::Mutex<()>,
}

/// Result of a budget check before/after an inference call.
#[derive(Debug, Clone, PartialEq)]
pub enum BudgetCheckResult {
    /// Within budget — proceed normally.
    Ok,
    /// Soft warning threshold crossed.
    Warning { resource: String, current_pct: f64 },
    /// Pause threshold crossed — agent should be suspended.
    PauseRequired { resource: String, current_pct: f64 },
    /// Hard limit exceeded — enforce BudgetAction.
    HardLimitExceeded {
        resource: String,
        action: BudgetAction,
    },
    /// Pause threshold crossed but a downgrade model is configured — switch models
    /// instead of pausing. The executor should route subsequent LLM calls to
    /// `downgrade_to` (same provider, cheaper model) and continue the task.
    ModelDowngradeRecommended {
        downgrade_to: String,
        provider: String,
        resource: String,
        current_pct: f64,
    },
    /// Model is not in the agent's allowlist.
    ModelNotAllowed { model: String, agent_id: String },
    /// Wall-time limit exceeded for a task.
    WallTimeExceeded { elapsed_secs: u64, limit_secs: u64 },
}

/// Kernel-owned cost tracking for all agents.
pub struct CostTracker {
    agents: RwLock<HashMap<AgentID, AgentCostState>>,
    pricing: RwLock<Vec<ModelPricing>>,
    /// Optional persistence backend for crash-safe counter restoration.
    state_store: Option<Arc<KernelStateStore>>,
    /// Last persisted snapshots (includes agents that are currently disconnected).
    persisted_snapshots: RwLock<HashMap<AgentID, PersistedCostSnapshot>>,
}

impl CostTracker {
    pub fn new() -> Self {
        Self::with_state_store(None)
    }

    pub fn with_state_store(state_store: Option<Arc<KernelStateStore>>) -> Self {
        Self {
            agents: RwLock::new(HashMap::new()),
            pricing: RwLock::new(default_pricing_table()),
            state_store,
            persisted_snapshots: RwLock::new(HashMap::new()),
        }
    }

    /// Hydrate persisted cost snapshots from SQLite at boot.
    pub async fn restore_from_store(&self) -> anyhow::Result<usize> {
        let Some(store) = &self.state_store else {
            return Ok(0);
        };
        let snapshots = store.load_cost_snapshots().await?;
        let restored = snapshots.len();

        let mut cache = self.persisted_snapshots.write().await;
        cache.clear();
        for snapshot in snapshots {
            cache.insert(snapshot.agent_id, snapshot);
        }

        Ok(restored)
    }

    /// Latch for operator budget notifications: returns `true` only the first
    /// time `tier` (1 = warn, 2 = pause/downgrade, 3 = hard limit) is reached
    /// within the current budget period, so a crossing notifies once instead of
    /// on every inference. Escalating to a higher tier still fires; the latch
    /// is cleared when the period rolls. Unknown agent → `false`.
    pub async fn should_alert(&self, agent_id: &AgentID, tier: u8) -> bool {
        let agents = self.agents.read().await;
        match agents.get(agent_id) {
            Some(state) => state.alert_tier.fetch_max(tier, Ordering::AcqRel) < tier,
            None => false,
        }
    }

    /// Once-per-period latch for the tool-call hard-limit notification,
    /// independent of [`Self::should_alert`] (see `tool_limit_notified`).
    pub async fn should_alert_tool_calls(&self, agent_id: &AgentID) -> bool {
        let agents = self.agents.read().await;
        match agents.get(agent_id) {
            Some(state) => !state.tool_limit_notified.swap(true, Ordering::AcqRel),
            None => false,
        }
    }

    /// Length of a budget period. One definition, shared by the live rollover
    /// (`maybe_roll_period`) and its display-only mirror
    /// (`persisted_to_snapshot`) so the two cannot drift apart.
    const PERIOD_SECS: i64 = 24 * 3600;

    /// True when `elapsed` seconds since `period_start` puts us outside the
    /// current period. Signed on purpose: a `period_start` in the *future*
    /// (host clock ahead — VM resumed before NTP, bad RTC, hand-edited row)
    /// yields a negative elapsed and must count as expired, or the lockout
    /// survives every restart.
    fn period_expired(elapsed: i64) -> bool {
        !(0..Self::PERIOD_SECS).contains(&elapsed)
    }

    /// Roll the 24-hour budget period (and zero the counters) if it has elapsed.
    ///
    /// This must run on the *read* paths too, not only when usage is recorded.
    /// Enforcement bails before inference once a hard limit trips, so a rollover
    /// that lived only in `record_inference_with_cost` sat downstream of the very
    /// gate that stopped it — a hard-limited agent could never reach the code
    /// that would have freed it (MA-03).
    ///
    /// The rollover is serialized by `roll_lock` and publishes
    /// `period_start_unix` **last**, with `Release`/`Acquire` ordering against
    /// the zeroing stores. That is the whole invariant this function exists to
    /// hold: *any* caller that observes a non-expired period start is
    /// guaranteed to observe this period's counters, never the last period's.
    ///
    /// The earlier shape published the new timestamp with a `compare_exchange`
    /// and zeroed the counters after, which left a window where a third caller
    /// loaded the new timestamp, computed `elapsed ~= 0`, and then read the
    /// winner's not-yet-zeroed counters as the new period's — a false
    /// `HardLimitExceeded`, which `task_executor` turns into a
    /// `BudgetAction::Suspend`/`Kill` on a live task (H18). It reproduced in
    /// half of the runs of the regression test below.
    ///
    /// Returns `true` only when this call performed the roll. Either way the
    /// counters are authoritative once it returns.
    fn maybe_roll_period(state: &AgentCostState) -> bool {
        let now_ts = chrono::Utc::now().timestamp();
        if !Self::period_expired(now_ts - state.period_start_unix.load(Ordering::Acquire)) {
            return false;
        }
        let _guard = state
            .roll_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Re-check under the lock — another caller may have rolled while we waited.
        if !Self::period_expired(now_ts - state.period_start_unix.load(Ordering::Acquire)) {
            return false;
        }
        state.input_tokens.store(0, Ordering::Relaxed);
        state.output_tokens.store(0, Ordering::Relaxed);
        state.tokens_used.store(0, Ordering::Relaxed);
        state.cost_micro_usd.store(0, Ordering::Relaxed);
        state.tool_calls.store(0, Ordering::Relaxed);
        // Fresh period — the operator gets a new set of budget alerts.
        state.alert_tier.store(0, Ordering::Relaxed);
        state.tool_limit_notified.store(false, Ordering::Relaxed);
        // Publish last: this is what makes the zeroing visible to everyone else.
        state.period_start_unix.store(now_ts, Ordering::Release);
        true
    }

    fn state_to_snapshot(agent_id: AgentID, state: &AgentCostState) -> CostSnapshot {
        Self::maybe_roll_period(state);
        let tokens = state.tokens_used.load(Ordering::Relaxed);
        let cost_micro = state.cost_micro_usd.load(Ordering::Relaxed);
        let calls = state.tool_calls.load(Ordering::Relaxed);
        let cost_usd = cost_micro as f64 / 1_000_000.0;

        let tokens_pct = if state.budget.max_tokens_per_day > 0 {
            (tokens as f64 / state.budget.max_tokens_per_day as f64) * 100.0
        } else {
            0.0
        };
        let cost_pct = if state.budget.max_cost_usd_per_day > 0.0 {
            (cost_usd / state.budget.max_cost_usd_per_day) * 100.0
        } else {
            0.0
        };
        let tool_calls_pct = if state.budget.max_tool_calls_per_day > 0 {
            (calls as f64 / state.budget.max_tool_calls_per_day as f64) * 100.0
        } else {
            0.0
        };

        let period_start =
            chrono::DateTime::from_timestamp(state.period_start_unix.load(Ordering::Relaxed), 0)
                .unwrap_or_default();

        let forecast_exhaustion_hours = Self::forecast_hours(period_start, cost_usd, &state.budget);

        CostSnapshot {
            agent_id,
            agent_name: state.agent_name.clone(),
            period_start,
            tokens_used: tokens,
            cost_usd,
            tool_calls: calls,
            budget: state.budget.clone(),
            tokens_pct,
            cost_pct,
            tool_calls_pct,
            forecast_exhaustion_hours,
        }
    }

    fn persisted_to_snapshot(snapshot: &PersistedCostSnapshot) -> CostSnapshot {
        let budget = AgentBudget::default();
        // Display-only mirror of `maybe_roll_period`. Read paths must not write
        // to the DB, so an in-memory rollover is never persisted: a row whose
        // period expired still carries pre-roll counters, and a disconnected
        // agent would be reported as stale over-100% when it is not actually
        // limited. Same signed-elapsed rule as `maybe_roll_period` — a
        // future-dated `period_start` counts as expired too.
        let expired = Self::period_expired(
            chrono::Utc::now()
                .signed_duration_since(snapshot.period_start)
                .num_seconds(),
        );
        let (tokens_used, cost_usd, tool_calls) = if expired {
            (0, 0.0, 0)
        } else {
            (
                snapshot.input_tokens.saturating_add(snapshot.output_tokens),
                snapshot.total_cost_usd,
                snapshot.tool_calls,
            )
        };
        let tokens_pct = if budget.max_tokens_per_day > 0 {
            (tokens_used as f64 / budget.max_tokens_per_day as f64) * 100.0
        } else {
            0.0
        };
        let cost_pct = if budget.max_cost_usd_per_day > 0.0 {
            (cost_usd / budget.max_cost_usd_per_day) * 100.0
        } else {
            0.0
        };
        let tool_calls_pct = if budget.max_tool_calls_per_day > 0 {
            (tool_calls as f64 / budget.max_tool_calls_per_day as f64) * 100.0
        } else {
            0.0
        };

        let forecast_exhaustion_hours =
            Self::forecast_hours(snapshot.period_start, cost_usd, &budget);

        CostSnapshot {
            agent_id: snapshot.agent_id,
            agent_name: snapshot.agent_name.clone(),
            period_start: snapshot.period_start,
            tokens_used,
            cost_usd,
            tool_calls,
            budget,
            tokens_pct,
            cost_pct,
            tool_calls_pct,
            forecast_exhaustion_hours,
        }
    }

    /// Linear forecast: given current spend and elapsed time since period start,
    /// estimate hours until the cost budget is exhausted. Returns `None` when
    /// the budget is unlimited or no measurable time/cost has elapsed.
    fn forecast_hours(
        period_start: chrono::DateTime<chrono::Utc>,
        cost_usd: f64,
        budget: &AgentBudget,
    ) -> Option<f64> {
        if budget.max_cost_usd_per_day <= 0.0 || cost_usd <= 0.0 {
            return None;
        }
        let elapsed_secs = chrono::Utc::now()
            .signed_duration_since(period_start)
            .num_seconds();
        if elapsed_secs <= 0 {
            return None;
        }
        let burn_rate_per_hour = cost_usd / (elapsed_secs as f64 / 3600.0);
        if burn_rate_per_hour <= 0.0 {
            return None;
        }
        let remaining_usd = budget.max_cost_usd_per_day - cost_usd;
        if remaining_usd <= 0.0 {
            return Some(0.0);
        }
        Some(remaining_usd / burn_rate_per_hour)
    }

    fn next_persisted_snapshot(
        &self,
        agent_id: AgentID,
        state: &AgentCostState,
    ) -> PersistedCostSnapshot {
        let version = state.persist_version.fetch_add(1, Ordering::AcqRel) + 1;
        PersistedCostSnapshot {
            agent_id,
            agent_name: state.agent_name.clone(),
            input_tokens: state.input_tokens.load(Ordering::Relaxed),
            output_tokens: state.output_tokens.load(Ordering::Relaxed),
            total_cost_usd: state.cost_micro_usd.load(Ordering::Relaxed) as f64 / 1_000_000.0,
            tool_calls: state.tool_calls.load(Ordering::Relaxed),
            period_start: chrono::DateTime::from_timestamp(
                state.period_start_unix.load(Ordering::Relaxed),
                0,
            )
            .unwrap_or_default(),
            version,
        }
    }

    async fn persist_snapshot(&self, snapshot: PersistedCostSnapshot) {
        self.persisted_snapshots
            .write()
            .await
            .insert(snapshot.agent_id, snapshot.clone());

        if let Some(store) = &self.state_store {
            if let Err(e) = store.upsert_cost_snapshot(snapshot).await {
                tracing::error!(error = %e, "Failed to persist cost snapshot");
            }
        }
    }

    /// Register an agent with a budget. Call on agent connect.
    pub async fn register_agent(&self, agent_id: AgentID, agent_name: String, budget: AgentBudget) {
        let cached = { self.persisted_snapshots.write().await.remove(&agent_id) };
        let persisted = if cached.is_some() {
            cached
        } else if let Some(store) = &self.state_store {
            match store.load_cost_snapshot(agent_id).await {
                Ok(snapshot) => snapshot,
                Err(e) => {
                    tracing::warn!(
                        agent_id = %agent_id,
                        error = %e,
                        "Failed to load persisted cost snapshot for connecting agent"
                    );
                    None
                }
            }
        } else {
            None
        };

        let (input_tokens, output_tokens, cost_micro_usd, tool_calls, period_start_unix, version) =
            if let Some(snapshot) = persisted {
                (
                    snapshot.input_tokens,
                    snapshot.output_tokens,
                    ((snapshot.total_cost_usd.max(0.0) * 1_000_000.0) as u64),
                    snapshot.tool_calls,
                    snapshot.period_start.timestamp(),
                    snapshot.version,
                )
            } else {
                (0, 0, 0, 0, chrono::Utc::now().timestamp(), 0)
            };

        let total_tokens = input_tokens.saturating_add(output_tokens);
        let state = AgentCostState {
            input_tokens: AtomicU64::new(input_tokens),
            output_tokens: AtomicU64::new(output_tokens),
            tokens_used: AtomicU64::new(total_tokens),
            cost_micro_usd: AtomicU64::new(cost_micro_usd),
            tool_calls: AtomicU64::new(tool_calls),
            period_start_unix: AtomicI64::new(period_start_unix),
            // ponytail: not persisted — a reconnecting agent re-notifies once
            // per tier. Persist alongside the counters if the repeat is noisy.
            alert_tier: AtomicU8::new(0),
            tool_limit_notified: std::sync::atomic::AtomicBool::new(false),
            budget,
            agent_name,
            persist_version: AtomicU64::new(version),
            roll_lock: std::sync::Mutex::new(()),
        };
        self.agents.write().await.insert(agent_id, state);
    }

    /// Remove an agent's cost tracking state.
    pub async fn unregister_agent(&self, agent_id: &AgentID) {
        let removed = self.agents.write().await.remove(agent_id);
        if let Some(state) = removed {
            let snapshot = self.next_persisted_snapshot(*agent_id, &state);
            self.persist_snapshot(snapshot).await;
        }
    }

    /// Check if a model is permitted for the given agent. Returns Ok if the
    /// model is allowed (or if the allowlist is empty). Returns ModelNotAllowed otherwise.
    pub async fn validate_model(&self, agent_id: &AgentID, model_name: &str) -> BudgetCheckResult {
        let agents = self.agents.read().await;
        if let Some(state) = agents.get(agent_id) {
            if !state.budget.allowed_models.is_empty()
                && !state.budget.allowed_models.iter().any(|m| m == model_name)
            {
                return BudgetCheckResult::ModelNotAllowed {
                    model: model_name.to_string(),
                    agent_id: agent_id.to_string(),
                };
            }
        }
        BudgetCheckResult::Ok
    }

    /// Look up pricing for a provider + model. Falls back to wildcard, then zero.
    pub async fn get_pricing(&self, provider: &str, model: &str) -> ModelPricing {
        agentos_llm::lookup_pricing(&self.pricing.read().await, provider, model)
    }

    /// Record an inference call's token usage and cost. Returns the budget check result.
    ///
    /// If `pre_computed_cost` is provided (e.g., from an adapter that computed it),
    /// it is used directly; otherwise cost is calculated from the pricing table.
    pub async fn record_inference_with_cost(
        &self,
        agent_id: &AgentID,
        usage: &TokenUsage,
        provider: &str,
        model: &str,
        pre_computed_cost: Option<&InferenceCost>,
    ) -> BudgetCheckResult {
        let cost_usd = if let Some(c) = pre_computed_cost {
            if c.total_cost_usd.is_finite() {
                c.total_cost_usd
            } else {
                0.0
            }
        } else {
            let pricing = self.get_pricing(provider, model).await;
            let cost = calculate_inference_cost(usage, &pricing);
            if cost.total_cost_usd.is_finite() {
                cost.total_cost_usd
            } else {
                tracing::warn!(
                    provider = %provider,
                    model = %model,
                    "Inference cost is non-finite ({}) — recording as zero",
                    cost.total_cost_usd
                );
                0.0
            }
        };
        let cost_micro = (cost_usd * 1_000_000.0).max(0.0) as u64;

        let (result, snapshot) = {
            let agents = self.agents.read().await;
            let state = match agents.get(agent_id) {
                Some(s) => s,
                None => return BudgetCheckResult::Ok, // untracked agent
            };

            // Reset counters if we've crossed into a new budget period (24 hours).
            // Whether we rolled or another caller did, the counters are zeroed
            // before the new period is visible, so the accumulation below is
            // always against the right period's totals (H18).
            Self::maybe_roll_period(state);

            // Accumulate
            state
                .input_tokens
                .fetch_add(usage.prompt_tokens, Ordering::Relaxed);
            state
                .output_tokens
                .fetch_add(usage.completion_tokens, Ordering::Relaxed);
            let new_tokens = state
                .tokens_used
                .fetch_add(usage.total_tokens, Ordering::Relaxed)
                + usage.total_tokens;
            let new_cost_micro = state
                .cost_micro_usd
                .fetch_add(cost_micro, Ordering::Relaxed)
                + cost_micro;

            // Check limits
            let result = self.check_limits(state, new_tokens, new_cost_micro);
            let snapshot = self.next_persisted_snapshot(*agent_id, state);
            (result, snapshot)
        };

        self.persist_snapshot(snapshot).await;
        result
    }

    /// Record an inference call using the pricing table for cost calculation.
    /// Backward-compatible wrapper for `record_inference_with_cost`.
    pub async fn record_inference(
        &self,
        agent_id: &AgentID,
        usage: &TokenUsage,
        provider: &str,
        model: &str,
    ) -> BudgetCheckResult {
        self.record_inference_with_cost(agent_id, usage, provider, model, None)
            .await
    }

    /// Read-only budget check against current counters — does NOT accumulate any usage.
    /// Use this for pre-inference checks to avoid wasting tokens on an exhausted budget.
    pub async fn check_budget(&self, agent_id: &AgentID) -> BudgetCheckResult {
        let agents = self.agents.read().await;
        let state = match agents.get(agent_id) {
            Some(s) => s,
            None => return BudgetCheckResult::Ok,
        };
        // Roll first: the counters read below are this period's either way.
        Self::maybe_roll_period(state);
        let tokens = state.tokens_used.load(Ordering::Relaxed);
        let cost_micro = state.cost_micro_usd.load(Ordering::Relaxed);
        self.check_limits(state, tokens, cost_micro)
    }

    /// Record a tool call. Returns the budget check result.
    pub async fn record_tool_call(&self, agent_id: &AgentID) -> BudgetCheckResult {
        let (result, snapshot) = {
            let agents = self.agents.read().await;
            let state = match agents.get(agent_id) {
                Some(s) => s,
                None => return BudgetCheckResult::Ok,
            };

            // W2: same rolled-period rule as `record_inference_with_cost` — a
            // rollover (ours or a concurrent CAS winner's) makes this the first
            // call of a fresh period, not the Nth of the old one.
            Self::maybe_roll_period(state);
            let new_calls = state.tool_calls.fetch_add(1, Ordering::Relaxed) + 1;

            let result = if state.budget.max_tool_calls_per_day > 0 {
                let pct = (new_calls as f64 / state.budget.max_tool_calls_per_day as f64) * 100.0;
                if pct >= 100.0 {
                    BudgetCheckResult::HardLimitExceeded {
                        resource: "tool_calls".into(),
                        action: state.budget.on_hard_limit,
                    }
                } else if pct >= state.budget.pause_at_pct as f64 {
                    BudgetCheckResult::PauseRequired {
                        resource: "tool_calls".into(),
                        current_pct: pct,
                    }
                } else if pct >= state.budget.warn_at_pct as f64 {
                    BudgetCheckResult::Warning {
                        resource: "tool_calls".into(),
                        current_pct: pct,
                    }
                } else {
                    BudgetCheckResult::Ok
                }
            } else {
                BudgetCheckResult::Ok
            };

            let snapshot = self.next_persisted_snapshot(*agent_id, state);
            (result, snapshot)
        };

        self.persist_snapshot(snapshot).await;
        result
    }

    /// Check if a task has exceeded its wall-time budget.
    /// Returns `WallTimeExceeded` if the elapsed time since `task_started_at` exceeds
    /// the agent's `max_wall_time_seconds` budget.
    pub async fn check_wall_time(
        &self,
        agent_id: &AgentID,
        task_started_at: chrono::DateTime<chrono::Utc>,
    ) -> BudgetCheckResult {
        let agents = self.agents.read().await;
        let state = match agents.get(agent_id) {
            Some(s) => s,
            None => return BudgetCheckResult::Ok,
        };

        Self::maybe_roll_period(state);
        if state.budget.max_wall_time_seconds > 0 {
            let elapsed = chrono::Utc::now()
                .signed_duration_since(task_started_at)
                .num_seconds()
                .max(0) as u64;
            if elapsed >= state.budget.max_wall_time_seconds {
                return BudgetCheckResult::WallTimeExceeded {
                    elapsed_secs: elapsed,
                    limit_secs: state.budget.max_wall_time_seconds,
                };
            }
        }

        BudgetCheckResult::Ok
    }

    /// Get a cost snapshot for a specific agent.
    pub async fn get_snapshot(&self, agent_id: &AgentID) -> Option<CostSnapshot> {
        {
            let agents = self.agents.read().await;
            if let Some(state) = agents.get(agent_id) {
                return Some(Self::state_to_snapshot(*agent_id, state));
            }
        }

        let cache = self.persisted_snapshots.read().await;
        cache.get(agent_id).map(Self::persisted_to_snapshot)
    }

    /// Get cost snapshots for all tracked agents.
    pub async fn get_all_snapshots(&self) -> Vec<CostSnapshot> {
        let mut snapshots = Vec::new();
        let mut active_ids = HashSet::new();

        {
            let agents = self.agents.read().await;
            for (id, state) in agents.iter() {
                snapshots.push(Self::state_to_snapshot(*id, state));
                active_ids.insert(*id);
            }
        }

        let cache = self.persisted_snapshots.read().await;
        for (id, snapshot) in cache.iter() {
            if active_ids.contains(id) {
                continue;
            }
            snapshots.push(Self::persisted_to_snapshot(snapshot));
        }

        snapshots
    }

    fn check_limits(
        &self,
        state: &AgentCostState,
        tokens: u64,
        cost_micro: u64,
    ) -> BudgetCheckResult {
        let budget = &state.budget;
        let cost_usd = cost_micro as f64 / 1_000_000.0;

        // Check token limit
        if budget.max_tokens_per_day > 0 {
            let pct = (tokens as f64 / budget.max_tokens_per_day as f64) * 100.0;
            if pct >= 100.0 {
                return BudgetCheckResult::HardLimitExceeded {
                    resource: "tokens".into(),
                    action: budget.on_hard_limit,
                };
            }
            if pct >= budget.pause_at_pct as f64 {
                // If a downgrade model is configured, recommend it instead of pausing
                if let Some(ref tier) = budget.downgrade_model {
                    return BudgetCheckResult::ModelDowngradeRecommended {
                        downgrade_to: tier.model.clone(),
                        provider: tier.provider.clone(),
                        resource: "tokens".into(),
                        current_pct: pct,
                    };
                }
                return BudgetCheckResult::PauseRequired {
                    resource: "tokens".into(),
                    current_pct: pct,
                };
            }
            if pct >= budget.warn_at_pct as f64 {
                return BudgetCheckResult::Warning {
                    resource: "tokens".into(),
                    current_pct: pct,
                };
            }
        }

        // Check cost limit
        if budget.max_cost_usd_per_day > 0.0 {
            let pct = (cost_usd / budget.max_cost_usd_per_day) * 100.0;
            if pct >= 100.0 {
                return BudgetCheckResult::HardLimitExceeded {
                    resource: "cost_usd".into(),
                    action: budget.on_hard_limit,
                };
            }
            if pct >= budget.pause_at_pct as f64 {
                if let Some(ref tier) = budget.downgrade_model {
                    return BudgetCheckResult::ModelDowngradeRecommended {
                        downgrade_to: tier.model.clone(),
                        provider: tier.provider.clone(),
                        resource: "cost_usd".into(),
                        current_pct: pct,
                    };
                }
                return BudgetCheckResult::PauseRequired {
                    resource: "cost_usd".into(),
                    current_pct: pct,
                };
            }
            if pct >= budget.warn_at_pct as f64 {
                return BudgetCheckResult::Warning {
                    resource: "cost_usd".into(),
                    current_pct: pct,
                };
            }
        }

        BudgetCheckResult::Ok
    }
}

impl Default for CostTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_cost_tracker_basic() {
        let tracker = CostTracker::new();
        let agent_id = AgentID::new();
        let budget = AgentBudget {
            max_tokens_per_day: 1000,
            max_cost_usd_per_day: 1.0,
            max_tool_calls_per_day: 10,
            warn_at_pct: 80,
            pause_at_pct: 95,
            on_hard_limit: BudgetAction::Suspend,
            downgrade_model: None,
            allowed_models: vec![],
            max_wall_time_seconds: 0,
        };
        tracker
            .register_agent(agent_id, "test-agent".into(), budget)
            .await;

        let usage = TokenUsage {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
        };

        let result = tracker
            .record_inference(&agent_id, &usage, "ollama", "llama3")
            .await;
        assert_eq!(result, BudgetCheckResult::Ok);

        let snap = tracker.get_snapshot(&agent_id).await.unwrap();
        assert_eq!(snap.tokens_used, 150);
        assert_eq!(snap.agent_name, "test-agent");
    }

    #[tokio::test]
    async fn test_cost_tracker_warning() {
        let tracker = CostTracker::new();
        let agent_id = AgentID::new();
        let budget = AgentBudget {
            max_tokens_per_day: 1000,
            max_cost_usd_per_day: 0.0, // unlimited cost
            max_tool_calls_per_day: 0, // unlimited calls
            warn_at_pct: 80,
            pause_at_pct: 95,
            on_hard_limit: BudgetAction::Suspend,
            downgrade_model: None,
            allowed_models: vec![],
            max_wall_time_seconds: 0,
        };
        tracker
            .register_agent(agent_id, "test-agent".into(), budget)
            .await;

        // Use 850 tokens — should trigger warning (85%)
        let usage = TokenUsage {
            prompt_tokens: 500,
            completion_tokens: 350,
            total_tokens: 850,
        };
        let result = tracker
            .record_inference(&agent_id, &usage, "ollama", "llama3")
            .await;
        assert!(matches!(result, BudgetCheckResult::Warning { .. }));
    }

    #[tokio::test]
    async fn test_cost_tracker_hard_limit() {
        let tracker = CostTracker::new();
        let agent_id = AgentID::new();
        let budget = AgentBudget {
            max_tokens_per_day: 100,
            max_cost_usd_per_day: 0.0,
            max_tool_calls_per_day: 0,
            warn_at_pct: 80,
            pause_at_pct: 95,
            on_hard_limit: BudgetAction::Suspend,
            downgrade_model: None,
            allowed_models: vec![],
            max_wall_time_seconds: 0,
        };
        tracker
            .register_agent(agent_id, "test-agent".into(), budget)
            .await;

        let usage = TokenUsage {
            prompt_tokens: 80,
            completion_tokens: 30,
            total_tokens: 110,
        };
        let result = tracker
            .record_inference(&agent_id, &usage, "ollama", "llama3")
            .await;
        assert!(matches!(
            result,
            BudgetCheckResult::HardLimitExceeded { .. }
        ));
    }

    /// MA-03 regression: a hard-limited agent must become callable again once
    /// the 24h period elapses, *without* any intervening
    /// `record_inference_with_cost`. The rollover used to live only inside that
    /// method, which the pre-inference gate never reaches once the limit trips.
    #[tokio::test]
    async fn test_hard_limit_clears_after_period_rollover_without_recording() {
        let tracker = CostTracker::new();
        let agent_id = AgentID::new();
        let budget = AgentBudget {
            max_tokens_per_day: 100,
            max_cost_usd_per_day: 0.0,
            max_tool_calls_per_day: 10,
            warn_at_pct: 80,
            pause_at_pct: 95,
            on_hard_limit: BudgetAction::Suspend,
            downgrade_model: None,
            allowed_models: vec![],
            max_wall_time_seconds: 0,
        };
        tracker
            .register_agent(agent_id, "test-agent".into(), budget)
            .await;

        let usage = TokenUsage {
            prompt_tokens: 80,
            completion_tokens: 30,
            total_tokens: 110,
        };
        tracker
            .record_inference(&agent_id, &usage, "ollama", "llama3")
            .await;
        assert!(matches!(
            tracker.check_budget(&agent_id).await,
            BudgetCheckResult::HardLimitExceeded { .. }
        ));

        // Backdate the period start by 25h. Nothing else happens — in
        // particular no inference is recorded, exactly as when the gate is
        // bailing before every LLM call.
        {
            let agents = tracker.agents.read().await;
            let state = agents.get(&agent_id).unwrap();
            state.period_start_unix.store(
                chrono::Utc::now().timestamp() - 25 * 3600,
                Ordering::Relaxed,
            );
        }

        assert_eq!(
            tracker.check_budget(&agent_id).await,
            BudgetCheckResult::Ok,
            "check_budget must roll the expired period instead of staying locked out"
        );
        assert_eq!(
            tracker.record_tool_call(&agent_id).await,
            BudgetCheckResult::Ok
        );
        let snap = tracker.get_snapshot(&agent_id).await.unwrap();
        assert_eq!(snap.tokens_used, 0, "counters reset for the new period");
        assert_eq!(
            snap.tool_calls, 1,
            "only the post-rollover tool call counts"
        );
    }

    /// W1 regression: a `period_start` in the *future* must still roll. The
    /// old guard subtracted signed timestamps, so a snapshot written while the
    /// host clock was ahead (VM resumed before NTP, bad RTC, hand-edited row)
    /// made `elapsed` negative, passed the guard, and never rolled — and
    /// `register_agent` reseeds `period_start_unix` from that same row, so the
    /// lockout survived every restart.
    #[tokio::test]
    async fn test_future_dated_period_start_still_rolls() {
        let tracker = CostTracker::new();
        let agent_id = AgentID::new();
        let budget = AgentBudget {
            max_tokens_per_day: 100,
            max_cost_usd_per_day: 0.0,
            max_tool_calls_per_day: 0,
            warn_at_pct: 80,
            pause_at_pct: 95,
            on_hard_limit: BudgetAction::Suspend,
            downgrade_model: None,
            allowed_models: vec![],
            max_wall_time_seconds: 0,
        };
        tracker
            .register_agent(agent_id, "clock-skew".into(), budget)
            .await;

        let usage = TokenUsage {
            prompt_tokens: 80,
            completion_tokens: 30,
            total_tokens: 110,
        };
        tracker
            .record_inference(&agent_id, &usage, "ollama", "llama3")
            .await;
        assert!(matches!(
            tracker.check_budget(&agent_id).await,
            BudgetCheckResult::HardLimitExceeded { .. }
        ));

        // Period start 48h in the *future* — as restored from a snapshot
        // written under a fast clock.
        {
            let agents = tracker.agents.read().await;
            let state = agents.get(&agent_id).unwrap();
            state.period_start_unix.store(
                chrono::Utc::now().timestamp() + 48 * 3600,
                Ordering::Relaxed,
            );
        }

        assert_eq!(
            tracker.check_budget(&agent_id).await,
            BudgetCheckResult::Ok,
            "a future-dated period_start must roll, not lock the agent out forever"
        );
        let snap = tracker.get_snapshot(&agent_id).await.unwrap();
        assert_eq!(snap.tokens_used, 0, "counters reset for the new period");
    }

    /// W6 regression: the `compare_exchange` loser must not report the
    /// pre-reset counters as a hard limit. Exactly one of these concurrent
    /// gates wins the CAS; the losers return before its `store(0)` calls land,
    /// so reading `tokens_used` there produced a one-shot false "cannot run
    /// until its daily budget resets" one second after it did reset.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_concurrent_period_rollover_does_not_falsely_hard_limit() {
        let tracker = Arc::new(CostTracker::new());
        let agent_id = AgentID::new();
        let budget = AgentBudget {
            max_tokens_per_day: 100,
            max_cost_usd_per_day: 0.0,
            max_tool_calls_per_day: 0,
            warn_at_pct: 80,
            pause_at_pct: 95,
            on_hard_limit: BudgetAction::Suspend,
            downgrade_model: None,
            allowed_models: vec![],
            max_wall_time_seconds: 0,
        };
        tracker
            .register_agent(agent_id, "race-agent".into(), budget)
            .await;

        let usage = TokenUsage {
            prompt_tokens: 80,
            completion_tokens: 30,
            total_tokens: 110,
        };
        tracker
            .record_inference(&agent_id, &usage, "ollama", "llama3")
            .await;

        // Park the period start just over the boundary, then send several
        // chat turns through the gate at once.
        {
            let agents = tracker.agents.read().await;
            let state = agents.get(&agent_id).unwrap();
            state.period_start_unix.store(
                chrono::Utc::now().timestamp() - 25 * 3600,
                Ordering::Relaxed,
            );
        }

        let mut handles = Vec::new();
        for _ in 0..16 {
            let tracker = Arc::clone(&tracker);
            handles.push(tokio::spawn(async move {
                tracker.check_budget(&agent_id).await
            }));
        }
        for handle in handles {
            assert_eq!(
                handle.await.expect("gate task should not panic"),
                BudgetCheckResult::Ok,
                "a rollover CAS loser must not be reported as hard-limited"
            );
        }
    }

    /// W2 regression: `record_tool_call` and `record_inference_with_cost`
    /// discarded `maybe_roll_period`'s verdict. The `compare_exchange` loser
    /// therefore incremented the **pre-reset** counters and reported
    /// `HardLimitExceeded` — which `task_executor` turns into a
    /// `BudgetAction::Suspend`/`Kill` — for a budget that had just reset. Its
    /// increment was then lost to the winner's `store(0)` as well.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_concurrent_rollover_does_not_falsely_hard_limit_recording() {
        let tracker = Arc::new(CostTracker::new());
        let agent_id = AgentID::new();
        let budget = AgentBudget {
            max_tokens_per_day: 1000,
            max_cost_usd_per_day: 0.0,
            max_tool_calls_per_day: 100,
            warn_at_pct: 80,
            pause_at_pct: 95,
            on_hard_limit: BudgetAction::Kill,
            downgrade_model: None,
            allowed_models: vec![],
            max_wall_time_seconds: 0,
        };
        tracker
            .register_agent(agent_id, "race-record".into(), budget)
            .await;

        // Both budgets sitting at the hard limit, with the period parked just
        // over the 24h boundary — the state every caller sees at a rollover.
        {
            let agents = tracker.agents.read().await;
            let state = agents.get(&agent_id).unwrap();
            state.tokens_used.store(1100, Ordering::Relaxed);
            state.tool_calls.store(100, Ordering::Relaxed);
            state.period_start_unix.store(
                chrono::Utc::now().timestamp() - 25 * 3600,
                Ordering::Relaxed,
            );
        }

        let barrier = Arc::new(tokio::sync::Barrier::new(16));
        let mut handles = Vec::new();
        for i in 0..16 {
            let tracker = Arc::clone(&tracker);
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                if i % 2 == 0 {
                    tracker.record_tool_call(&agent_id).await
                } else {
                    let usage = TokenUsage {
                        prompt_tokens: 1,
                        completion_tokens: 1,
                        total_tokens: 2,
                    };
                    tracker
                        .record_inference(&agent_id, &usage, "ollama", "llama3")
                        .await
                }
            }));
        }
        for handle in handles {
            let result = handle.await.expect("recording task should not panic");
            assert!(
                !matches!(result, BudgetCheckResult::HardLimitExceeded { .. }),
                "a rollover CAS loser must not report a just-reset budget as exceeded, got {:?}",
                result
            );
        }
    }

    /// W5 regression: read paths deliberately never write the rollover back to
    /// SQLite, so a persisted row can outlive its period. The display-only
    /// conversion must mirror the rollover, otherwise the panel and
    /// `agentos cost` show a stale over-100% agent that is not actually limited.
    #[test]
    fn test_persisted_snapshot_zeroes_counters_past_period() {
        let budget = AgentBudget::default();
        let mut persisted = PersistedCostSnapshot {
            agent_id: AgentID::new(),
            agent_name: "stale-agent".into(),
            input_tokens: budget.max_tokens_per_day,
            output_tokens: budget.max_tokens_per_day,
            total_cost_usd: budget.max_cost_usd_per_day * 2.0,
            tool_calls: 99,
            period_start: chrono::Utc::now() - chrono::Duration::hours(25),
            version: 7,
        };

        let snap = CostTracker::persisted_to_snapshot(&persisted);
        assert_eq!(snap.tokens_used, 0, "expired period reports zeroed tokens");
        assert_eq!(snap.cost_usd, 0.0, "expired period reports zeroed cost");
        assert_eq!(snap.tool_calls, 0, "expired period reports zeroed calls");
        assert!(snap.tokens_pct < 100.0 && snap.cost_pct < 100.0);

        // Inside the period the counters are reported verbatim.
        persisted.period_start = chrono::Utc::now() - chrono::Duration::hours(1);
        let snap = CostTracker::persisted_to_snapshot(&persisted);
        assert_eq!(snap.tokens_used, budget.max_tokens_per_day * 2);
        assert_eq!(snap.tool_calls, 99);
    }

    #[tokio::test]
    async fn test_cost_tracker_tool_calls() {
        let tracker = CostTracker::new();
        let agent_id = AgentID::new();
        let budget = AgentBudget {
            max_tokens_per_day: 0,
            max_cost_usd_per_day: 0.0,
            max_tool_calls_per_day: 5,
            warn_at_pct: 80,
            pause_at_pct: 95,
            on_hard_limit: BudgetAction::Kill,
            downgrade_model: None,
            allowed_models: vec![],
            max_wall_time_seconds: 0,
        };
        tracker
            .register_agent(agent_id, "test-agent".into(), budget)
            .await;

        // 4 calls = 80% → warning
        for _ in 0..4 {
            tracker.record_tool_call(&agent_id).await;
        }
        let snap = tracker.get_snapshot(&agent_id).await.unwrap();
        assert_eq!(snap.tool_calls, 4);

        // 5th call = 100% → hard limit
        let result = tracker.record_tool_call(&agent_id).await;
        assert!(matches!(
            result,
            BudgetCheckResult::HardLimitExceeded {
                action: BudgetAction::Kill,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_model_downgrade_recommended_at_pause_pct() {
        let tracker = CostTracker::new();
        let agent_id = AgentID::new();
        let budget = AgentBudget {
            max_tokens_per_day: 1000,
            max_cost_usd_per_day: 0.0,
            max_tool_calls_per_day: 0,
            warn_at_pct: 80,
            pause_at_pct: 95,
            on_hard_limit: BudgetAction::Suspend,
            downgrade_model: Some(agentos_types::ModelDowngradeTier {
                model: "claude-haiku-4-5".to_string(),
                provider: "anthropic".to_string(),
            }),
            allowed_models: vec![],
            max_wall_time_seconds: 0,
        };
        tracker
            .register_agent(agent_id, "test-agent".into(), budget)
            .await;

        // 960 tokens = 96% → should trigger ModelDowngradeRecommended, not PauseRequired
        let usage = TokenUsage {
            prompt_tokens: 600,
            completion_tokens: 360,
            total_tokens: 960,
        };
        let result = tracker
            .record_inference(&agent_id, &usage, "anthropic", "claude-sonnet-4-6")
            .await;
        assert!(
            matches!(result, BudgetCheckResult::ModelDowngradeRecommended { ref downgrade_to, .. } if downgrade_to == "claude-haiku-4-5"),
            "Expected ModelDowngradeRecommended, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_pause_required_without_downgrade_model() {
        let tracker = CostTracker::new();
        let agent_id = AgentID::new();
        let budget = AgentBudget {
            max_tokens_per_day: 1000,
            max_cost_usd_per_day: 0.0,
            max_tool_calls_per_day: 0,
            warn_at_pct: 80,
            pause_at_pct: 95,
            on_hard_limit: BudgetAction::Suspend,
            downgrade_model: None,
            allowed_models: vec![], // no downgrade configured
            max_wall_time_seconds: 0,
        };
        tracker
            .register_agent(agent_id, "test-agent".into(), budget)
            .await;

        let usage = TokenUsage {
            prompt_tokens: 600,
            completion_tokens: 360,
            total_tokens: 960,
        };
        let result = tracker
            .record_inference(&agent_id, &usage, "ollama", "llama3")
            .await;
        // Without downgrade_model, PauseRequired is returned as before
        assert!(matches!(result, BudgetCheckResult::PauseRequired { .. }));
    }

    #[tokio::test]
    async fn test_cost_tracker_usd_pricing() {
        let tracker = CostTracker::new();
        let agent_id = AgentID::new();
        let budget = AgentBudget {
            max_tokens_per_day: 0,      // unlimited
            max_cost_usd_per_day: 0.01, // very low limit
            max_tool_calls_per_day: 0,
            warn_at_pct: 80,
            pause_at_pct: 95,
            on_hard_limit: BudgetAction::Suspend,
            downgrade_model: None,
            allowed_models: vec![],
            max_wall_time_seconds: 0,
        };
        tracker
            .register_agent(agent_id, "test-agent".into(), budget)
            .await;

        // Use Claude Sonnet: $0.003/1K input, $0.015/1K output
        // 1000 input + 500 output = $0.003 + $0.0075 = $0.0105 > $0.01 limit
        let usage = TokenUsage {
            prompt_tokens: 1000,
            completion_tokens: 500,
            total_tokens: 1500,
        };
        let result = tracker
            .record_inference(&agent_id, &usage, "anthropic", "claude-sonnet-4-6")
            .await;
        assert!(matches!(
            result,
            BudgetCheckResult::HardLimitExceeded { .. }
        ));
    }

    #[tokio::test]
    async fn test_model_allowlist_blocks_unauthorized() {
        let tracker = CostTracker::new();
        let agent_id = AgentID::new();
        let budget = AgentBudget {
            allowed_models: vec!["llama3".to_string(), "mistral".to_string()],
            ..Default::default()
        };
        tracker
            .register_agent(agent_id, "test-agent".into(), budget)
            .await;

        // Allowed model
        let result = tracker.validate_model(&agent_id, "llama3").await;
        assert_eq!(result, BudgetCheckResult::Ok);

        // Unauthorized model
        let result = tracker.validate_model(&agent_id, "claude-sonnet").await;
        assert!(
            matches!(result, BudgetCheckResult::ModelNotAllowed { .. }),
            "Unauthorized model should be blocked, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_model_allowlist_empty_allows_all() {
        let tracker = CostTracker::new();
        let agent_id = AgentID::new();
        let budget = AgentBudget {
            allowed_models: vec![], // empty = all allowed
            ..Default::default()
        };
        tracker
            .register_agent(agent_id, "test-agent".into(), budget)
            .await;

        // Any model should be allowed
        let result = tracker.validate_model(&agent_id, "gpt-4o").await;
        assert_eq!(result, BudgetCheckResult::Ok);

        let result = tracker.validate_model(&agent_id, "claude-sonnet").await;
        assert_eq!(result, BudgetCheckResult::Ok);
    }

    #[tokio::test]
    async fn test_persist_and_restore_cost_snapshot() {
        let dir = tempdir().expect("temp dir");
        let db_path = dir.path().join("kernel_state.db");
        let store = Arc::new(
            KernelStateStore::open(db_path)
                .await
                .expect("state store should open"),
        );

        let tracker = CostTracker::with_state_store(Some(store.clone()));
        let agent_id = AgentID::new();
        tracker
            .register_agent(agent_id, "persist-agent".into(), AgentBudget::default())
            .await;

        let usage = TokenUsage {
            prompt_tokens: 120,
            completion_tokens: 80,
            total_tokens: 200,
        };
        let _ = tracker
            .record_inference(&agent_id, &usage, "ollama", "llama3")
            .await;
        let _ = tracker.record_tool_call(&agent_id).await;

        // Ensure latest snapshot is persisted even if the agent disconnects.
        tracker.unregister_agent(&agent_id).await;

        let restored = CostTracker::with_state_store(Some(store));
        restored
            .restore_from_store()
            .await
            .expect("restore should succeed");
        restored
            .register_agent(agent_id, "persist-agent".into(), AgentBudget::default())
            .await;

        let snap = restored
            .get_snapshot(&agent_id)
            .await
            .expect("snapshot exists");
        assert_eq!(snap.tokens_used, 200);
        assert_eq!(snap.tool_calls, 1);
        assert_eq!(snap.agent_name, "persist-agent");
    }

    #[tokio::test]
    async fn test_record_inference_with_precomputed_cost_uses_it() {
        let tracker = CostTracker::new();
        let agent_id = AgentID::new();
        // Very tight cost budget — $0.011 would exceed it.
        let budget = AgentBudget {
            max_tokens_per_day: 1_000_000,
            max_cost_usd_per_day: 0.01,
            max_tool_calls_per_day: 100,
            warn_at_pct: 80,
            pause_at_pct: 95,
            on_hard_limit: BudgetAction::Suspend,
            downgrade_model: None,
            allowed_models: vec![],
            max_wall_time_seconds: 0,
        };
        tracker
            .register_agent(agent_id, "cost-test".into(), budget)
            .await;

        let usage = TokenUsage {
            prompt_tokens: 1000,
            completion_tokens: 500,
            total_tokens: 1500,
        };
        // Pre-computed cost that exceeds the $0.01 budget.
        let precomputed = InferenceCost {
            input_cost_usd: 0.003,
            output_cost_usd: 0.0075,
            total_cost_usd: 0.0105,
        };

        let result = tracker
            .record_inference_with_cost(
                &agent_id,
                &usage,
                "anthropic",
                "claude-sonnet-4-6",
                Some(&precomputed),
            )
            .await;

        // Budget exceeded → HardLimitExceeded
        assert!(
            matches!(result, BudgetCheckResult::HardLimitExceeded { .. }),
            "Expected HardLimitExceeded, got {:?}",
            result
        );
        // Verify the cost was recorded correctly in the snapshot.
        let snap = tracker.get_snapshot(&agent_id).await.unwrap();
        assert!((snap.cost_usd - 0.0105).abs() < 1e-9);
    }

    #[tokio::test]
    async fn test_record_inference_with_nan_cost_records_zero() {
        let tracker = CostTracker::new();
        let agent_id = AgentID::new();
        tracker
            .register_agent(agent_id, "nan-test".into(), AgentBudget::default())
            .await;

        let usage = TokenUsage {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
        };
        // NaN cost — should be sanitized to zero, not corrupt the budget counters.
        let nan_cost = InferenceCost {
            input_cost_usd: f64::NAN,
            output_cost_usd: 0.0,
            total_cost_usd: f64::NAN,
        };

        let result = tracker
            .record_inference_with_cost(&agent_id, &usage, "test", "test", Some(&nan_cost))
            .await;

        assert_eq!(result, BudgetCheckResult::Ok);
        let snap = tracker.get_snapshot(&agent_id).await.unwrap();
        assert_eq!(snap.cost_usd, 0.0, "NaN cost should be sanitized to zero");
        assert_eq!(
            snap.tokens_used, 150,
            "Token usage should still be recorded"
        );
    }

    async fn tracker_with_agent(name: &str) -> (CostTracker, AgentID) {
        let tracker = CostTracker::new();
        let agent_id = AgentID::new();
        tracker
            .register_agent(agent_id, name.into(), AgentBudget::default())
            .await;
        (tracker, agent_id)
    }

    /// The operator gets one notification per tier crossing, not one per
    /// inference: the first `should_alert(1)` fires, later ones do not.
    #[tokio::test]
    async fn test_should_alert_latches_per_tier() {
        let (tracker, agent_id) = tracker_with_agent("latch-agent").await;

        assert!(tracker.should_alert(&agent_id, 1).await);
        assert!(!tracker.should_alert(&agent_id, 1).await);
        assert!(!tracker.should_alert(&agent_id, 1).await);

        // Unknown agents never alert.
        assert!(!tracker.should_alert(&AgentID::new(), 1).await);
    }

    /// Escalation must still notify: a warn already latched at tier 1 must not
    /// swallow the pause notification at tier 2, while tier 1 stays latched.
    #[tokio::test]
    async fn test_should_alert_escalates_to_higher_tier() {
        let (tracker, agent_id) = tracker_with_agent("escalate-agent").await;

        assert!(tracker.should_alert(&agent_id, 1).await);
        assert!(tracker.should_alert(&agent_id, 2).await);
        assert!(!tracker.should_alert(&agent_id, 2).await);
        assert!(
            !tracker.should_alert(&agent_id, 1).await,
            "a lower tier must not re-fire after a higher one latched"
        );
        assert!(tracker.should_alert(&agent_id, 3).await);
    }

    /// The latch is per budget period — once the period rolls, the operator is
    /// notified again for the new period's crossings.
    #[tokio::test]
    async fn test_should_alert_resets_after_period_rollover() {
        let (tracker, agent_id) = tracker_with_agent("roll-agent").await;

        assert!(tracker.should_alert(&agent_id, 2).await);
        assert!(!tracker.should_alert(&agent_id, 1).await);

        // Backdate past the 24h boundary and let a read path roll the period.
        {
            let agents = tracker.agents.read().await;
            let state = agents.get(&agent_id).expect("agent registered");
            state.period_start_unix.store(
                chrono::Utc::now().timestamp() - 25 * 3600,
                Ordering::Relaxed,
            );
        }
        assert_eq!(tracker.check_budget(&agent_id).await, BudgetCheckResult::Ok);

        assert!(
            tracker.should_alert(&agent_id, 1).await,
            "a rolled period must re-arm the operator alert latch"
        );
    }

    /// Tool calls and inference spend are separate resources: a tripped
    /// tool-call cap must not silence a later cost warning, and vice versa.
    #[tokio::test]
    async fn test_tool_call_latch_is_independent_of_tier_latch() {
        let (tracker, agent_id) = tracker_with_agent("tool-agent").await;

        assert!(tracker.should_alert_tool_calls(&agent_id).await);
        assert!(!tracker.should_alert_tool_calls(&agent_id).await);
        assert!(
            tracker.should_alert(&agent_id, 1).await,
            "a tool-call limit must not suppress the first cost warning"
        );

        // Reverse order: a hard cost limit must not suppress the tool-call alert.
        let (tracker, agent_id) = tracker_with_agent("tool-agent-2").await;
        assert!(tracker.should_alert(&agent_id, 3).await);
        assert!(tracker.should_alert_tool_calls(&agent_id).await);

        // Both latches re-arm on rollover.
        {
            let agents = tracker.agents.read().await;
            let state = agents.get(&agent_id).expect("agent registered");
            state.period_start_unix.store(
                chrono::Utc::now().timestamp() - 25 * 3600,
                Ordering::Relaxed,
            );
        }
        assert_eq!(tracker.check_budget(&agent_id).await, BudgetCheckResult::Ok);
        assert!(tracker.should_alert_tool_calls(&agent_id).await);
        assert!(tracker.should_alert(&agent_id, 1).await);
    }
}
