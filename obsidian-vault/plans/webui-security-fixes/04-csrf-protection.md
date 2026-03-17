---
title: "Phase 04 -- CSRF Protection"
tags:
  - webui
  - security
  - plan
  - v3
date: 2026-03-17
status: complete
effort: 4h
priority: critical
---

# Phase 04 -- CSRF Protection

> Add per-session CSRF tokens to all HTML forms and validate them on every state-changing request (POST, DELETE, PUT) to prevent cross-site request forgery.

---

## Why This Phase

Without CSRF tokens, any website a user visits can silently submit forms to the AgentOS web UI on their behalf -- creating agents, installing tools, writing secrets, or running pipelines. Even with the bearer-token auth middleware from Phase 03, browser requests authenticated via session cookies are vulnerable because the browser automatically includes cookies on cross-origin form submissions. This phase adds a server-side CSRF token to every form, validated on submission.

This phase depends on Phase 03 because CSRF tokens are bound to the session state introduced by the auth middleware's session cookie.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Form submissions | All `<form>` elements in templates submit without any CSRF token | Every form includes a hidden `<input name="_csrf" value="{{ csrf_token }}">` field |
| CSRF validation | None | Axum middleware validates `X-CSRF-Token` header (for HTMX) on POST/DELETE/PUT requests |
| Token generation | N/A | Random 256-bit hex token generated per session, stored in a `DashMap<String, String>` keyed by session ID |
| HTMX support | HTMX requests send no CSRF token | `htmx:configRequest` event listener in `base.html` injects `X-CSRF-Token` header on every HTMX request |
| Bearer token bypass | N/A | API requests authenticated via Bearer token (no session cookie) skip CSRF validation |

---

## Subtasks

### 1. Add CSRF token store to AppState

**File:** `crates/agentos-web/src/state.rs`

The `AppState` struct currently holds `kernel: Arc<Kernel>` and `templates: Arc<Environment<'static>>`. Add a concurrent map for CSRF tokens keyed by session ID:

```rust
use dashmap::DashMap;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub kernel: Arc<Kernel>,
    pub templates: Arc<Environment<'static>>,
    pub csrf_tokens: Arc<DashMap<String, String>>,  // session_id -> csrf_token
}
```

**Dependency:** Add `dashmap = "6"` to `crates/agentos-web/Cargo.toml`.

Initialize in `server.rs` when constructing `AppState`:

```rust
let state = AppState {
    kernel,
    templates,
    csrf_tokens: Arc::new(DashMap::new()),
};
```

### 2. Create CSRF middleware

**New file:** `crates/agentos-web/src/csrf.rs`

Create an Axum middleware that:
- On GET/HEAD/OPTIONS requests: generates a CSRF token if the session does not have one, and stores it.
- On POST/DELETE/PUT requests: validates the `X-CSRF-Token` header against the stored token for the session.
- Skips CSRF for requests without a session cookie (i.e., bearer-token-only API requests).

```rust
use axum::extract::State;
use axum::http::{Method, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use rand::Rng;

use crate::state::AppState;

fn generate_csrf_token() -> String {
    let bytes: [u8; 32] = rand::thread_rng().gen();
    hex::encode(bytes)
}

pub async fn csrf_middleware(
    State(state): State<AppState>,
    jar: axum_extra::extract::CookieJar,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    // Skip CSRF for static files and login page
    let path = request.uri().path().to_string();
    if path.starts_with("/static") || path == "/login" {
        return next.run(request).await;
    }

    // Get session ID from cookie (set by auth middleware in Phase 03)
    let session_id = match jar.get("agentos_session") {
        Some(c) => c.value().to_string(),
        None => {
            // No session cookie = bearer-token auth or unauthenticated.
            // Bearer-token requests do not need CSRF protection.
            return next.run(request).await;
        }
    };

    let method = request.method().clone();

    if method == Method::GET || method == Method::HEAD || method == Method::OPTIONS {
        // Ensure a CSRF token exists for this session (idempotent)
        state.csrf_tokens
            .entry(session_id)
            .or_insert_with(generate_csrf_token);
        return next.run(request).await;
    }

    // For state-changing methods (POST, DELETE, PUT), validate CSRF token
    let provided_token = request
        .headers()
        .get("x-csrf-token")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let expected = state.csrf_tokens.get(&session_id).map(|v| v.clone());

    match (provided_token, expected) {
        (Some(provided), Some(expected)) if provided == expected => {
            next.run(request).await
        }
        _ => {
            tracing::warn!(
                path = %path,
                method = %method,
                "CSRF token missing or invalid"
            );
            (StatusCode::FORBIDDEN, "CSRF token missing or invalid").into_response()
        }
    }
}
```

### 3. Inject CSRF token into all templates

**File:** `crates/agentos-web/src/templates/base.html`

Add a meta tag in the `<head>` for JavaScript/HTMX access, and a script before `</body>` that configures HTMX to include the token:

```html
<head>
    <!-- ... existing head content ... -->
    <meta name="csrf-token" content="{{ csrf_token }}">
</head>
<body>
    <!-- ... existing body content ... -->
    <script>
        // Inject CSRF token into all HTMX requests
        document.addEventListener('htmx:configRequest', function(event) {
            var meta = document.querySelector('meta[name="csrf-token"]');
            if (meta) {
                event.detail.headers['X-CSRF-Token'] = meta.content;
            }
        });
    </script>
</body>
```

### 4. Update all handlers to pass CSRF token to templates

**Files:** All handler files in `crates/agentos-web/src/handlers/`

Every handler that renders a full page must include the `csrf_token` in its template context. The token is retrieved from the `csrf_tokens` map using the session ID from the cookie.

Example pattern for each handler:

```rust
pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
    jar: axum_extra::extract::CookieJar,
) -> Response {
    // ... existing data loading ...

    let csrf_token = jar.get("agentos_session")
        .and_then(|c| state.csrf_tokens.get(c.value()).map(|v| v.clone()))
        .unwrap_or_default();

    let ctx = context! {
        page_title => "Agents",
        agents,
        csrf_token,
    };
    super::render(&state.templates, "agents.html", ctx)
}
```

Apply this pattern to every handler that renders HTML:
- `dashboard.rs` -- `index`
- `agents.rs` -- `list`
- `tasks.rs` -- `list`, `detail`
- `tools.rs` -- `list`
- `secrets.rs` -- `list`
- `pipelines.rs` -- `list`
- `audit.rs` -- `list`

### 5. Wire CSRF middleware into router

**File:** `crates/agentos-web/src/router.rs`

Add the CSRF middleware layer after the auth middleware:

```rust
use crate::csrf::csrf_middleware;

// In build_router():
.layer(axum::middleware::from_fn_with_state(state.clone(), csrf_middleware))
```

### 6. Register module

**File:** `crates/agentos-web/src/lib.rs`

Add: `pub mod csrf;`

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-web/src/csrf.rs` | **New file** -- CSRF token generation, validation middleware |
| `crates/agentos-web/src/lib.rs` | Add `pub mod csrf;` |
| `crates/agentos-web/src/state.rs` | Add `csrf_tokens: Arc<DashMap<String, String>>` field to `AppState` |
| `crates/agentos-web/src/server.rs` | Initialize `csrf_tokens` in `AppState` constructor |
| `crates/agentos-web/src/router.rs` | Add CSRF middleware layer |
| `crates/agentos-web/src/templates/base.html` | Add CSRF meta tag and `htmx:configRequest` event listener |
| `crates/agentos-web/src/handlers/dashboard.rs` | Add `CookieJar` extractor; pass `csrf_token` to template context |
| `crates/agentos-web/src/handlers/agents.rs` | Add `CookieJar` extractor; pass `csrf_token` to template context |
| `crates/agentos-web/src/handlers/tasks.rs` | Add `CookieJar` extractor; pass `csrf_token` to template context |
| `crates/agentos-web/src/handlers/tools.rs` | Add `CookieJar` extractor; pass `csrf_token` to template context |
| `crates/agentos-web/src/handlers/secrets.rs` | Add `CookieJar` extractor; pass `csrf_token` to template context |
| `crates/agentos-web/src/handlers/pipelines.rs` | Add `CookieJar` extractor; pass `csrf_token` to template context |
| `crates/agentos-web/src/handlers/audit.rs` | Add `CookieJar` extractor; pass `csrf_token` to template context |
| `crates/agentos-web/Cargo.toml` | Add `dashmap = "6"` dependency (if not already added in Phase 03) |

---

## Dependencies

**Requires:** [[03-cors-auth-csp-ratelimit]] must be complete first. The CSRF middleware depends on the `agentos_session` cookie set by the auth middleware's login handler.

**Blocks:** Nothing directly.

---

## Test Plan

1. **CSRF token generation test:** Make a GET request to `/agents` with a valid session cookie. Verify the response HTML contains a `<meta name="csrf-token" content="...">` tag with a non-empty 64-character hex value.

2. **CSRF validation -- valid token:** Make a POST request to `/agents` with a valid session cookie and matching `X-CSRF-Token` header. Verify it returns 200 or 302 (success).

3. **CSRF validation -- missing token:** Make a POST request to `/agents` with a valid session cookie but NO `X-CSRF-Token` header. Verify it returns 403 Forbidden with body "CSRF token missing or invalid".

4. **CSRF validation -- wrong token:** Make a POST request to `/agents` with a valid session cookie and `X-CSRF-Token: wrong-value`. Verify it returns 403.

5. **CSRF bypass for bearer token:** Make a POST request with `Authorization: Bearer <token>` and no session cookie. Verify it succeeds without a CSRF token (API clients do not need CSRF protection since they are not vulnerable to cross-origin attacks).

6. **CSRF bypass for static files:** `GET /static/css/pico.min.css` succeeds without any CSRF involvement.

7. **HTMX integration:** Verify that the rendered HTML contains the `htmx:configRequest` event listener that injects `X-CSRF-Token` headers.

---

## Verification

```bash
# Must compile
cargo build -p agentos-web

# Tests pass
cargo test -p agentos-web

# Verify CSRF module exists
test -f crates/agentos-web/src/csrf.rs && echo "csrf.rs exists"

# Verify CSRF middleware wired
grep -n "csrf_middleware" crates/agentos-web/src/router.rs

# Verify DashMap in state
grep -n "csrf_tokens" crates/agentos-web/src/state.rs

# Verify CSRF meta tag in base template
grep -n "csrf-token" crates/agentos-web/src/templates/base.html

# Verify HTMX configRequest listener
grep -n "htmx:configRequest" crates/agentos-web/src/templates/base.html
```

---

## Related

- [[WebUI Security Fixes Plan]] -- Master plan
- [[WebUI Security Fixes Data Flow]] -- Flow diagrams
- [[03-cors-auth-csp-ratelimit]] -- prerequisite (provides session infrastructure)
