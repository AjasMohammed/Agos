use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use minijinja::context;

pub async fn list(State(state): State<AppState>, jar: CookieJar) -> Response {
    let servers = state
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

    let attachments = state
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
                args => a.args.join(" "),
                url => a.url.unwrap_or_default(),
                timeout_secs => a.timeout_secs.unwrap_or_default(),
                oauth_connector_id => a.oauth_connector_id.unwrap_or_default(),
                created_at => a.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
            }
        })
        .collect::<Vec<_>>();

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title => "MCP",
        breadcrumbs => vec![
            context! { label => "Management", href => "/management" },
            context! { label => "MCP" },
        ],
        servers,
        attachments,
        csrf_token,
    };
    super::render(&state.templates, "mcp_page.html", ctx)
}

pub async fn detach(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    let removed_runtime = state.kernel.mcp_supervisor.remove_server(&name).await;
    let removed_persisted = state
        .kernel
        .mcp_attachment_store
        .delete(&name)
        .await
        .unwrap_or(false);

    if removed_runtime || removed_persisted {
        Redirect::to("/mcp").into_response()
    } else {
        (
            StatusCode::NOT_FOUND,
            format!("MCP server '{}' not found", name),
        )
            .into_response()
    }
}
