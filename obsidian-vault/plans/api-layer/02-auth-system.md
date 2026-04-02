---
title: "Phase 2: Auth System (API Keys + JWT)"
tags:
  - api
  - security
  - v3
  - phase-2
date: 2026-03-30
status: planned
effort: 2d
priority: high
---

# Phase 2: Auth System (API Keys + JWT)

> Implement API key management and JWT token exchange for the REST/WebSocket API, including CLI commands, vault storage, and Axum middleware.

---

## Why This Phase

The current auth model (single bearer token printed at startup) can't scope permissions per consumer, can't be rotated, and can't be audited. External integrations need named, scoped, revocable API keys that exchange for short-lived JWTs.

## Current State

- Single bearer token generated at `WebServer::new()` in `crates/agentos-web/src/server.rs`
- Session cookie (HTTP-only, SameSite=Strict, 8h max-age) for browser sessions
- No per-consumer identity or permission scoping
- No API key storage, no JWT infrastructure

## Target State

- API keys: `agos_` + 32 random hex bytes, stored hashed in SQLite, scoped to `PermissionSet`
- JWT: RS256-signed, 1h access token + 24h refresh token
- CLI: `agentctl auth create-key`, `list-keys`, `revoke-key`
- Bus commands: `CreateApiKey`, `ListApiKeys`, `RevokeApiKey`
- Axum middleware: extract JWT from `Authorization: Bearer` header, attach `AuthClaims`
- Audit logging for key creation, usage, revocation

## Detailed Subtasks

### 1. Add dependencies to `agentos-api`

Add to `crates/agentos-api/Cargo.toml`:
```toml
jsonwebtoken = "9"        # JWT signing/verification
axum = { workspace = true }
axum-extra = "0.12"       # cookie extraction (if needed)
sha2 = "0.10"             # API key hashing
rand = { workspace = true }
```

### 2. API key storage

**New file: `crates/agentos-api/src/auth/api_keys.rs`**

```rust
pub struct ApiKeyStore {
    db: rusqlite::Connection,
}

pub struct ApiKeyRecord {
    pub name: String,
    pub key_hash: String,       // SHA-256 hex
    pub permissions: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
}

impl ApiKeyStore {
    pub fn new(data_dir: &Path) -> Result<Self> {
        // Opens/creates api_keys.db in kernel data dir
        // CREATE TABLE IF NOT EXISTS api_keys (
        //   name TEXT PRIMARY KEY,
        //   key_hash TEXT NOT NULL,
        //   permissions TEXT NOT NULL,  -- JSON array
        //   created_at TEXT NOT NULL,
        //   last_used_at TEXT,
        //   expires_at TEXT
        // )
    }

    pub fn create_key(&self, name: &str, permissions: &[String], expires_at: Option<DateTime<Utc>>) -> Result<String> {
        // Generate: "agos_" + 32 random bytes hex = 68 chars
        // Store SHA-256(key) in db
        // Return plaintext key (shown once)
    }

    pub fn validate_key(&self, key: &str) -> Result<ApiKeyRecord> {
        // Hash the key, look up in db
        // Check not expired
        // Update last_used_at
        // Return record with permissions
    }

    pub fn list_keys(&self) -> Result<Vec<ApiKeyRecord>> { ... }
    pub fn revoke_key(&self, name: &str) -> Result<()> { ... }
}
```

### 3. JWT signing and verification

**New file: `crates/agentos-api/src/auth/jwt.rs`**

```rust
pub struct JwtManager {
    encoding_key: EncodingKey,  // RSA private
    decoding_key: DecodingKey,  // RSA public
    revoked_jtis: Arc<RwLock<HashSet<String>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthClaims {
    pub sub: String,             // "key:<name>"
    pub permissions: Vec<String>,
    pub iat: i64,
    pub exp: i64,
    pub jti: String,             // unique token ID
}

impl JwtManager {
    pub fn new(vault: &SecretsVault) -> Result<Self> {
        // Load or generate RSA keypair from vault
        // Key name: "_internal_jwt_rsa"
    }

    pub fn issue_access_token(&self, key_record: &ApiKeyRecord) -> Result<String> {
        // Claims with key's permissions, 1h expiry, random jti
    }

    pub fn issue_refresh_token(&self, key_name: &str) -> Result<String> {
        // Minimal claims, 24h expiry
    }

    pub fn verify(&self, token: &str) -> Result<AuthClaims> {
        // Verify signature, check expiry, check revocation set
    }

    pub fn revoke(&self, jti: &str) {
        // Add to revocation set
    }
}

impl AuthClaims {
    pub fn require(&self, permission: &str) -> Result<(), ApiError> {
        // Check if self.permissions contains the required permission
        // Return Forbidden if not
    }
}
```

### 4. Auth middleware for API routes

**New file: `crates/agentos-api/src/auth/middleware.rs`**

```rust
pub async fn api_auth_layer(
    State(jwt_manager): State<Arc<JwtManager>>,
    mut req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let token = extract_bearer_token(&req)
        .ok_or(ApiError::Unauthorized)?;

    let claims = jwt_manager.verify(&token)
        .map_err(|_| ApiError::Unauthorized)?;

    req.extensions_mut().insert(claims);
    Ok(next.run(req).await)
}

fn extract_bearer_token(req: &Request) -> Option<&str> {
    req.headers()
        .get("Authorization")?
        .to_str().ok()?
        .strip_prefix("Bearer ")
}
```

### 5. Auth REST endpoints

**New file: `crates/agentos-api/src/rest/auth.rs`**

```rust
// POST /api/v1/auth/token
pub async fn exchange_token(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<TokenRequest>,
) -> ApiResult<TokenResponse> {
    let key_record = state.key_store.validate_key(&req.api_key)
        .map_err(|_| ApiError::Unauthorized)?;

    let access_token = state.jwt_manager.issue_access_token(&key_record)?;
    let refresh_token = state.jwt_manager.issue_refresh_token(&key_record.name)?;

    // Audit log
    state.audit.log_api_key_used(&key_record.name, "token_exchange").await;

    Ok(ApiResponse::ok(TokenResponse {
        access_token,
        refresh_token,
        expires_in: 3600,
        token_type: "Bearer".into(),
    }))
}

// POST /api/v1/auth/refresh
pub async fn refresh_token(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<RefreshRequest>,
) -> ApiResult<TokenResponse> {
    let claims = state.jwt_manager.verify(&req.refresh_token)
        .map_err(|_| ApiError::Unauthorized)?;

    // Look up original key to get current permissions
    let key_name = claims.sub.strip_prefix("key:").unwrap_or(&claims.sub);
    let key_record = state.key_store.list_keys()?
        .into_iter().find(|k| k.name == key_name)
        .ok_or(ApiError::Unauthorized)?;

    let access_token = state.jwt_manager.issue_access_token(&key_record)?;

    Ok(ApiResponse::ok(TokenResponse {
        access_token,
        refresh_token: req.refresh_token, // reuse until it expires
        expires_in: 3600,
        token_type: "Bearer".into(),
    }))
}
```

### 6. Bus commands for key management

Add to `crates/agentos-bus/src/message.rs`:
```rust
// In KernelCommand enum:
CreateApiKey { name: String, permissions: Vec<String>, expires_secs: Option<u64> },
ListApiKeys,
RevokeApiKey { name: String },
```

Add to `KernelResponse`:
```rust
ApiKeyCreated { name: String, key: String },  // plaintext shown once
ApiKeyList(Vec<ApiKeyInfo>),
```

### 7. CLI commands

Add to `crates/agentos-cli/src/commands/`:
```rust
// auth.rs (new file)
#[derive(Subcommand)]
pub enum AuthCommand {
    CreateKey {
        #[arg(long)]
        name: String,
        #[arg(long)]
        permissions: String,  // comma-separated: "tasks:r,agents:r"
        #[arg(long)]
        expires: Option<String>,  // "30d", "1h", etc.
    },
    ListKeys,
    RevokeKey {
        #[arg(long)]
        name: String,
    },
}
```

### 8. KernelService auth methods

Add to `KernelService` trait:
```rust
async fn create_api_key(&self, name: &str, permissions: &[String], expires_at: Option<DateTime<Utc>>) -> Result<String, ApiError>;
async fn list_api_keys(&self) -> Result<Vec<ApiKeyInfo>, ApiError>;
async fn revoke_api_key(&self, name: &str) -> Result<(), ApiError>;
```

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-api/Cargo.toml` | Add `jsonwebtoken`, `sha2`, `rand`, `axum` deps |
| `crates/agentos-api/src/auth/mod.rs` | New module |
| `crates/agentos-api/src/auth/api_keys.rs` | API key store (SQLite) |
| `crates/agentos-api/src/auth/jwt.rs` | JWT manager (RS256) |
| `crates/agentos-api/src/auth/middleware.rs` | Axum auth middleware |
| `crates/agentos-api/src/rest/auth.rs` | Token exchange + refresh endpoints |
| `crates/agentos-api/src/service.rs` | Add auth methods to trait |
| `crates/agentos-api/src/kernel_impl.rs` | Implement auth methods |
| `crates/agentos-api/src/types/auth.rs` | TokenRequest, TokenResponse, ApiKeyInfo |
| `crates/agentos-bus/src/message.rs` | Add `CreateApiKey`, `ListApiKeys`, `RevokeApiKey` commands |
| `crates/agentos-cli/src/commands/auth.rs` | New CLI subcommand group |
| `crates/agentos-cli/src/commands/mod.rs` | Register auth subcommand |
| `crates/agentos-kernel/src/run_loop.rs` | Dispatch new auth commands |

## Dependencies

- **Requires:** Phase 1 (KernelService trait exists)
- **Blocks:** Phase 3, 4, 5 (all API endpoints need auth middleware)

## Test Plan

1. Create API key → verify plaintext returned, hash stored in SQLite
2. Validate API key → returns correct permissions
3. Validate expired key → returns error
4. Revoke key → subsequent validation fails
5. Exchange key for JWT → valid RS256 token with correct claims
6. Verify JWT → claims extracted correctly
7. Verify expired JWT → returns error
8. Verify revoked JWT (by jti) → returns error
9. Refresh token → new access token issued with current permissions
10. Auth middleware → 401 without token, 200 with valid token
11. Permission check → 403 when `claims.require()` fails
12. CLI `create-key` → key printed, `list-keys` shows it, `revoke-key` removes it

## Verification

```bash
cargo build -p agentos-api
cargo test -p agentos-api
cargo build -p agentos-cli
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```
