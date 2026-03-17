use crate::state::AppState;
use axum::extract::{Query, State};
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
                details => {
                    let s = e.details.to_string();
                    let limit = s.char_indices().nth(80).map(|(i, _)| i).unwrap_or(s.len());
                    if limit < s.len() { format!("{}…", &s[..limit]) } else { s }
                },
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
        entries => rows,
        total_count,
        csrf_token,
    };
    super::render(&state.templates, "audit.html", ctx)
}
