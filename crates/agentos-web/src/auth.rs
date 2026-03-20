use std::sync::Arc;

use axum::extract::{Extension, State};
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Response};
use axum_extra::extract::cookie::{Cookie, SameSite};
use axum_extra::extract::CookieJar;
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

use crate::state::AppState;

/// Shared auth token generated at server startup.
///
/// The inner string is wrapped in `Zeroizing` so the token bytes are cleared
/// from memory when the last Arc reference drops.
#[derive(Clone)]
pub struct AuthToken(pub Arc<Zeroizing<String>>);

/// Constant-time string comparison to prevent timing-based token oracle attacks.
///
/// When lengths differ the function returns false, but a dummy constant-time comparison
/// is still performed on `a` against itself so that the timing difference between a
/// "wrong length" attempt and a "correct length but wrong bytes" attempt is minimised.
/// All tokens in this system are fixed-length (64 hex chars), so in practice an attacker
/// cannot learn the length from timing that they did not already know.
pub(crate) fn ct_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        // Perform a dummy comparison to reduce timing variation on length mismatch.
        let _dummy: bool = a.as_bytes().ct_eq(a.as_bytes()).into();
        return false;
    }
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

/// Axum middleware for dual-mode authentication.
///
/// Accepts either:
/// 1. `Authorization: Bearer <token>` header (API / CLI clients)
/// 2. `agentos_session` cookie (browser / HTMX clients)
///
/// Requests to `/static/` prefix and `/login` bypass authentication.
pub async fn require_auth(
    Extension(token): Extension<AuthToken>,
    jar: CookieJar,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let path = request.uri().path().to_string();

    // Skip auth for static assets and the login page.
    if path == "/static" || path.starts_with("/static/") || path == "/login" {
        return next.run(request).await;
    }

    // 1. Bearer token (for API / CLI clients) — constant-time comparison.
    if let Some(header) = request.headers().get(axum::http::header::AUTHORIZATION) {
        if let Ok(h) = header.to_str() {
            if let Some(candidate) = h.strip_prefix("Bearer ") {
                if ct_eq(candidate, token.0.as_str()) {
                    return next.run(request).await;
                }
            }
        }
    }

    // 2. Session cookie (for browser / HTMX requests) — constant-time comparison.
    if let Some(cookie) = jar.get("agentos_session") {
        if ct_eq(cookie.value(), token.0.as_str()) {
            return next.run(request).await;
        }
    }

    // Not authenticated — redirect browsers to login, return 401 for API clients.
    let accepts_html = request
        .headers()
        .get(axum::http::header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.contains("text/html"))
        .unwrap_or(false);

    if accepts_html {
        return axum::response::Redirect::to("/login").into_response();
    }

    (StatusCode::UNAUTHORIZED, "Authentication required").into_response()
}

/// GET /login — renders a minimal login form, injecting a CSRF token when a session exists.
pub async fn login_page(State(state): State<AppState>, jar: CookieJar) -> Response {
    let csrf_token = crate::csrf::csrf_token_for_session(&state, &jar);
    let csrf_field = if csrf_token.is_empty() {
        String::new()
    } else {
        format!(r#"<input type="hidden" name="_csrf" value="{csrf_token}">"#)
    };
    Html(format!(
        r#"<!DOCTYPE html>
<html><head><title>AgentOS Login</title>
<link rel="stylesheet" href="/static/css/pico.min.css">
</head><body>
<main class="container">
<h1>AgentOS Web UI</h1>
<form method="POST" action="/login">
{csrf_field}
<label for="token">Auth Token</label>
<input name="token" id="token" type="password"
       placeholder="Paste the token printed at startup" required>
<button type="submit">Login</button>
</form>
</main></body></html>"#
    ))
    .into_response()
}

#[derive(serde::Deserialize)]
pub struct LoginForm {
    pub token: String,
}

/// POST /login — validates the token and sets an HttpOnly session cookie.
///
/// Note: the `Secure` flag is intentionally omitted for local development
/// (the server binds to plain HTTP). TODO: pass a `tls: bool` flag and set
/// `.secure(true)` when running behind TLS in production.
pub async fn login_submit(
    Extension(auth_token): Extension<AuthToken>,
    axum::Form(mut form): axum::Form<LoginForm>,
) -> Response {
    // Move the token into a Zeroizing wrapper immediately so it's cleared on drop.
    let candidate = Zeroizing::new(std::mem::take(&mut form.token));
    if ct_eq(&candidate, auth_token.0.as_str()) {
        let cookie = Cookie::build(("agentos_session", auth_token.0.as_str().to_string()))
            .path("/")
            .http_only(true)
            .same_site(SameSite::Strict)
            .max_age(time::Duration::hours(8))
            .build();
        let jar = CookieJar::new();
        let jar = jar.add(cookie);
        (jar, axum::response::Redirect::to("/")).into_response()
    } else {
        (StatusCode::UNAUTHORIZED, "Invalid token").into_response()
    }
}
