pub mod a2a;
pub mod agent_convo;
pub mod agent_detail;
pub mod agents;
pub mod audit;
pub mod channels;
pub mod chat;
pub mod config_page;
pub mod connectors;
pub mod costs;
pub mod dashboard;
pub mod doctor;
pub mod escalations;
pub mod events;
pub mod events_log;
pub mod files;
pub mod hal_page;
pub mod identity_page;
pub mod logs;
pub mod manual_page;
pub mod management;
pub mod marketplace;
pub mod mcp_page;
pub mod notifications;
pub mod oauth;
pub mod observability;
pub mod pipeline_ui;
pub mod pipelines;
pub mod plugins;
pub mod resources_page;
pub mod roles;
pub mod schedules;
pub mod scratchpad;
pub mod secrets;
pub mod tasks;
pub mod teams;
pub mod tools;
pub mod webhooks;
pub mod webhooks_page;

use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use minijinja::Environment;

/// Render a template or return a 500 error.
pub fn render(
    env: &Environment<'_>,
    template_name: &str,
    ctx: minijinja::value::Value,
) -> Response {
    match env.get_template(template_name) {
        Ok(tmpl) => match tmpl.render(ctx) {
            Ok(html) => Html(html).into_response(),
            Err(e) => {
                tracing::error!(error = %e, template = template_name, "Template render error");
                (StatusCode::INTERNAL_SERVER_ERROR, "Template render error").into_response()
            }
        },
        Err(e) => {
            tracing::error!(error = %e, template = template_name, "Template not found");
            (StatusCode::INTERNAL_SERVER_ERROR, "Template not found").into_response()
        }
    }
}
