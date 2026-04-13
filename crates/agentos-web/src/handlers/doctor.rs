use crate::state::AppState;
use axum::extract::State;
use axum::response::Response;
use axum_extra::extract::CookieJar;
use minijinja::context;

pub async fn page(State(state): State<AppState>, jar: CookieJar) -> Response {
    let status = state.service.get_status().await.ok();

    let checks = vec![
        context! {
            name => "Audit Log Path",
            status => if std::path::Path::new(&state.kernel.config.audit.log_path).exists() { "ok" } else { "error" },
            details => state.kernel.config.audit.log_path.clone(),
        },
        context! {
            name => "Vault Path",
            status => if std::path::Path::new(&state.kernel.config.secrets.vault_path).exists() { "ok" } else { "error" },
            details => state.kernel.config.secrets.vault_path.clone(),
        },
        context! {
            name => "Bus Socket",
            status => if std::path::Path::new(&state.kernel.config.bus.socket_path).exists() { "ok" } else { "warning" },
            details => state.kernel.config.bus.socket_path.clone(),
        },
    ];

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title => "Doctor",
        breadcrumbs => vec![
            context! { label => "Observability", href => "/observability" },
            context! { label => "Doctor" },
        ],
        status,
        checks,
        csrf_token,
    };
    super::render(&state.templates, "doctor.html", ctx)
}
