use crate::state::AppState;
use axum::extract::{Query, State};
use axum::response::Response;
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
) -> Response {
    let limit = query.limit.unwrap_or(50);
    let entries = state.kernel.audit.query_recent(limit).unwrap_or_default();

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
            }
        })
        .collect();

    if query.partial.as_deref() == Some("list") {
        let ctx = context! { entries => rows };
        return super::render(&state.templates, "partials/log_line.html", ctx);
    }

    let ctx = context! {
        page_title => "Audit Log",
        entries => rows,
        total_count => state.kernel.audit.count().unwrap_or(0),
    };
    super::render(&state.templates, "audit.html", ctx)
}
