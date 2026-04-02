use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use minijinja::context;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Default)]
pub struct SearchParams {
    pub q: Option<String>,
    pub artifact_type: Option<String>,
}

/// Lightweight tool summary used inside the marketplace pages.
#[derive(Debug, Clone, Serialize)]
struct MarketplaceTool {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub downloads: i64,
    pub tags: Vec<String>,
    pub artifact_type: String,
}

/// Form payload submitted when a user posts a review.
#[derive(Deserialize)]
pub struct ReviewForm {
    pub author_key: String,
    pub rating: u8,
    pub body: Option<String>,
}

/// GET /marketplace  — list / search page.
pub async fn list(
    State(state): State<AppState>,
    Query(params): Query<SearchParams>,
    jar: CookieJar,
) -> Response {
    let search_query = params.q.clone().unwrap_or_default();
    let artifact_type = params.artifact_type.clone().unwrap_or_default();

    let tools = fetch_tools(&state, &search_query, &artifact_type).await;

    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        page_title => "Marketplace",
        breadcrumbs => vec![context! { label => "Marketplace" }],
        search_query,
        artifact_type,
        tools,
        csrf_token,
    };
    super::render(&state.templates, "marketplace.html", ctx)
}

/// GET /marketplace/:name — detail page.
pub async fn detail(
    State(state): State<AppState>,
    Path(name): Path<String>,
    jar: CookieJar,
) -> Response {
    let tool = match fetch_tool_detail(&state, &name).await {
        Some(t) => t,
        None => {
            return (StatusCode::NOT_FOUND, "Tool not found in marketplace").into_response();
        }
    };

    let reviews = fetch_reviews(&state, &name).await;
    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);

    let ctx = context! {
        page_title => format!("{} — Marketplace", name),
        breadcrumbs => vec![
            context! { label => "Marketplace", href => "/marketplace" },
            context! { label => name.clone() },
        ],
        name => tool.name,
        version => tool.version,
        description => tool.description,
        author => tool.author,
        downloads => tool.downloads,
        tags => tool.tags,
        artifact_type => tool.artifact_type,
        created_at => String::new(),
        updated_at => String::new(),
        reviews,
        csrf_token,
    };
    super::render(&state.templates, "marketplace_detail.html", ctx)
}

/// POST /marketplace/:name/review — submit a review, return updated reviews partial.
pub async fn submit_review(
    State(state): State<AppState>,
    Path(name): Path<String>,
    jar: CookieJar,
    axum::Form(form): axum::Form<ReviewForm>,
) -> Response {
    // POST review to registry API.
    post_review(&state, &name, &form).await;

    // Re-fetch reviews to show the updated list.
    let reviews = fetch_reviews(&state, &name).await;
    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let ctx = context! {
        name,
        reviews,
        csrf_token,
    };
    super::render(&state.templates, "partials/marketplace_reviews.html", ctx)
}

// ---------------------------------------------------------------------------
// Registry API helpers
// ---------------------------------------------------------------------------

fn registry_base_url() -> String {
    std::env::var("AGENTOS_REGISTRY_URL").unwrap_or_else(|_| "http://localhost:8090".to_string())
}

/// Fetch tools from the registry API (returns an empty list on any failure).
async fn fetch_tools(_state: &AppState, query: &str, artifact_type: &str) -> Vec<minijinja::Value> {
    use minijinja::value::Value;

    let base = registry_base_url();
    let mut url = format!("{}/v1/tools?limit=50", base);
    if !query.is_empty() {
        url.push_str(&format!("&q={}", urlencoding_simple(query)));
    }

    // We call the registry over HTTP using a plain tokio::task so we don't need
    // reqwest in the web crate's dependencies — fall back gracefully on error.
    let result = tokio::task::spawn_blocking(move || {
        let response = ureq::get(&url).call().ok()?;
        let body = response.into_string().ok()?;
        let parsed: serde_json::Value = serde_json::from_str(&body).ok()?;
        Some(parsed)
    })
    .await;

    let json = match result {
        Ok(Some(j)) => j,
        _ => return vec![],
    };

    let arr = match json.as_array() {
        Some(a) => a,
        None => return vec![],
    };

    arr.iter()
        .filter(|item| {
            if artifact_type.is_empty() {
                return true;
            }
            item.get("artifact_type")
                .and_then(|v| v.as_str())
                .map(|t| t == artifact_type)
                .unwrap_or(true)
        })
        .map(|item| {
            let tags: Vec<String> = item
                .get("tags")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            context! {
                name => item.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                version => item.get("version").and_then(|v| v.as_str()).unwrap_or(""),
                description => item.get("description").and_then(|v| v.as_str()).unwrap_or(""),
                author => item.get("author").and_then(|v| v.as_str()).unwrap_or(""),
                downloads => item.get("downloads").and_then(|v| v.as_i64()).unwrap_or(0),
                tags => tags,
                artifact_type => item.get("artifact_type").and_then(|v| v.as_str()).unwrap_or("tool"),
            }
        })
        .collect::<Vec<Value>>()
}

/// Fetch a single tool's details from the registry API.
async fn fetch_tool_detail(_state: &AppState, name: &str) -> Option<MarketplaceTool> {
    let base = registry_base_url();
    let url = format!("{}/v1/tools/{}", base, urlencoding_simple(name));

    let result = tokio::task::spawn_blocking(move || {
        let response = ureq::get(&url).call().ok()?;
        let body = response.into_string().ok()?;
        let parsed: serde_json::Value = serde_json::from_str(&body).ok()?;
        Some(parsed)
    })
    .await;

    let json = match result {
        Ok(Some(j)) if j.get("error").is_none() => j,
        _ => return None,
    };

    let tags: Vec<String> = json
        .get("tags")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    Some(MarketplaceTool {
        name: json
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        version: json
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        description: json
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        author: json
            .get("author")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        downloads: json.get("downloads").and_then(|v| v.as_i64()).unwrap_or(0),
        tags,
        artifact_type: json
            .get("artifact_type")
            .and_then(|v| v.as_str())
            .unwrap_or("tool")
            .to_string(),
    })
}

/// Fetch reviews for a tool from the registry API.
async fn fetch_reviews(_state: &AppState, name: &str) -> Vec<minijinja::Value> {
    use minijinja::value::Value;

    let base = registry_base_url();
    let url = format!("{}/v1/tools/{}/reviews", base, urlencoding_simple(name));

    let result = tokio::task::spawn_blocking(move || {
        let response = ureq::get(&url).call().ok()?;
        let body = response.into_string().ok()?;
        let parsed: serde_json::Value = serde_json::from_str(&body).ok()?;
        Some(parsed)
    })
    .await;

    let json = match result {
        Ok(Some(j)) => j,
        _ => return vec![],
    };

    let arr = match json.as_array() {
        Some(a) => a,
        None => return vec![],
    };

    arr.iter()
        .map(|item| {
            context! {
                id => item.get("id").and_then(|v| v.as_i64()).unwrap_or(0),
                tool_name => item.get("tool_name").and_then(|v| v.as_str()).unwrap_or(""),
                author_key => item.get("author_key").and_then(|v| v.as_str()).unwrap_or(""),
                rating => item.get("rating").and_then(|v| v.as_i64()).unwrap_or(1) as u8,
                body => item.get("body").and_then(|v| v.as_str()).map(|s| s.to_string()),
                created_at => item.get("created_at").and_then(|v| v.as_str()).unwrap_or(""),
            }
        })
        .collect::<Vec<Value>>()
}

/// POST a review to the registry API (fire-and-forget; errors are logged but not fatal).
async fn post_review(_state: &AppState, name: &str, form: &ReviewForm) {
    let base = registry_base_url();
    let url = format!("{}/v1/tools/{}/reviews", base, urlencoding_simple(name));
    let payload = serde_json::json!({
        "author_key": form.author_key,
        "rating": form.rating,
        "body": form.body,
    });
    let body_str = payload.to_string();

    let result = tokio::task::spawn_blocking(move || {
        ureq::post(&url)
            .set("content-type", "application/json")
            .send_string(&body_str)
    })
    .await;

    if let Err(e) = result {
        tracing::warn!(error = %e, "Failed to post review to registry");
    }
}

/// Minimal percent-encoding for path segments (no external dep needed).
fn urlencoding_simple(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'~' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}
