use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use minijinja::context;
use serde::Deserialize;

#[derive(Deserialize, Default)]
pub struct ListQuery {
    pub partial: Option<String>,
    pub limit: Option<u32>,
    pub event_type: Option<String>,
}

pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
    jar: CookieJar,
) -> Response {
    let requested = query.limit.unwrap_or(50);
    if requested > 1000 {
        tracing::warn!(
            requested = requested,
            capped = 1000,
            "Audit limit clamped to maximum"
        );
    }
    let limit = requested.min(1000);
    let audit = state.kernel.audit.clone();
    let (entries, total_count) = match tokio::task::spawn_blocking(move || {
        let entries = audit.query_recent(limit).unwrap_or_default();
        let total_count = audit.count().unwrap_or(0);
        (entries, total_count)
    })
    .await
    {
        Ok(result) => result,
        Err(e) => {
            tracing::error!("audit query panicked: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response();
        }
    };

    let rows: Vec<_> = entries
        .iter()
        .filter(|e| {
            if let Some(ref et) = query.event_type {
                format!("{:?}", e.event_type)
                    .to_lowercase()
                    .contains(&et.to_lowercase())
            } else {
                true
            }
        })
        .map(|e| {
            context! {
                timestamp => e.timestamp.format("%Y-%m-%d %H:%M:%S").to_string(),
                event_type => format!("{:?}", e.event_type),
                severity => format!("{:?}", e.severity),
                agent_id => e.agent_id.as_ref().map(|id| id.to_string()),
                task_id => e.task_id.as_ref().map(|id| id.to_string()),
                tool_id => e.tool_id.as_ref().map(|id| id.to_string()),
                details => e.details.to_string(),
                trace_id => e.trace_id.to_string(),
            }
        })
        .collect();

    if query.partial.as_deref() == Some("list") {
        let ctx = context! { entries => rows };
        return super::render(&state.templates, "partials/log_line.html", ctx);
    }

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);

    let ctx = context! {
        page_title => "Audit Log",
        breadcrumbs => vec![context! { label => "Audit Log" }],
        entries => rows,
        total_count,
        csrf_token,
    };
    super::render(&state.templates, "audit.html", ctx)
}

pub async fn detail(
    State(state): State<AppState>,
    Path(trace_id_str): Path<String>,
    jar: CookieJar,
) -> Response {
    let parsed_uuid = match uuid::Uuid::parse_str(&trace_id_str) {
        Ok(u) => u,
        Err(_) => {
            return (StatusCode::BAD_REQUEST, "Invalid trace ID format").into_response();
        }
    };
    let trace_id = agentos_types::TraceID::from_uuid(parsed_uuid);

    let audit = state.kernel.audit.clone();
    let entries = match tokio::task::spawn_blocking(move || audit.query_by_trace(&trace_id)).await {
        Ok(Ok(entries)) => entries,
        Ok(Err(e)) => {
            tracing::error!("audit query_by_trace failed: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Database error").into_response();
        }
        Err(e) => {
            tracing::error!("audit query_by_trace panicked: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response();
        }
    };

    if entries.is_empty() {
        return (StatusCode::NOT_FOUND, "Audit entry not found").into_response();
    }

    let first = &entries[0];
    let event_type = format!("{:?}", first.event_type);
    let severity = format!("{:?}", first.severity);

    let rows: Vec<_> = entries
        .iter()
        .map(|e| {
            context! {
                timestamp => e.timestamp.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
                timestamp_iso => e.timestamp.to_rfc3339(),
                event_type => format!("{:?}", e.event_type),
                severity => format!("{:?}", e.severity),
                agent_id => e.agent_id.as_ref().map(|id| id.to_string()),
                task_id => e.task_id.as_ref().map(|id| id.to_string()),
                tool_id => e.tool_id.as_ref().map(|id| id.to_string()),
                details => serde_json::to_string_pretty(&e.details)
                    .unwrap_or_else(|_| e.details.to_string()),
                reversible => e.reversible,
                rollback_ref => e.rollback_ref.clone(),
            }
        })
        .collect();

    let short_id = &trace_id_str[..8.min(trace_id_str.len())];
    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title => format!("Audit — {}", short_id),
        breadcrumbs => vec![
            context! { label => "Audit Log", href => "/audit" },
            context! { label => format!("Trace {}", short_id) },
        ],
        trace_id => trace_id_str,
        event_type,
        severity,
        entry_count => rows.len(),
        entries => rows,
        csrf_token,
    };
    super::render(&state.templates, "audit_detail.html", ctx)
}
