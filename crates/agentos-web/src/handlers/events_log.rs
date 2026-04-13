use crate::state::AppState;
use axum::extract::{Query, State};
use axum::response::Response;
use axum_extra::extract::CookieJar;
use minijinja::context;
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
pub struct EventsQuery {
    pub q: Option<String>,
    pub event_type: Option<String>,
    pub agent_id: Option<String>,
    pub limit: Option<u32>,
}

pub async fn page(
    State(state): State<AppState>,
    Query(query): Query<EventsQuery>,
    jar: CookieJar,
) -> Response {
    let subscriptions = state
        .kernel
        .event_bus
        .list_subscriptions()
        .await
        .into_iter()
        .map(|s| {
            context! {
                id => s.id.to_string(),
                agent_id => s.agent_id.to_string(),
                event_type_filter => format!("{:?}", s.event_type_filter),
                filter => s.filter.unwrap_or_default(),
                enabled => s.enabled,
                created_at => s.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                throttle_policy => format!("{:?}", s.throttle),
                priority => format!("{:?}", s.priority),
            }
        })
        .collect::<Vec<_>>();

    let q = query.q.unwrap_or_default().to_lowercase();
    let event_type_filter = query.event_type.unwrap_or_default().to_lowercase();
    let agent_id_filter = query.agent_id.unwrap_or_default().to_lowercase();
    let limit = query.limit.unwrap_or(200).clamp(1, 1000);
    let fetch_limit = limit.saturating_mul(5).min(5000);

    let mut event_rows = {
        let audit = state.kernel.audit.clone();
        tokio::task::spawn_blocking(move || audit.query_recent(fetch_limit).unwrap_or_default())
            .await
            .unwrap_or_default()
    }
    .into_iter()
    .filter(|entry| entry.event_type == agentos_audit::AuditEventType::EventEmitted)
    .filter(|entry| {
        if !q.is_empty() {
            let details = entry.details.to_string().to_lowercase();
            if !details.contains(&q) {
                return false;
            }
        }
        true
    })
    .filter(|entry| {
        if event_type_filter.is_empty() {
            true
        } else {
            entry
                .details
                .get("event_type")
                .and_then(|v| v.as_str())
                .map(|v| v.to_lowercase().contains(&event_type_filter))
                .unwrap_or(false)
        }
    })
    .filter(|entry| {
        if agent_id_filter.is_empty() {
            true
        } else {
            entry
                .agent_id
                .map(|id| id.to_string().to_lowercase().contains(&agent_id_filter))
                .unwrap_or(false)
        }
    })
    .map(|entry| {
        context! {
            timestamp => entry.timestamp.format("%Y-%m-%d %H:%M:%S").to_string(),
            trace_id => entry.trace_id.to_string(),
            event_type => entry.details.get("event_type").and_then(|v| v.as_str()).unwrap_or_default(),
            event_source => entry.details.get("source").and_then(|v| v.as_str()).unwrap_or_default(),
            severity => format!("{:?}", entry.severity),
            agent_id => entry.agent_id.map(|id| id.to_string()).unwrap_or_default(),
            task_id => entry.task_id.map(|id| id.to_string()).unwrap_or_default(),
            details => entry.details,
        }
    })
    .collect::<Vec<_>>();
    if event_rows.len() > limit as usize {
        event_rows.truncate(limit as usize);
    }

    let categories = agentos_kernel::event_permissions::ALL_EVENT_CATEGORIES
        .iter()
        .map(|cat| {
            context! {
                name => format!("{:?}", cat),
                permission => agentos_kernel::event_permissions::permission_for_category(*cat),
            }
        })
        .collect::<Vec<_>>();

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title => "Events",
        breadcrumbs => vec![
            context! { label => "Observability", href => "/observability" },
            context! { label => "Events" },
        ],
        subscriptions,
        categories,
        event_rows,
        q,
        event_type_filter,
        agent_id_filter,
        limit,
        csrf_token,
    };
    super::render(&state.templates, "events_log.html", ctx)
}
