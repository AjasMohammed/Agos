use crate::state::AppState;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use minijinja::context;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Default)]
pub struct CostQuery {
    pub agent_id: Option<String>,
}

/// GET /costs — cost dashboard page.
pub async fn dashboard(
    State(state): State<AppState>,
    Query(query): Query<CostQuery>,
    jar: CookieJar,
) -> Response {
    let snapshots = state.kernel.cost_tracker.get_all_snapshots().await;

    // Filter by agent_id if provided.
    let filtered: Vec<_> = if let Some(ref agent_filter) = query.agent_id {
        snapshots
            .into_iter()
            .filter(|s| s.agent_id.to_string().starts_with(agent_filter))
            .collect()
    } else {
        snapshots
    };

    // Compute totals.
    let total_cost_usd: f64 = filtered.iter().map(|s| s.cost_usd).sum();
    let total_tokens: u64 = filtered.iter().map(|s| s.tokens_used).sum();
    let total_tool_calls: u64 = filtered.iter().map(|s| s.tool_calls).sum();

    // Per-agent breakdown.
    let by_agent: Vec<_> = filtered
        .iter()
        .map(|s| {
            let budget_status = if s.cost_pct >= 100.0 {
                "exceeded"
            } else if s.cost_pct >= 80.0 {
                "warning"
            } else {
                "ok"
            };
            let tokens_status = if s.tokens_pct >= 100.0 {
                "exceeded"
            } else if s.tokens_pct >= 80.0 {
                "warning"
            } else {
                "ok"
            };
            context! {
                agent_id => s.agent_id.to_string(),
                agent_name => s.agent_name.clone(),
                cost_usd => format!("{:.6}", s.cost_usd),
                cost_usd_raw => s.cost_usd,
                tokens_used => s.tokens_used,
                tool_calls => s.tool_calls,
                cost_pct => format!("{:.1}", s.cost_pct),
                tokens_pct => format!("{:.1}", s.tokens_pct),
                tool_calls_pct => format!("{:.1}", s.tool_calls_pct),
                max_cost_usd_per_day => format!("{:.2}", s.budget.max_cost_usd_per_day),
                max_tokens_per_day => s.budget.max_tokens_per_day,
                has_cost_budget => s.budget.max_cost_usd_per_day > 0.0,
                has_token_budget => s.budget.max_tokens_per_day > 0,
                budget_status,
                tokens_status,
                period_start => s.period_start.format("%Y-%m-%d %H:%M UTC").to_string(),
            }
        })
        .collect();

    // Count how many running tasks there are.
    let tasks = state.kernel.scheduler.list_tasks().await;
    let running_tasks = tasks
        .iter()
        .filter(|t| {
            matches!(
                t.state,
                agentos_types::TaskState::Running | agentos_types::TaskState::Waiting
            )
        })
        .count();

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);

    let ctx = context! {
        page_title => "Cost Dashboard",
        breadcrumbs => vec![context! { label => "Costs" }],
        total_cost_usd => format!("{:.6}", total_cost_usd),
        total_tokens,
        total_tool_calls,
        running_tasks,
        agent_count => by_agent.len(),
        by_agent,
        csrf_token,
    };
    super::render(&state.templates, "costs/dashboard.html", ctx)
}

/// GET /api/costs/summary — JSON API for cost data.
pub async fn summary_json(
    State(state): State<AppState>,
    Query(query): Query<CostQuery>,
) -> Response {
    let snapshots = state.kernel.cost_tracker.get_all_snapshots().await;

    let filtered: Vec<_> = if let Some(ref agent_filter) = query.agent_id {
        snapshots
            .into_iter()
            .filter(|s| s.agent_id.to_string().starts_with(agent_filter))
            .collect()
    } else {
        snapshots
    };

    let total_cost_usd: f64 = filtered.iter().map(|s| s.cost_usd).sum();
    let total_tokens: u64 = filtered.iter().map(|s| s.tokens_used).sum();
    let total_tool_calls: u64 = filtered.iter().map(|s| s.tool_calls).sum();

    let by_agent: Vec<AgentCostEntry> = filtered
        .iter()
        .map(|s| AgentCostEntry {
            agent_id: s.agent_id.to_string(),
            agent_name: s.agent_name.clone(),
            cost_usd: s.cost_usd,
            tokens_used: s.tokens_used,
            tool_calls: s.tool_calls,
            cost_pct: s.cost_pct,
            tokens_pct: s.tokens_pct,
        })
        .collect();

    let summary = CostSummary {
        total_cost_usd,
        total_tokens,
        total_tool_calls,
        by_agent,
    };

    axum::Json(summary).into_response()
}

#[derive(Serialize)]
struct CostSummary {
    total_cost_usd: f64,
    total_tokens: u64,
    total_tool_calls: u64,
    by_agent: Vec<AgentCostEntry>,
}

#[derive(Serialize)]
struct AgentCostEntry {
    agent_id: String,
    agent_name: String,
    cost_usd: f64,
    tokens_used: u64,
    tool_calls: u64,
    cost_pct: f64,
    tokens_pct: f64,
}
