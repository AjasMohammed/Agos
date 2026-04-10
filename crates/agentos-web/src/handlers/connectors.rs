use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use minijinja::context;
use serde::Serialize;

use crate::handlers::render;
use crate::state::AppState;

/// View model for the connectors page template.
#[derive(Debug, Serialize)]
struct ConnectorView {
    id: String,
    name: String,
    status: String,
    scopes: Vec<String>,
    expires_at: String,
    connected: bool,
}

/// GET /connectors — List all configured connectors with their connection status.
pub async fn list_connectors(State(state): State<AppState>, jar: CookieJar) -> Response {
    let oauth_store = state.kernel.vault.oauth_store();

    // Get registered connectors from the connector registry
    let registered = state.kernel.connector_registry.list().await;

    // Get stored OAuth credentials
    let oauth_creds = oauth_store.list().await.unwrap_or_default();
    let connected_ids: std::collections::HashSet<String> =
        oauth_creds.iter().map(|c| c.connector_id.clone()).collect();

    // Also check oauth_providers.toml for configured-but-not-yet-connected providers
    let provider_configs = super::oauth::load_provider_configs();

    let mut connectors: Vec<ConnectorView> = Vec::new();

    // Add registered connectors first
    for manifest in &registered {
        let id = &manifest.connector.id;
        let cred = oauth_creds.iter().find(|c| &c.connector_id == id);
        let is_connected = connected_ids.contains(id);

        connectors.push(ConnectorView {
            id: id.clone(),
            name: manifest.connector.name.clone(),
            status: if is_connected {
                "Connected".into()
            } else {
                "Disconnected".into()
            },
            scopes: cred.map(|c| c.scopes.clone()).unwrap_or_default(),
            expires_at: cred
                .and_then(|c| c.expires_at)
                .map(|dt| dt.format("%Y-%m-%d %H:%M UTC").to_string())
                .unwrap_or_else(|| "\u{2014}".into()),
            connected: is_connected,
        });
    }

    // Add providers from config that aren't already registered as connectors
    for provider_id in provider_configs.keys() {
        if !connectors.iter().any(|c| &c.id == provider_id) {
            let cred = oauth_creds.iter().find(|c| &c.connector_id == provider_id);
            let is_connected = connected_ids.contains(provider_id);

            connectors.push(ConnectorView {
                id: provider_id.clone(),
                name: provider_id.clone(),
                status: if is_connected {
                    "Connected".into()
                } else {
                    "Not connected".into()
                },
                scopes: cred.map(|c| c.scopes.clone()).unwrap_or_default(),
                expires_at: cred
                    .and_then(|c| c.expires_at)
                    .map(|dt| dt.format("%Y-%m-%d %H:%M UTC").to_string())
                    .unwrap_or_else(|| "\u{2014}".into()),
                connected: is_connected,
            });
        }
    }

    connectors.sort_by(|a, b| a.id.cmp(&b.id));

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);

    let ctx = context! {
        page_title => "Connectors",
        breadcrumbs => vec![context! { label => "Connectors" }],
        connectors => connectors,
        active_page => "connectors",
        csrf_token,
    };

    render(&state.templates, "connectors.html", ctx)
}

/// POST /connectors/:connector_id/disconnect — Revoke OAuth tokens and deregister.
pub async fn disconnect_connector(
    State(state): State<AppState>,
    Path(connector_id): Path<String>,
) -> Response {
    let oauth_store = state.kernel.vault.oauth_store();

    // Delete OAuth credential from vault
    if let Err(e) = oauth_store.delete(&connector_id).await {
        tracing::warn!(
            connector = %connector_id,
            error = %e,
            "Failed to delete OAuth credential (may not exist)"
        );
    }

    // Deregister from connector registry (if registered)
    if let Err(e) = state
        .kernel
        .connector_registry
        .deregister(&connector_id)
        .await
    {
        tracing::debug!(
            connector = %connector_id,
            error = %e,
            "Connector was not in registry"
        );
    }

    tracing::info!(connector = %connector_id, "Connector disconnected");

    Redirect::to("/connectors").into_response()
}

/// GET /api/connectors — JSON list for programmatic access.
pub async fn list_connectors_json(State(state): State<AppState>) -> Response {
    let oauth_store = state.kernel.vault.oauth_store();
    let creds = oauth_store.list().await.unwrap_or_default();
    let registered = state.kernel.connector_registry.list().await;

    let result: Vec<serde_json::Value> = registered
        .iter()
        .map(|m| {
            let id = &m.connector.id;
            let cred = creds.iter().find(|c| &c.connector_id == id);
            serde_json::json!({
                "id": id,
                "name": m.connector.name,
                "connected": cred.is_some(),
                "scopes": cred.map(|c| &c.scopes).cloned().unwrap_or_default(),
                "tools": m.tools.iter().map(|t| format!("{}.{}", id, t.name)).collect::<Vec<_>>(),
            })
        })
        .collect();

    (StatusCode::OK, axum::Json(result)).into_response()
}
