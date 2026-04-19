---
title: "Phase 3: OAuth Web Flow"
tags:
  - plan
  - real-world
  - web
  - auth
  - phase-3
date: 2026-04-08
status: complete
effort: 1.5d
priority: high
---

# Phase 3: OAuth Web Flow

> Add Axum routes and Web UI pages so human operators can authorize external services (GitHub, Google, Slack) via OAuth2, storing the resulting tokens in the vault for agent use.

---

## Why This Phase

Agents cannot click "Authorize" buttons on third-party websites. A human operator must perform the OAuth2 authorization code flow (redirect to provider → grant access → receive callback with code → exchange for tokens). This phase adds the web infrastructure for that handshake.

After this phase, the full flow is:
1. Operator clicks "Connect GitHub" in the Web UI
2. Browser redirects to GitHub's OAuth consent screen
3. GitHub redirects back to AgentOS callback URL with an authorization code
4. AgentOS exchanges the code for tokens, stores them in the vault
5. Kernel emits `AgentPermissionGranted` event
6. Agent immediately gets access to `github.*` connector tools

---

## Current State

- `agentos-web` has Axum router with auth middleware (Bearer token + session cookie)
- CSRF protection via `DashMap<String, (String, Instant)>` in `AppState`
- MiniJinja2 templating with Pico CSS + HTMX
- No OAuth routes or connector management pages exist
- Phase 1 provides `store_oauth()`, `store_pending_flow()`, `complete_pending_flow()` in vault
- Phase 2 provides `ConnectorRegistry` and `ConnectorManifest` with `AuthType::OAuth2`

## Target State

- `GET /auth/:connector_id/start` — initiates OAuth2 flow (generates state, PKCE, redirects)
- `GET /auth/:connector_id/callback` — handles provider callback, exchanges code, stores tokens
- `GET /connectors` — Web UI page listing connected services with connect/disconnect buttons
- `POST /connectors/:connector_id/disconnect` — revokes tokens and deregisters connector
- `KernelCommand::ConnectorConnect` / `ConnectorDisconnect` / `ConnectorList`

---

## Detailed Subtasks

### 1. Add OAuth handler module

**File:** `crates/agentos-web/src/handlers/oauth.rs` (new)

```rust
use axum::{extract::{Path, State, Query}, response::Redirect};

/// GET /auth/:connector_id/start
/// 1. Look up ConnectorManifest to get auth_type and scopes
/// 2. Generate random `state` param (32 bytes, hex-encoded)
/// 3. Generate PKCE `code_verifier` and `code_challenge`
/// 4. Store pending flow in vault: store_pending_flow(state, code_verifier, ...)
/// 5. Build authorization URL with client_id, redirect_uri, scope, state, code_challenge
/// 6. Return 302 redirect to the authorization URL
pub async fn start_oauth(
    State(state): State<Arc<AppState>>,
    Path(connector_id): Path<String>,
) -> Result<Redirect, AppError>;

/// GET /auth/:connector_id/callback?code=...&state=...
/// 1. Validate `state` param against pending flows (CSRF protection)
/// 2. Retrieve code_verifier from pending flow
/// 3. POST to token_endpoint with authorization_code, code_verifier, redirect_uri
/// 4. Parse response: access_token, refresh_token, expires_in, scope
/// 5. Store OAuthCredential in vault via store_oauth()
/// 6. Delete pending flow
/// 7. Emit AgentPermissionGranted event
/// 8. Register connector tools in ConnectorRegistry
/// 9. Redirect to /connectors with success flash
pub async fn oauth_callback(
    State(state): State<Arc<AppState>>,
    Path(connector_id): Path<String>,
    Query(params): Query<OAuthCallbackParams>,
) -> Result<Redirect, AppError>;

#[derive(Deserialize)]
pub struct OAuthCallbackParams {
    pub code: String,
    pub state: String,
    pub error: Option<String>,        // provider may redirect with error
    pub error_description: Option<String>,
}
```

### 2. Add connector management page

**File:** `crates/agentos-web/src/handlers/connectors.rs` (new)

```rust
/// GET /connectors — list all connectors with status
pub async fn list_connectors(
    State(state): State<Arc<AppState>>,
) -> Result<Html<String>, AppError>;

/// POST /connectors/:connector_id/disconnect — revoke and remove
pub async fn disconnect_connector(
    State(state): State<Arc<AppState>>,
    Path(connector_id): Path<String>,
) -> Result<Redirect, AppError>;
```

**Template:** `crates/agentos-web/templates/connectors.html`

```html
<!-- Pico CSS semantic layout -->
<main class="container">
  <h1>Connected Services</h1>
  <table>
    <thead><tr><th>Service</th><th>Status</th><th>Scopes</th><th>Expires</th><th></th></tr></thead>
    <tbody>
      {% for c in connectors %}
      <tr>
        <td>{{ c.name }}</td>
        <td>{{ c.status }}</td>
        <td>{{ c.scopes | join(", ") }}</td>
        <td>{{ c.expires_at | default("never") }}</td>
        <td>
          {% if c.connected %}
            <form method="post" action="/connectors/{{ c.id }}/disconnect">
              <input type="hidden" name="_csrf" value="{{ csrf }}">
              <button class="secondary outline">Disconnect</button>
            </form>
          {% else %}
            <a href="/auth/{{ c.id }}/start" role="button">Connect</a>
          {% endif %}
        </td>
      </tr>
      {% endfor %}
    </tbody>
  </table>
</main>
```

### 3. Add OAuth provider config

**File:** `config/oauth_providers.toml` (new)

Store non-secret OAuth provider metadata (client_id is semi-public, client_secret goes in vault):

```toml
[github]
authorize_url = "https://github.com/login/oauth/authorize"
token_url = "https://github.com/login/oauth/access_token"
client_id_env = "GITHUB_CLIENT_ID"       # read from env at boot
client_secret_vault_key = "github_client_secret"  # read from vault
default_scopes = ["repo", "read:org"]

[google]
authorize_url = "https://accounts.google.com/o/oauth2/v2/auth"
token_url = "https://oauth2.googleapis.com/token"
client_id_env = "GOOGLE_CLIENT_ID"
client_secret_vault_key = "google_client_secret"
default_scopes = ["https://www.googleapis.com/auth/calendar.readonly"]

[slack]
authorize_url = "https://slack.com/oauth/v2/authorize"
token_url = "https://slack.com/api/oauth.v2.access"
client_id_env = "SLACK_CLIENT_ID"
client_secret_vault_key = "slack_client_secret"
default_scopes = ["chat:write", "channels:read"]
```

### 4. Register routes in router

**File:** `crates/agentos-web/src/router.rs`

Add to `build_router()`:

```rust
.route("/auth/{connector_id}/start", get(oauth::start_oauth))
.route("/auth/{connector_id}/callback", get(oauth::oauth_callback))
.route("/connectors", get(connectors::list_connectors))
.route("/connectors/{connector_id}/disconnect", post(connectors::disconnect_connector))
```

All routes require authentication (existing `require_auth` middleware).

### 5. KernelCommand wiring

**File:** `crates/agentos-bus/src/message.rs`

Add variants:
```rust
ConnectorList,
ConnectorConnect { connector_id: String, credential_json: String },
ConnectorDisconnect { connector_id: String },
```

**File:** `crates/agentos-kernel/src/commands/connector.rs` (new)

Handlers that delegate to `ConnectorRegistry` and `SecretsVault`.

**File:** `crates/agentos-kernel/src/run_loop.rs`

Add dispatch arms for the new commands.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-web/src/handlers/oauth.rs` | **New** — OAuth start + callback handlers |
| `crates/agentos-web/src/handlers/connectors.rs` | **New** — Connector list + disconnect handlers |
| `crates/agentos-web/src/handlers/mod.rs` | Add `pub mod oauth; pub mod connectors;` |
| `crates/agentos-web/src/router.rs` | Register 4 new routes |
| `crates/agentos-web/templates/connectors.html` | **New** — Connected services page |
| `config/oauth_providers.toml` | **New** — Provider metadata |
| `crates/agentos-bus/src/message.rs` | Add `ConnectorList/Connect/Disconnect` variants |
| `crates/agentos-kernel/src/commands/connector.rs` | **New** — Command handlers |
| `crates/agentos-kernel/src/run_loop.rs` | Add dispatch arms |
| `crates/agentos-cli/src/commands/connector.rs` | **New** — `agentos connector list/connect/disconnect` CLI |

---

## Dependencies

- **Requires:** Phase 1 (OAuth Token Lifecycle), Phase 2 (Connector Hub)
- **Blocks:** None (end of Subsystem A)

---

## Test Plan

1. **Unit: state/PKCE generation** — Verify `start_oauth` generates valid state (hex, 32 bytes) and PKCE challenge (S256)
2. **Unit: callback validation** — Verify state mismatch returns 403, expired flow returns 410
3. **Unit: error callback** — Verify provider error params (`?error=access_denied`) render an error page
4. **Integration: full OAuth flow** — Use `wiremock` to mock GitHub's token endpoint; simulate start → callback → token exchange → vault storage
5. **Security: CSRF on disconnect** — Verify POST without valid `_csrf` token is rejected
6. **Security: auth required** — Verify unauthenticated requests to `/auth/*` and `/connectors` return 401

---

## Verification

```bash
cargo build -p agentos-web
cargo test -p agentos-web
cargo test -p agentos-kernel
cargo clippy -p agentos-web -p agentos-kernel -- -D warnings
cargo fmt --all -- --check
```
