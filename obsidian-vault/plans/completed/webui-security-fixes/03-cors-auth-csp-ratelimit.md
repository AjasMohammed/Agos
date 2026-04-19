---
title: "Phase 03 -- CORS, Auth Middleware, CSP Header, and Rate Limiting"
tags:
  - webui
  - security
  - plan
  - v3
date: 2026-03-17
status: complete
effort: 6h
priority: critical
---

# Phase 03 -- CORS, Auth Middleware, CSP Header, and Rate Limiting

> Restrict CORS to the bound address, add shared-secret bearer token + session cookie authentication middleware, set Content-Security-Policy headers, and add rate limiting to all endpoints.

---

## Why This Phase

These four issues (C1, C2, S4, S5) form the foundational security layer of the web crate. Without authentication, every other security measure is moot -- an attacker can directly call any endpoint. CORS restriction prevents browser-based cross-origin attacks. CSP prevents injected script execution. Rate limiting prevents brute-force and DoS. This phase must be completed before Phase 04 (CSRF) and Phase 08 (kernel dispatch) because those phases depend on the auth/session infrastructure established here.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| CORS (C1) | `CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any)` in `router.rs:35-40` | `allow_origin` restricted to `http://{host}:{port}` (the bound address); methods restricted to GET, POST, DELETE; headers restricted to Content-Type, Authorization, X-CSRF-Token |
| Authentication (C2) | None -- all endpoints publicly accessible | Dual-mode auth middleware: validates `Authorization: Bearer <token>` header OR `agentos_session` cookie on all endpoints except `/static/*` and `/login` |
| CSP (S4) | No `Content-Security-Policy` header | `default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'` plus `X-Frame-Options: DENY` and `X-Content-Type-Options: nosniff` |
| Rate limiting (S5) | None | `tower_governor` middleware: 60 req/min burst for reads, applied globally |

---

## Subtasks

### 1. Restrict CORS to bound address

**File:** `crates/agentos-web/src/router.rs`

The `build_router` function currently takes only `state: AppState`. It needs to also accept the bind address so it can construct the correct CORS origin.

Change the function signature and CORS configuration:

```rust
use std::net::SocketAddr;
use tower_http::cors::CorsLayer;
use axum::http::{HeaderValue, Method};

pub fn build_router(state: AppState, bind_addr: SocketAddr) -> Router {
    let origin = format!("http://{}", bind_addr);
    let cors = CorsLayer::new()
        .allow_origin(origin.parse::<HeaderValue>().expect("valid origin"))
        .allow_methods([Method::GET, Method::POST, Method::DELETE])
        .allow_headers([
            axum::http::header::CONTENT_TYPE,
            axum::http::header::AUTHORIZATION,
            axum::http::HeaderName::from_static("x-csrf-token"),
        ]);

    Router::new()
        // ... routes unchanged ...
        .with_state(state)
        .layer(TraceLayer::new_for_http())
        .layer(CompressionLayer::new())
        .layer(cors)
}
```

Remove the `use tower_http::cors::Any;` import.

**Update caller** in `server.rs` to pass `self.bind_addr`:

```rust
let app = build_router(self.state, self.bind_addr);
```

### 2. Add bearer token + session cookie authentication middleware

**New file:** `crates/agentos-web/src/auth.rs`

Create a Tower middleware that validates a bearer token or session cookie on every request except static files and the login page.

```rust
use axum::extract::Extension;
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Response};
use std::sync::Arc;

/// Shared auth token generated at server startup.
#[derive(Clone)]
pub struct AuthToken(pub Arc<String>);

/// Axum middleware for dual-mode authentication.
/// Accepts either:
/// 1. Authorization: Bearer <token> header (for API/CLI clients)
/// 2. agentos_session cookie (for browser/HTMX clients)
pub async fn require_auth(
    token: Extension<AuthToken>,
    jar: axum_extra::extract::CookieJar,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let path = request.uri().path().to_string();

    // Skip auth for static files and login page
    if path.starts_with("/static") || path == "/login" {
        return next.run(request).await;
    }

    // Check bearer token first (for API/CLI clients)
    if let Some(header) = request.headers().get(axum::http::header::AUTHORIZATION) {
        if let Ok(h) = header.to_str() {
            if h.starts_with("Bearer ") && &h["Bearer ".len()..] == token.0.as_str() {
                return next.run(request).await;
            }
        }
    }

    // Check session cookie (for browser/HTMX requests)
    if let Some(cookie) = jar.get("agentos_session") {
        if cookie.value() == token.0.as_str() {
            return next.run(request).await;
        }
    }

    // Not authenticated -- redirect browsers to login, return 401 for API
    if request.headers().get("accept")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.contains("text/html"))
        .unwrap_or(false)
    {
        return axum::response::Redirect::to("/login").into_response();
    }

    (StatusCode::UNAUTHORIZED, "Authentication required").into_response()
}

/// Login page handler -- renders a minimal login form.
pub async fn login_page() -> Response {
    Html(r#"<!DOCTYPE html>
<html><head><title>AgentOS Login</title>
<link rel="stylesheet" href="/static/css/pico.min.css">
</head><body>
<main class="container">
<h1>AgentOS Web UI</h1>
<form method="POST" action="/login">
<label for="token">Auth Token</label>
<input name="token" id="token" type="password" placeholder="Paste the token printed at startup" required>
<button type="submit">Login</button>
</form>
</main></body></html>"#).into_response()
}

#[derive(serde::Deserialize)]
pub struct LoginForm {
    pub token: String,
}

/// Login form submission handler -- validates token and sets session cookie.
pub async fn login_submit(
    auth_token: Extension<AuthToken>,
    axum::Form(form): axum::Form<LoginForm>,
) -> Response {
    if form.token == auth_token.0.as_str() {
        let cookie = axum_extra::extract::cookie::Cookie::build(("agentos_session", auth_token.0.as_str().to_string()))
            .path("/")
            .http_only(true)
            .same_site(axum_extra::extract::cookie::SameSite::Strict)
            .max_age(time::Duration::hours(8))
            .build();
        let jar = axum_extra::extract::CookieJar::new();
        let jar = jar.add(cookie);
        (jar, axum::response::Redirect::to("/")).into_response()
    } else {
        (StatusCode::UNAUTHORIZED, "Invalid token").into_response()
    }
}
```

**Token generation** in `server.rs`:

```rust
fn generate_auth_token() -> String {
    use rand::Rng;
    let bytes: [u8; 32] = rand::thread_rng().gen();
    hex::encode(bytes)
}
```

Print the token at startup in `WebServer::new()` or `start()`:

```rust
let auth_token = generate_auth_token();
println!("=== AgentOS Web UI ===");
println!("Auth token: {}", auth_token);
println!("Use this token to log in at http://{}/login", self.bind_addr);
```

### 3. Add Content-Security-Policy and security headers

**File:** `crates/agentos-web/src/router.rs`

Add a middleware layer that sets security headers on every response:

```rust
use axum::middleware;

async fn add_security_headers(
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        axum::http::HeaderName::from_static("content-security-policy"),
        axum::http::HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
             img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'"
        ),
    );
    response.headers_mut().insert(
        axum::http::header::X_FRAME_OPTIONS,
        axum::http::HeaderValue::from_static("DENY"),
    );
    response.headers_mut().insert(
        axum::http::header::X_CONTENT_TYPE_OPTIONS,
        axum::http::HeaderValue::from_static("nosniff"),
    );
    response
}
```

Add to the router layer stack:

```rust
.layer(middleware::from_fn(add_security_headers))
```

### 4. Add rate limiting

**File:** `crates/agentos-web/src/router.rs`

Add `tower_governor` for rate limiting:

```rust
use tower_governor::{GovernorConfigBuilder, GovernorLayer};

let governor_conf = GovernorConfigBuilder::default()
    .per_second(1)      // 1 token per second replenished
    .burst_size(60)     // burst capacity of 60 requests
    .finish()
    .expect("valid governor config");
```

Apply to the router as a layer:

```rust
.layer(GovernorLayer { config: governor_conf.into() })
```

### 5. Wire auth middleware and login routes into router

**File:** `crates/agentos-web/src/router.rs`

Add the login routes and auth middleware to `build_router`:

```rust
use crate::auth::{self, AuthToken};

pub fn build_router(state: AppState, bind_addr: SocketAddr, auth_token: AuthToken) -> Router {
    // ... existing routes ...
    Router::new()
        .route("/", axum::routing::get(dashboard::index))
        .route("/login", axum::routing::get(auth::login_page).post(auth::login_submit))
        // ... all other routes ...
        .with_state(state)
        .layer(axum::middleware::from_fn(auth::require_auth))
        .layer(axum::Extension(auth_token))
        // ... other layers ...
}
```

### 6. Register auth module and add dependencies

**File:** `crates/agentos-web/src/lib.rs`

Add: `pub mod auth;`

**File:** `crates/agentos-web/Cargo.toml`

Add dependencies:

```toml
axum-extra = { version = "0.10", features = ["cookie"] }
rand = "0.8"
hex = "0.4"
time = "0.3"
tower_governor = "0.4"
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-web/src/router.rs` | Restrict CORS to bound address; remove `Any` import; add CSP header middleware; add rate limiting layer; add auth middleware layer; add `/login` routes |
| `crates/agentos-web/src/auth.rs` | **New file** -- `require_auth` middleware, `login_page`, `login_submit`, `AuthToken`, `LoginForm` |
| `crates/agentos-web/src/lib.rs` | Add `pub mod auth;` |
| `crates/agentos-web/src/server.rs` | Generate auth token at startup; pass `bind_addr` and `auth_token` to `build_router`; print token to stdout |
| `crates/agentos-web/src/state.rs` | No change needed (auth token stored as `Extension`, not in `AppState`) |
| `crates/agentos-web/Cargo.toml` | Add `axum-extra`, `rand`, `hex`, `time`, `tower_governor` dependencies |

---

## Dependencies

None -- this is a foundational phase. Phase 04 (CSRF) and Phase 08 (kernel dispatch) depend on this phase being complete.

---

## Test Plan

1. **CORS restriction test:**
   - Send a request with `Origin: http://evil.com` header. Verify the response does NOT include `Access-Control-Allow-Origin: http://evil.com`.
   - Send a request with `Origin: http://127.0.0.1:8080` (the bound address). Verify it IS allowed.

2. **Auth middleware -- bearer token:**
   - `GET /agents` without any auth header returns 401
   - `GET /agents` with `Authorization: Bearer <wrong-token>` returns 401
   - `GET /agents` with `Authorization: Bearer <correct-token>` returns 200

3. **Auth middleware -- static bypass:**
   - `GET /static/css/pico.min.css` without any auth returns 200

4. **Auth middleware -- login bypass:**
   - `GET /login` without auth returns 200 (login form HTML)

5. **Session cookie auth:**
   - `POST /login` with correct token sets `agentos_session` cookie with `HttpOnly`, `SameSite=Strict` flags
   - Subsequent requests with the session cookie succeed without bearer header
   - `POST /login` with wrong token returns 401

6. **CSP header test:**
   - Any response includes `Content-Security-Policy` header with `default-src 'self'`
   - Any response includes `X-Frame-Options: DENY`
   - Any response includes `X-Content-Type-Options: nosniff`

7. **Rate limiting test:**
   - Send 61 rapid requests to `/agents`. The 61st should receive 429 Too Many Requests.

---

## Verification

```bash
# Must compile
cargo build -p agentos-web

# Tests pass
cargo test -p agentos-web

# Verify CORS no longer uses Any
grep -c "allow_origin(Any)" crates/agentos-web/src/router.rs
# Expected: 0

# Verify auth module exists
test -f crates/agentos-web/src/auth.rs && echo "auth.rs exists"

# Verify CSP header is set
grep -n "content-security-policy" crates/agentos-web/src/router.rs

# Verify rate limiting
grep -n "GovernorLayer\|tower_governor" crates/agentos-web/src/router.rs

# Verify login route exists
grep -n "/login" crates/agentos-web/src/router.rs
```

---

## Related

- [[WebUI Security Fixes Plan]] -- Master plan
- [[WebUI Security Fixes Data Flow]] -- Flow diagrams showing middleware stack
- [[04-csrf-protection]] -- depends on this phase (needs session infrastructure)
- [[08-kernel-dispatch-integration]] -- depends on this phase (needs auth in place)
