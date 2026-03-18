pub mod agents;
pub mod audit;
pub mod dashboard;
pub mod events;
pub mod pipelines;
pub mod secrets;
pub mod tasks;
pub mod tools;

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
