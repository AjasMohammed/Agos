use crate::state::AppState;
use axum::extract::{Query, State};
use axum::response::Response;
use axum_extra::extract::CookieJar;
use minijinja::context;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
pub struct ConfigQuery {
    pub section: Option<String>,
}

pub async fn page(
    State(state): State<AppState>,
    Query(query): Query<ConfigQuery>,
    jar: CookieJar,
) -> Response {
    let config_json = serde_json::to_value(&state.kernel.config).unwrap_or(Value::Null);

    let mut sections = Vec::new();
    if let Value::Object(map) = &config_json {
        sections = map.keys().cloned().collect::<Vec<_>>();
        sections.sort();
    }

    let selected = query
        .section
        .as_deref()
        .filter(|s| sections.iter().any(|k| k == *s))
        .map(ToOwned::to_owned)
        .or_else(|| sections.first().cloned())
        .unwrap_or_default();

    let section_value = if selected.is_empty() {
        Value::Null
    } else {
        config_json.get(&selected).cloned().unwrap_or(Value::Null)
    };

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title => "Config",
        breadcrumbs => vec![
            context! { label => "Management", href => "/management" },
            context! { label => "Config" },
        ],
        sections,
        selected,
        section_value,
        csrf_token,
    };
    super::render(&state.templates, "config_page.html", ctx)
}
