---
title: Phase 2 — Mobile OAuth2 Auth Layer
tags:
  - mobile
  - auth
  - oauth2
  - api
  - phase-2
date: 2026-04-19
status: planned
effort: 4d
priority: high
---

# Phase 2 — Mobile OAuth2 Auth Layer

> Add OAuth2 Authorization Code + PKCE on top of `agentos-api`, with short-lived JWT access tokens, rotating refresh tokens, and a user/credential store. Keeps the existing HMAC API-key flow intact for CLI/machine clients — this is purely additive.

---

## Why this phase

Mobile apps cannot use HMAC-signed API keys — users can't paste a 32-byte secret into a phone. OAuth2 with PKCE is the standard mobile flow: the app opens an in-app browser, the user authenticates, and the app receives a short-lived JWT plus a rotating refresh token. This phase also establishes a minimal user/credential store so mobile has something to authenticate against.

## Current → Target state

**Current:**
- `agentos-api` authenticates via HMAC-SHA256 of `X-AgentOS-Timestamp` + body, keyed by an API key.
- No user model — API keys are anonymous principals with a name.
- No token issuance, no sessions, no refresh.

**Target:**
- New module `crates/agentos-api/src/auth/` containing:
  - `users.rs` — user table (`id`, `email`, `display_name`, `password_hash` (Argon2id), `created_at`, `disabled_at`).
  - `oauth.rs` — authorize + token + refresh endpoints.
  - `jwt.rs` — sign/verify JWT access tokens (RS256 via vault-held Ed25519 key).
  - `middleware.rs` — `Bearer <jwt>` extractor; populates `AuthPrincipal { user_id, scopes }`.
- SQLite store `auth.db` at `<data_dir>/auth.db` with tables `users`, `auth_codes`, `refresh_tokens`.
- Routes:
  - `GET /v1/auth/authorize` — login form (HTML) / JSON for API clients
  - `POST /v1/auth/login` — credential check, issues one-time `code` + 302 to `redirect_uri`
  - `POST /v1/auth/token` — exchange `code + code_verifier` → `{access, refresh}`
  - `POST /v1/auth/refresh` — rotate refresh → `{access, refresh}`
  - `POST /v1/auth/logout` — revoke current refresh
  - `GET /v1/auth/me` — return current user
- Both `Bearer <jwt>` and existing HMAC auth accepted; middleware branches on `Authorization` header scheme.
- CLI: `agentos user create|list|disable|enable|reset-password` (uses internal DB, not API).

## Detailed subtasks

### 2.1 Create auth SQLite store

File: `crates/agentos-api/src/auth/store.rs` (new).

```rust
pub struct AuthStore {
    pool: rusqlite::Pool,  // or r2d2-sqlite
}

pub struct User {
    pub id: Uuid,
    pub email: String,
    pub display_name: String,
    pub password_hash: String,  // Argon2id PHC string
    pub created_at: DateTime<Utc>,
    pub disabled_at: Option<DateTime<Utc>>,
}

pub struct AuthCode {
    pub code: String,                 // 43-char url-safe random
    pub code_challenge: String,       // from client
    pub code_challenge_method: String, // "S256"
    pub user_id: Uuid,
    pub redirect_uri: String,
    pub expires_at: DateTime<Utc>,
    pub used_at: Option<DateTime<Utc>>,
}

pub struct RefreshToken {
    pub id: Uuid,
    pub token_hash: String,           // SHA-256 of token (store hash, not token)
    pub user_id: Uuid,
    pub device_id: Option<Uuid>,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub replaced_by: Option<Uuid>,    // for rotation chain detection
}
```

Migrations via `rusqlite_migration` (already a workspace dep — verify). Schema:

```sql
CREATE TABLE users (
    id TEXT PRIMARY KEY,
    email TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    password_hash TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    disabled_at INTEGER
);
CREATE TABLE auth_codes (
    code TEXT PRIMARY KEY,
    code_challenge TEXT NOT NULL,
    code_challenge_method TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id),
    redirect_uri TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    used_at INTEGER
);
CREATE INDEX idx_auth_codes_expires ON auth_codes(expires_at);
CREATE TABLE refresh_tokens (
    id TEXT PRIMARY KEY,
    token_hash TEXT NOT NULL UNIQUE,
    user_id TEXT NOT NULL REFERENCES users(id),
    device_id TEXT,
    issued_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    revoked_at INTEGER,
    replaced_by TEXT
);
CREATE INDEX idx_refresh_user ON refresh_tokens(user_id);
CREATE INDEX idx_refresh_expires ON refresh_tokens(expires_at);
```

All SQL via parameterized queries. No string interpolation.

### 2.2 Password hashing

Use `argon2` crate (workspace dep — `argon2 = "0.5"`). Default params: `Argon2::default()` which is Argon2id. Never log or serialize `password_hash`.

```rust
pub fn hash_password(plaintext: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let hash = argon2.hash_password(plaintext.as_bytes(), &salt)?.to_string();
    Ok(hash)
}
pub fn verify_password(plaintext: &str, phc: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(phc) else { return false };
    Argon2::default().verify_password(plaintext.as_bytes(), &parsed).is_ok()
}
```

### 2.3 JWT signing key

JWTs are signed with an Ed25519 key that lives in the vault under a well-known slot `auth.jwt_signer`. If missing on first boot, generate one (`Ed25519KeyPair::generate`) and store it. Use the `jsonwebtoken` crate with `Algorithm::EdDSA`.

Claims:

```rust
#[derive(Serialize, Deserialize)]
pub struct AccessClaims {
    pub sub: Uuid,           // user_id
    pub email: String,
    pub iat: i64,
    pub exp: i64,            // iat + 15min
    pub jti: Uuid,
    pub scopes: Vec<String>, // e.g. ["tasks:write", "chat:stream"]
}
```

Verify `exp`, `iat`, signature. Reject tokens where `disabled_at IS NOT NULL` on the user row — re-check on every request (cheap: one indexed lookup).

### 2.4 Authorize endpoint

File: `crates/agentos-api/src/handlers/auth.rs` (new).

```rust
pub async fn authorize(Query(q): Query<AuthorizeParams>) -> impl IntoResponse {
    // Validate params: response_type=code, code_challenge_method=S256
    // Render login HTML form with hidden fields (code_challenge, redirect_uri, state)
}
pub async fn login(State(s): State<AppState>, Form(f): Form<LoginForm>) -> impl IntoResponse {
    let user = s.auth_store.find_user_by_email(&f.email).await?;
    if user.disabled_at.is_some() || !verify_password(&f.password, &user.password_hash) {
        return Redirect::to("/v1/auth/authorize?error=invalid_credentials").into_response();
    }
    let code = generate_url_safe_token(32);
    s.auth_store.insert_auth_code(AuthCode {
        code: code.clone(),
        code_challenge: f.code_challenge,
        code_challenge_method: "S256".into(),
        user_id: user.id,
        redirect_uri: f.redirect_uri.clone(),
        expires_at: Utc::now() + Duration::seconds(60),
        used_at: None,
    }).await?;
    Redirect::to(&format!("{}?code={}&state={}", f.redirect_uri, code, f.state)).into_response()
}
```

**Security:** validate `redirect_uri` against an allowlist (`config.auth.allowed_redirect_uris`, default `["agentos://callback", "http://localhost"]`).

### 2.5 Token endpoint

```rust
pub async fn token(State(s): State<AppState>, Json(req): Json<TokenRequest>) -> impl IntoResponse {
    match req.grant_type.as_str() {
        "authorization_code" => {
            let row = s.auth_store.consume_auth_code(&req.code).await?;
            // Verify PKCE
            let digest = Sha256::digest(req.code_verifier.as_bytes());
            let b64 = URL_SAFE_NO_PAD.encode(digest);
            if b64 != row.code_challenge { return Err(AuthError::PkceMismatch); }
            if row.redirect_uri != req.redirect_uri { return Err(AuthError::RedirectMismatch); }
            let user = s.auth_store.get_user(row.user_id).await?;
            let access = issue_access_token(&user, &s.jwt_signer)?;
            let refresh = issue_refresh_token(&user, req.device_id, &s.auth_store).await?;
            Ok(Json(TokenResponse { access_token: access, refresh_token: refresh, token_type: "Bearer", expires_in: 900 }))
        }
        _ => Err(AuthError::UnsupportedGrant),
    }
}
```

`consume_auth_code` must set `used_at` atomically; any subsequent use returns error.

### 2.6 Refresh endpoint (rotation + reuse detection)

```rust
pub async fn refresh(State(s): State<AppState>, Json(req): Json<RefreshRequest>) -> impl IntoResponse {
    let hash = sha256_hex(&req.refresh_token);
    let row = s.auth_store.find_refresh(&hash).await?;
    if row.revoked_at.is_some() {
        // REUSE DETECTED — attacker presenting an already-rotated token.
        // Revoke entire refresh chain for this user.
        s.auth_store.revoke_all_refresh_for_user(row.user_id).await?;
        return Err(AuthError::TokenReuse);
    }
    if row.expires_at < Utc::now() { return Err(AuthError::Expired); }
    // Issue new pair, mark old as revoked + replaced_by new id.
    let user = s.auth_store.get_user(row.user_id).await?;
    let access = issue_access_token(&user, &s.jwt_signer)?;
    let (new_id, new_refresh) = issue_refresh_token(&user, row.device_id, &s.auth_store).await?;
    s.auth_store.revoke_refresh(row.id, Some(new_id)).await?;
    Ok(Json(TokenResponse { access_token: access, refresh_token: new_refresh, ... }))
}
```

Reuse detection is the classic OWASP recommendation — presenting an already-rotated refresh token signals theft, so we nuke the whole chain.

### 2.7 Auth middleware

File: `crates/agentos-api/src/auth/middleware.rs` (new). Plug into the existing middleware stack in `crates/agentos-api/src/service.rs`.

```rust
pub async fn auth_middleware(State(s): State<AppState>, mut req: Request<Body>, next: Next) -> Response {
    let hdr = req.headers().get("authorization").and_then(|h| h.to_str().ok()).unwrap_or("");
    let principal = if let Some(jwt) = hdr.strip_prefix("Bearer ") {
        match verify_access_token(jwt, &s.jwt_verifier) {
            Ok(claims) => AuthPrincipal::User { user_id: claims.sub, scopes: claims.scopes },
            Err(_) => return (StatusCode::UNAUTHORIZED, "invalid token").into_response(),
        }
    } else {
        // Fall back to existing HMAC flow.
        match s.hmac_verify(&req).await {
            Ok(api_key) => AuthPrincipal::ApiKey { key_id: api_key.id, scopes: api_key.scopes },
            Err(_) => return (StatusCode::UNAUTHORIZED, "auth required").into_response(),
        }
    };
    req.extensions_mut().insert(principal);
    next.run(req).await
}
```

Health routes (`/healthz`, `/readyz`) and `/v1/auth/*` MUST be excluded from this middleware (mounted before it).

### 2.8 CLI user management

File: `crates/agentos-cli/src/commands/user.rs` (new).

```
agentos user create <email> [--display-name <name>]       # prompts for password
agentos user list
agentos user disable <email>
agentos user enable <email>
agentos user reset-password <email>                        # prompts for new password
```

All operate directly on the auth DB via `agentos-api::auth::store::AuthStore` — no bus call needed. Requires vault password to open `auth.db` (we use the same SQLCipher wrapper the audit log uses; verify with `grep -n "sqlcipher" crates/agentos-audit/`).

### 2.9 Sweep expired auth codes + refresh tokens

Extend the existing `TimeoutChecker` loop in `crates/agentos-kernel/src/` to also sweep:
- `auth_codes WHERE expires_at < now AND used_at IS NULL` → delete
- `refresh_tokens WHERE expires_at < now OR revoked_at IS NOT NULL AND revoked_at < now - 7d` → delete

Audit event: `AuthTokensPruned { auth_codes_removed, refresh_tokens_removed }`.

## Files changed

| File | Change |
|------|--------|
| `crates/agentos-api/src/auth/mod.rs` | new |
| `crates/agentos-api/src/auth/store.rs` | new |
| `crates/agentos-api/src/auth/oauth.rs` | new — issue/verify helpers |
| `crates/agentos-api/src/auth/jwt.rs` | new |
| `crates/agentos-api/src/auth/middleware.rs` | new |
| `crates/agentos-api/src/handlers/auth.rs` | new |
| `crates/agentos-api/src/service.rs` | mount auth routes + middleware |
| `crates/agentos-api/Cargo.toml` | add `jsonwebtoken`, `argon2`, `sha2`, `base64` |
| `crates/agentos-cli/src/commands/user.rs` | new |
| `crates/agentos-cli/src/main.rs` | dispatch `user` subcommand |
| `crates/agentos-audit/src/events.rs` | add `AuthLoginSuccess`, `AuthLoginFailure`, `AuthRefreshRotated`, `AuthTokenReuseDetected`, `AuthTokensPruned` |
| `crates/agentos-kernel/src/timeout_checker.rs` | sweep auth tables |
| `config/default.toml` | add `[auth]` section (`allowed_redirect_uris`, `access_token_ttl_seconds`, `refresh_token_ttl_days`) |

## Dependencies

- [[01-cloud-deployment-foundation]] — TLS-terminated API surface.

## Test plan

- Unit: PKCE S256 match + mismatch.
- Unit: JWT issuance + verify; verify rejects expired, bad signature, disabled user.
- Unit: Refresh rotation — old token rejected after rotate.
- Unit: Refresh reuse detection — reusing revoked token revokes entire chain.
- Unit: `redirect_uri` allowlist blocks `https://attacker.example/`.
- Integration: end-to-end auth code flow against test server — `authorize → login → token → GET /v1/auth/me`.
- Integration: existing HMAC API keys still work (regression).
- Security: try replaying consumed auth code — rejected.
- Security: try using access token after disabling user — rejected.

## Verification

```bash
cargo test -p agentos-api --features auth
cargo clippy -p agentos-api -- -D warnings
# Smoke: spin up server, run e2e auth script (added under deploy/scripts/test-auth.sh)
./deploy/scripts/test-auth.sh https://agentos.example.com test@example.com
```

## Related

- [[Mobile App Plan]]
- [[01-cloud-deployment-foundation]]
- [[03-device-registration-and-push-relay]] — uses `device_id` from this phase
- [[05-mobile-app-scaffold-and-auth]] — client side of this flow
