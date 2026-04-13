use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use minijinja::context;

pub async fn list(State(state): State<AppState>, jar: CookieJar) -> Response {
    let channels = match state.kernel.channel_registry.list_active().await {
        Ok(rows) => rows
            .into_iter()
            .map(|ch| {
                context! {
                    id => ch.id.to_string(),
                    kind => ch.kind.to_string(),
                    display_name => ch.display_name,
                    external_id => ch.external_id,
                    reply_topic => ch.reply_topic.unwrap_or_default(),
                    server_url => ch.server_url.unwrap_or_default(),
                    webhook_url => ch.webhook_url.unwrap_or_default(),
                    connected_at => ch.connected_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                    last_active => ch.last_active.format("%Y-%m-%d %H:%M:%S").to_string(),
                }
            })
            .collect::<Vec<_>>(),
        Err(e) => {
            tracing::warn!(error = %e, "Failed to list active channels");
            Vec::new()
        }
    };

    let health = state
        .kernel
        .channel_manager
        .health()
        .await
        .into_iter()
        .map(|(id, status)| {
            context! {
                id,
                status => format!("{:?}", status),
            }
        })
        .collect::<Vec<_>>();

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title => "Channels",
        breadcrumbs => vec![
            context! { label => "Management", href => "/management" },
            context! { label => "Channels" },
        ],
        channels,
        health,
        csrf_token,
    };
    super::render(&state.templates, "channels.html", ctx)
}

pub async fn disconnect(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    state.kernel.channel_manager.deregister(&id).await;

    let channel_id: agentos_types::ChannelInstanceID = match id.parse() {
        Ok(v) => v,
        Err(_) => return (StatusCode::BAD_REQUEST, "Invalid channel ID").into_response(),
    };

    match state.kernel.channel_registry.deregister(&channel_id).await {
        Ok(()) => Redirect::to("/channels").into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            format!("Failed to disconnect channel '{id}': {e}"),
        )
            .into_response(),
    }
}
