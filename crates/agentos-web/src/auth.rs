use std::sync::Arc;

use axum::extract::{Extension, State};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Response};
use axum_extra::extract::cookie::{Cookie, SameSite};
use axum_extra::extract::CookieJar;
use rand::RngCore;
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

/// Stable principal for uploaded file rows: hashes the browser session cookie or bearer token
/// so the raw secret is never stored as the `owner_principal` column value.
pub fn file_owner_principal(jar: &CookieJar, headers: &HeaderMap, token: &AuthToken) -> String {
    if let Some(cookie) = jar.get("agentos_session") {
        return crate::csrf::session_key(cookie.value());
    }
    if let Some(h) = headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        if let Some(candidate) = h.strip_prefix("Bearer ") {
            if ct_eq(candidate, token.0.as_str()) {
                return crate::csrf::session_key(candidate);
            }
        }
    }
    crate::csrf::session_key(token.0.as_str())
}

fn opaque_browser_session_valid(state: &AppState, cookie_value: &str) -> bool {
    let key = crate::csrf::session_key(cookie_value);
    let Some(entry) = state.browser_sessions.get(&key) else {
        return false;
    };
    let ok = entry.value().elapsed() <= crate::csrf::TOKEN_TTL;
    drop(entry);
    if !ok {
        state.browser_sessions.remove(&key);
    }
    ok
}

/// Axum middleware for dual-mode authentication.
///
/// Accepts either:
/// 1. `Authorization: Bearer <token>` header (API / CLI clients)
/// 2. `agentos_session` cookie (browser / HTMX clients) — opaque per-login ID registered
///    in `AppState::browser_sessions` (see [`login_submit`]).
///
/// Requests to `/static/` prefix and `/login` bypass authentication.
pub async fn require_auth(
    State(state): State<AppState>,
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
    if let Some(header) = request.headers().get(AUTHORIZATION) {
        if let Ok(h) = header.to_str() {
            if let Some(candidate) = h.strip_prefix("Bearer ") {
                if ct_eq(candidate, token.0.as_str()) {
                    return next.run(request).await;
                }
            }
        }
    }

    // 2. Session cookie — opaque value issued at login, keyed in `browser_sessions`.
    if let Some(cookie) = jar.get("agentos_session") {
        if opaque_browser_session_valid(&state, cookie.value()) {
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
       placeholder="Paste your Web UI auth token" required>
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
/// The `Secure` flag is set when the server is bound to a non-loopback address
/// (i.e. production behind a TLS proxy). For local development on 127.0.0.1 it
/// is omitted so plain-HTTP dev workflows keep working.
pub async fn login_submit(
    State(state): State<AppState>,
    Extension(auth_token): Extension<AuthToken>,
    axum::Form(mut form): axum::Form<LoginForm>,
) -> Response {
    // Move the token into a Zeroizing wrapper immediately so it's cleared on drop.
    let candidate = Zeroizing::new(std::mem::take(&mut form.token));
    if ct_eq(&candidate, auth_token.0.as_str()) {
        // Opaque per-login session so concurrent users sharing one deployment token get
        // distinct file-owner principals (see `FileStore::owner_principal`).
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        let opaque_session = hex::encode(bytes);
        let session_key = crate::csrf::session_key(&opaque_session);
        state
            .browser_sessions
            .insert(session_key, std::time::Instant::now());

        let cookie = Cookie::build(("agentos_session", opaque_session))
            .path("/")
            .http_only(true)
            .secure(state.secure_cookies)
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
