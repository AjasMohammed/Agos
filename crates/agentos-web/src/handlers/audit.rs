use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use chrono::{DateTime, Utc};
use minijinja::context;
use serde::Deserialize;

#[derive(Deserialize, Default)]
pub struct ListQuery {
    pub partial: Option<String>,
    pub limit: Option<u32>,
    pub event_type: Option<String>,
    pub severity: Option<String>,
    pub from_ts: Option<DateTime<Utc>>,
    pub to_ts: Option<DateTime<Utc>>,
}

pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
    jar: CookieJar,
) -> Response {
    use agentos_api::types::AuditFilter;

    let requested = query.limit.unwrap_or(50);
    if requested > 1000 {
        tracing::warn!(
            requested = requested,
            capped = 1000,
            "Audit limit clamped to maximum"
        );
    }
    let limit = requested.min(1000);

    let filter = AuditFilter {
        limit: Some(limit),
        severity: query.severity.clone().filter(|s| !s.is_empty()),
        from: query.from_ts,
        to: query.to_ts,
    };
    let entries = match state.service.query_audit(filter).await {
        Ok(e) => e,
        Err(e) => {
            tracing::error!("audit query failed: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response();
        }
    };
    // total_count not exposed by KernelService; use entries length as fallback
    // TODO: expose total_count in KernelService::query_audit
    let total_count = entries.len();

    // NOTE: event_type filtering is applied client-side here because AuditFilter has no
    // event_type field. This means the `limit` cap applies before the event_type filter,
    // so a narrow event_type query against a busy log may return fewer rows than `limit`.
    // TODO: Add event_type to AuditFilter and push this filter down to the service/DB layer.
    let rows: Vec<_> = entries
        .iter()
        .filter(|e| {
            if let Some(ref et) = query.event_type {
                e.event_type.to_lowercase().contains(&et.to_lowercase())
            } else {
                true
            }
        })
        .map(|e| {
            context! {
                timestamp => e.timestamp.format("%Y-%m-%d %H:%M:%S").to_string(),
                event_type => e.event_type.trim_matches('"').to_string(),
                severity => String::new(),
                agent_id => e.agent_id.clone(),
                task_id => Option::<String>::None,
                tool_id => Option::<String>::None,
                details => e.details.clone(),
                trace_id => String::new(),
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
    if uuid::Uuid::parse_str(&trace_id_str).is_err() {
        return (StatusCode::BAD_REQUEST, "Invalid trace ID format").into_response();
    }

    let entry = match state.service.get_audit_detail(&trace_id_str).await {
        Ok(e) => e,
        Err(agentos_api::ApiError::NotFound(_)) => {
            return (StatusCode::NOT_FOUND, "Audit entry not found").into_response();
        }
        Err(e) => {
            tracing::error!("audit detail query failed: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Database error").into_response();
        }
    };

    let event_type = entry.event_type.trim_matches('"').to_string();
    // TODO: severity and reversible are unimplemented — AuditEntryDetail does not expose
    // these fields yet. Add them to AuditEntryDetail when the audit schema is extended.
    let severity = String::new();

    let details_pretty =
        serde_json::to_string_pretty(&entry.metadata).unwrap_or_else(|_| entry.details.clone());

    let rows: Vec<_> = vec![context! {
        timestamp => entry.timestamp.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        timestamp_iso => entry.timestamp.to_rfc3339(),
        event_type => event_type.clone(),
        severity => severity.clone(),
        agent_id => entry.agent_id.clone(),
        task_id => entry.task_id.clone(),
        tool_id => Option::<String>::None,
        details => details_pretty,
        reversible => false,
        rollback_ref => Option::<String>::None,
    }];

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
