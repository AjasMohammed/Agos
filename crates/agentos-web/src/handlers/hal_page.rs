use crate::state::AppState;
use axum::extract::State;
use axum::response::Response;
use axum_extra::extract::CookieJar;
use minijinja::context;

pub async fn page(State(state): State<AppState>, jar: CookieJar) -> Response {
    let devices = state
        .kernel
        .hardware_registry
        .list_devices()
        .into_iter()
        .map(|d| {
            context! {
                id => d.id,
                device_type => d.device_type,
                status => format!("{:?}", d.status),
                granted_to => d.granted_to.into_iter().map(|id| id.to_string()).collect::<Vec<_>>().join(", "),
                denied_to => d.denied_to.into_iter().map(|id| id.to_string()).collect::<Vec<_>>().join(", "),
                first_seen => d.first_seen.format("%Y-%m-%d %H:%M:%S").to_string(),
                status_changed_at => d.status_changed_at.format("%Y-%m-%d %H:%M:%S").to_string(),
            }
        })
        .collect::<Vec<_>>();

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title => "HAL",
        breadcrumbs => vec![
            context! { label => "Observability", href => "/observability" },
            context! { label => "HAL" },
        ],
        devices,
        csrf_token,
    };
    super::render(&state.templates, "hal.html", ctx)
}
