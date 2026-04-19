use crate::state::AppState;
use axum::extract::State;
use axum::response::Response;
use axum_extra::extract::CookieJar;
use minijinja::context;

pub async fn page(State(state): State<AppState>, jar: CookieJar) -> Response {
    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title => "Agent Manual",
        breadcrumbs => vec![
            context! { label => "Agent Manual" },
        ],
        csrf_token,
    };
    super::render(&state.templates, "manual.html", ctx)
}
