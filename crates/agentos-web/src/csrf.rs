use axum::body::Body;
use axum::extract::State;
use axum::http::{Method, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;

use crate::auth::ct_eq;
use crate::state::AppState;

/// The maximum age of a CSRF token before it is regenerated.
/// Matches the session cookie `max_age` of 8 hours set in `auth.rs`.
pub(crate) const TOKEN_TTL: std::time::Duration = std::time::Duration::from_secs(8 * 3600);

/// Derives a stable, non-reversible DashMap key from a raw session cookie value.
///
/// Using SHA-256(cookie) as the key means the raw auth credential is never stored
/// as a plain `String` anywhere in the CSRF token map.
pub(crate) fn session_key(session_value: &str) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(session_value.as_bytes());
    hex::encode(hash)
}

/// Generates a cryptographically random 256-bit CSRF token as a hex string.
pub fn generate_csrf_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Returns the current CSRF token for the session identified by `jar`, or an empty string
/// if no session cookie is present.
///
/// Callers (page-rendering handlers) use this to inject `{{ csrf_token }}` into templates.
pub fn csrf_token_for_session(state: &AppState, jar: &CookieJar) -> String {
    jar.get("agentos_session")
        .and_then(|c| {
            let key = session_key(c.value());
            state
                .csrf_tokens
                .get(&key)
                .map(|entry| entry.value().0.clone())
        })
        .unwrap_or_default()
}

/// Axum middleware that enforces CSRF protection for state-changing requests.
///
/// - GET / HEAD / OPTIONS: generates (or refreshes) a CSRF token for the session.
///   Tokens older than 8 h are regenerated to match the session cookie lifetime.
/// - POST / DELETE / PUT: validates either the `X-CSRF-Token` request header (HTMX)
///   **or** the `_csrf` URL-encoded form body field (plain HTML form fallback),
///   using constant-time comparison to prevent timing-based oracle attacks.
/// - Requests without a session cookie (bearer-token API clients) skip CSRF entirely.
/// - `/static/*` is always bypassed.
/// - `GET /login` is bypassed; `POST /login` is **not** bypassed (login CSRF protection).
pub async fn csrf_middleware(
    State(state): State<AppState>,
    jar: CookieJar,
    request: Request<Body>,
    next: Next,
) -> Response {
    let path = request.uri().path().to_string();
    let method = request.method().clone();

    // Static files never need CSRF protection.
    if path == "/static" || path.starts_with("/static/") {
        return next.run(request).await;
    }

    // Login GET (renders the form) is exempt; login POST is NOT — an already-authenticated
    // user re-submitting /login would otherwise skip validation.
    if path == "/login" && (method == Method::GET || method == Method::HEAD) {
        return next.run(request).await;
    }

    // Requests without a session cookie are bearer-token API clients.
    // They are not subject to cross-site request forgery via browser form submissions.
    let session_id = match jar.get("agentos_session") {
        Some(c) => c.value().to_string(),
        None => return next.run(request).await,
    };

    let key = session_key(&session_id);
    let now = std::time::Instant::now();

    if method == Method::GET || method == Method::HEAD || method == Method::OPTIONS {
        // Ensure a valid, non-expired token exists for this session.
        let needs_new = state
            .csrf_tokens
            .get(&key)
            .map(|entry| now.duration_since(entry.value().1) > TOKEN_TTL)
            .unwrap_or(true);

        if needs_new {
            state.csrf_tokens.insert(key, (generate_csrf_token(), now));
        }
        return next.run(request).await;
    }

    // POST / DELETE / PUT — validate via header (HTMX) or form body field (plain forms).
    validate_csrf(state, key, path, method, request, next).await
}

/// Validates the CSRF token for state-changing requests.
///
/// Checks `X-CSRF-Token` header first; if absent, buffers the request body and looks for
/// a `_csrf` field in `application/x-www-form-urlencoded` data, then reconstructs the
/// request with the original body so the handler can still read it.
async fn validate_csrf(
    state: AppState,
    key: String,
    path: String,
    method: Method,
    request: Request<Body>,
    next: Next,
) -> Response {
    let expected = state
        .csrf_tokens
        .get(&key)
        .map(|entry| entry.value().0.clone());

    // --- Try X-CSRF-Token header first (cheap, no body buffering needed) ---
    if let Some(header_val) = request
        .headers()
        .get("x-csrf-token")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
    {
        let valid = expected
            .as_deref()
            .map(|exp| ct_eq(&header_val, exp))
            .unwrap_or(false);

        if valid {
            return next.run(request).await;
        }
        // Header present but wrong — reject immediately, no need to check form body.
        tracing::warn!(path = %path, method = %method, "CSRF token invalid (header)");
        return (StatusCode::FORBIDDEN, "CSRF token missing or invalid").into_response();
    }

    // --- Try _csrf field in form body (plain HTML form fallback) ---
    let is_form = request
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.starts_with("application/x-www-form-urlencoded"))
        .unwrap_or(false);

    if is_form {
        let (parts, body) = request.into_parts();
        // Limit buffering to 256 KiB; secrets and pipeline inputs have their own size guards.
        match axum::body::to_bytes(body, 256 * 1024).await {
            Ok(bytes) => {
                let form_token = extract_csrf_from_form(&bytes);
                let valid = form_token
                    .as_deref()
                    .zip(expected.as_deref())
                    .map(|(provided, exp)| ct_eq(provided, exp))
                    .unwrap_or(false);

                // Reconstruct the request so the handler can still read the body.
                let request = Request::from_parts(parts, Body::from(bytes));

                if valid {
                    return next.run(request).await;
                }
                tracing::warn!(path = %path, method = %method, "CSRF token invalid (form body)");
                return (StatusCode::FORBIDDEN, "CSRF token missing or invalid").into_response();
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to buffer request body for CSRF check");
                return (StatusCode::BAD_REQUEST, "Request body too large").into_response();
            }
        }
    }

    // No token in header and not a form submission — reject.
    tracing::warn!(path = %path, method = %method, "CSRF token missing");
    (StatusCode::FORBIDDEN, "CSRF token missing or invalid").into_response()
}

/// Extracts the `_csrf` value from a URL-encoded form body.
///
/// CSRF tokens are 64-char lowercase hex strings and contain no characters that need
/// percent-decoding, so a simple key=value scan is sufficient.
fn extract_csrf_from_form(body: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(body).ok()?;
    text.split('&').find_map(|pair| {
        let (key, val) = pair.split_once('=')?;
        if key == "_csrf" {
            Some(val.to_string())
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_csrf_token_is_64_hex_chars() {
        let token = generate_csrf_token();
        assert_eq!(token.len(), 64, "CSRF token must be 64 hex characters");
        assert!(
            token.chars().all(|c| c.is_ascii_hexdigit()),
            "CSRF token must be lowercase hex"
        );
    }

    #[test]
    fn generate_csrf_token_is_random() {
        let a = generate_csrf_token();
        let b = generate_csrf_token();
        assert_ne!(a, b, "Consecutive CSRF tokens must differ");
    }

    #[test]
    fn session_key_is_deterministic() {
        let key1 = session_key("my-session-value");
        let key2 = session_key("my-session-value");
        assert_eq!(key1, key2);
    }

    #[test]
    fn session_key_is_not_the_input() {
        let raw = "my-session-value";
        let key = session_key(raw);
        assert_ne!(key, raw, "session_key must not return the raw value");
        assert_eq!(key.len(), 64, "SHA-256 hex output must be 64 chars");
    }

    #[test]
    fn session_key_differs_for_different_inputs() {
        let a = session_key("token-A");
        let b = session_key("token-B");
        assert_ne!(a, b);
    }

    #[test]
    fn extract_csrf_from_form_finds_token() {
        let body = b"name=Alice&_csrf=abcdef1234567890&action=submit";
        let result = extract_csrf_from_form(body);
        assert_eq!(result.as_deref(), Some("abcdef1234567890"));
    }

    #[test]
    fn extract_csrf_from_form_returns_none_when_missing() {
        let body = b"name=Alice&action=submit";
        assert!(extract_csrf_from_form(body).is_none());
    }

    #[test]
    fn extract_csrf_from_form_handles_leading_field() {
        let body = b"_csrf=token123&name=Bob";
        let result = extract_csrf_from_form(body);
        assert_eq!(result.as_deref(), Some("token123"));
    }

    #[test]
    fn token_ttl_constant_is_eight_hours() {
        assert_eq!(TOKEN_TTL.as_secs(), 8 * 3600);
    }
}
