use crate::state::AppState;
use axum::extract::State;
use axum::response::Response;
use axum_extra::extract::CookieJar;
use minijinja::context;

pub async fn page(State(state): State<AppState>, jar: CookieJar) -> Response {
    let locks = state
        .kernel
        .resource_arbiter
        .list_locks()
        .await
        .into_iter()
        .map(|l| {
            context! {
                resource_id => l.resource_id,
                lock_mode => l.lock_mode,
                held_by => l.held_by,
                acquired_at => l.acquired_at,
                ttl_seconds => l.ttl_seconds,
                waiters => l.waiters,
            }
        })
        .collect::<Vec<_>>();

    let stats = state.kernel.resource_arbiter.contention_stats().await;

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title => "Resources",
        breadcrumbs => vec![
            context! { label => "Observability", href => "/observability" },
            context! { label => "Resources" },
        ],
        locks,
        stats,
        csrf_token,
    };
    super::render(&state.templates, "resources_page.html", ctx)
}
