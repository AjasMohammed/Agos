use crate::state::AppState;
use axum::extract::{Query, State};
use axum::response::Response;
use axum_extra::extract::CookieJar;
use minijinja::context;
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
pub struct ObsQuery {
    pub scratch_agent: Option<String>,
    pub scratch_q: Option<String>,
}

pub async fn page(
    State(state): State<AppState>,
    Query(query): Query<ObsQuery>,
    jar: CookieJar,
) -> Response {
    let status = state.service.get_status().await.ok();

    let escalations = state
        .kernel
        .escalation_manager
        .list_pending()
        .await
        .into_iter()
        .map(|e| {
            context! {
                id => e.id,
                task_id => e.task_id.to_string(),
                agent_id => e.agent_id.to_string(),
                urgency => e.urgency,
                reason => format!("{:?}", e.reason),
                created_at => e.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                expires_at => e.expires_at.format("%Y-%m-%d %H:%M:%S").to_string(),
            }
        })
        .collect::<Vec<_>>();

    let webhooks = state
        .kernel
        .webhook_registry
        .list_endpoints(None)
        .await
        .into_iter()
        .map(|w| {
            context! {
                id => w.id.to_string(),
                agent_id => w.agent_id.to_string(),
                provider => w.provider,
                active => w.active,
                debounce_seconds => w.debounce_seconds,
                total_received => w.total_received,
                created_at => w.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                last_received_at => w.last_received_at.map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()).unwrap_or_default(),
            }
        })
        .collect::<Vec<_>>();

    let mcp_servers = state
        .kernel
        .mcp_supervisor
        .server_statuses()
        .await
        .into_iter()
        .map(|(name, state, tool_count, stats, note)| {
            context! {
                name,
                state => format!("{:?}", state),
                tool_count,
                total_calls => stats.total_calls,
                failure_count => stats.failure_count,
                avg_latency_ms => format!("{:.1}", stats.avg_latency_ms),
                note => note.unwrap_or_default(),
            }
        })
        .collect::<Vec<_>>();

    let mcp_attachments = state
        .kernel
        .mcp_attachment_store
        .list_all()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|a| {
            context! {
                name => a.name,
                command => a.command.unwrap_or_default(),
                url => a.url.unwrap_or_default(),
                timeout_secs => a.timeout_secs.unwrap_or_default(),
                created_at => a.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                oauth_connector_id => a.oauth_connector_id.unwrap_or_default(),
            }
        })
        .collect::<Vec<_>>();

    let checkpoints = state
        .kernel
        .checkpoint_store
        .list_checkpoints()
        .await
        .unwrap_or_default()
        .into_iter()
        .take(100)
        .map(|c| {
            context! {
                checkpoint_id => c.checkpoint_id,
                task_id => c.task_id.to_string(),
                agent_id => c.agent_id.to_string(),
                step_num => c.step_num,
                updated_at => c.updated_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                schema_version => c.schema_version,
            }
        })
        .collect::<Vec<_>>();

    let agent_names = {
        let reg = state.kernel.agent_registry.read().await;
        reg.list_online()
            .into_iter()
            .map(|a| a.name.clone())
            .collect::<Vec<_>>()
    };

    let selected_agent = query
        .scratch_agent
        .clone()
        .or_else(|| agent_names.first().cloned())
        .unwrap_or_default();
    let scratch_q = query.scratch_q.clone().unwrap_or_default();

    let scratch_pages = if selected_agent.is_empty() {
        Vec::new()
    } else {
        state
            .kernel
            .scratchpad_store
            .list_pages(&selected_agent)
            .await
            .unwrap_or_default()
            .into_iter()
            .take(100)
            .map(|p| {
                context! {
                    id => p.id,
                    title => p.title,
                    tags => p.tags.join(", "),
                    updated_at => p.updated_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                }
            })
            .collect::<Vec<_>>()
    };

    let scratch_results = if selected_agent.is_empty() || scratch_q.trim().is_empty() {
        Vec::new()
    } else {
        state
            .kernel
            .scratchpad_store
            .search(&selected_agent, &scratch_q, &[], 30)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|r| {
                context! {
                    title => r.page.title,
                    rank => format!("{:.3}", r.rank),
                    snippet => r.snippet,
                }
            })
            .collect::<Vec<_>>()
    };

    let recent_audit = state
        .kernel
        .audit
        .query_recent(50)
        .unwrap_or_default()
        .into_iter()
        .map(|e| {
            context! {
                timestamp => e.timestamp.format("%Y-%m-%d %H:%M:%S").to_string(),
                event_type => format!("{:?}", e.event_type),
                severity => format!("{:?}", e.severity),
                details => e.details,
            }
        })
        .collect::<Vec<_>>();

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title => "Observability",
        breadcrumbs => vec![context! { label => "Observability" }],
        status,
        escalations,
        webhooks,
        mcp_servers,
        mcp_attachments,
        checkpoints,
        recent_audit,
        agent_names,
        selected_agent,
        scratch_q,
        scratch_pages,
        scratch_results,
        csrf_token,
    };
    super::render(&state.templates, "observability.html", ctx)
}
