---
title: "Phase 1: OAuth Token Lifecycle"
tags:
  - plan
  - real-world
  - security
  - vault
  - phase-1
date: 2026-04-08
status: complete
effort: 2d
priority: high
---

# Phase 1: OAuth Token Lifecycle

> Extend `agentos-vault` with OAuth2 credential storage, automatic token refresh, and memory redaction so agents never see raw tokens.

---

## Why This Phase

Agents that interact with external APIs (GitHub, Slack, Stripe, Google) need OAuth2 credentials. Today, the vault stores static secrets (key-value pairs) but has no concept of:

- **Token expiry** — OAuth2 access tokens expire (typically 1h); the agent gets a 401 mid-task
- **Refresh flow** — refresh tokens must be exchanged before access tokens expire
- **PKCE state** — OAuth2 authorization code flow requires tracking `state` and `code_verifier` across the redirect
- **Credential structure** — OAuth2 credentials are multi-field (access_token, refresh_token, token_type, expires_at, scopes)

The existing `SecretsVault` in `crates/agentos-vault/src/vault.rs` stores opaque `ZeroizingString` values. This phase adds a typed `OAuthCredential` layer on top, with a background refresh loop.

---

## Current State

- `SecretsVault` stores secrets as encrypted blobs in SQLite (`vault.db`)
- `ProxyVault` issues opaque proxy handles so agents never see raw secret values
- `SecretScope` and `SecretOwner` control access
- All vault operations are audit-logged
- No OAuth2-specific logic exists anywhere in the codebase

## Target State

- New `OAuthCredential` struct with typed fields (access_token, refresh_token, expires_at, scopes, etc.)
- New `oauth_credentials` SQLite table in vault.db
- `store_oauth()` / `get_oauth()` / `delete_oauth()` methods on `SecretsVault`
- Background `TokenRefreshLoop` that proactively refreshes tokens before expiry
- `ContextRedactor` that strips `Bearer ...` patterns from any text entering the context window
- Audit events: `OAuthCredentialStored`, `OAuthTokenRefreshed`, `OAuthTokenExpired`

---

## Detailed Subtasks

### 1. Define `OAuthCredential` type

**File:** `crates/agentos-vault/src/oauth.rs` (new)

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

#[derive(Debug, Clone, Serialize, Deserialize, Zeroize)]
#[zeroize(drop)]
pub struct OAuthCredential {
    pub connector_id: String,
    pub provider: String,           // "github", "slack", "google", etc.
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub token_type: String,         // "Bearer"
    pub expires_at: Option<DateTime<Utc>>,
    pub scopes: Vec<String>,
    pub token_endpoint: String,     // URL for token refresh
    pub client_id: String,          // stored here, NOT the client_secret
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthPendingFlow {
    pub connector_id: String,
    pub state: String,              // CSRF state parameter
    pub code_verifier: Option<String>, // PKCE
    pub redirect_uri: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,  // 10min TTL
}
```

### 2. Add SQLite schema for OAuth credentials

**File:** `crates/agentos-vault/src/oauth.rs`

Add a `create_oauth_tables()` function called from `SecretsVault::initialize()`:

```sql
CREATE TABLE IF NOT EXISTS oauth_credentials (
    connector_id TEXT PRIMARY KEY,
    provider TEXT NOT NULL,
    encrypted_payload BLOB NOT NULL,   -- AES-256-GCM encrypted OAuthCredential JSON
    expires_at TEXT,                    -- plaintext for refresh scheduling
    owner TEXT NOT NULL,               -- SecretOwner serialized
    scope TEXT NOT NULL,               -- SecretScope serialized
    created_at TEXT NOT NULL,
    refreshed_at TEXT
);

CREATE TABLE IF NOT EXISTS oauth_pending_flows (
    state TEXT PRIMARY KEY,
    connector_id TEXT NOT NULL,
    encrypted_verifier BLOB,           -- PKCE code_verifier, encrypted
    redirect_uri TEXT NOT NULL,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);
```

### 3. Implement vault methods

**File:** `crates/agentos-vault/src/vault.rs`

Add methods to `SecretsVault`:

```rust
impl SecretsVault {
    /// Store an OAuth credential (encrypts at rest)
    pub fn store_oauth(
        &self,
        credential: &OAuthCredential,
        owner: SecretOwner,
        scope: SecretScope,
    ) -> Result<(), AgentOSError>;

    /// Retrieve a decrypted OAuth credential by connector_id
    pub fn get_oauth(&self, connector_id: &str) -> Result<OAuthCredential, AgentOSError>;

    /// Delete an OAuth credential
    pub fn delete_oauth(&self, connector_id: &str) -> Result<(), AgentOSError>;

    /// List all connector_ids with their provider and expiry (no secrets)
    pub fn list_oauth(&self) -> Result<Vec<OAuthCredentialMeta>, AgentOSError>;

    /// Update only the access_token and expires_at (after refresh)
    pub fn refresh_oauth(
        &self,
        connector_id: &str,
        new_access_token: &str,
        new_expires_at: Option<DateTime<Utc>>,
        new_refresh_token: Option<&str>,
    ) -> Result<(), AgentOSError>;

    /// Store a pending OAuth flow (for PKCE state tracking)
    pub fn store_pending_flow(&self, flow: &OAuthPendingFlow) -> Result<(), AgentOSError>;

    /// Complete a pending flow (lookup by state param, delete after use)
    pub fn complete_pending_flow(&self, state: &str) -> Result<OAuthPendingFlow, AgentOSError>;

    /// Sweep expired pending flows (called by TimeoutChecker)
    pub fn sweep_expired_flows(&self) -> Result<u64, AgentOSError>;
}
```

### 4. Background token refresh loop

**File:** `crates/agentos-vault/src/token_refresh.rs` (new)

```rust
pub struct TokenRefreshLoop {
    vault: Arc<SecretsVault>,
    audit: Arc<dyn AuditLog>,
    cancel: CancellationToken,
    http_client: reqwest::Client,
}

impl TokenRefreshLoop {
    pub fn new(vault, audit, cancel) -> Self;

    /// Spawns a tokio task that checks every 60s for tokens expiring within 5min
    pub fn start(self: Arc<Self>) -> tokio::task::JoinHandle<()>;

    /// Refresh a single credential using its refresh_token
    async fn refresh_credential(&self, cred: &OAuthCredential) -> Result<(), AgentOSError>;
}
```

The refresh loop:
1. Every 60s, query `oauth_credentials` for rows where `expires_at < now + 5min`
2. For each, POST to `token_endpoint` with `grant_type=refresh_token`
3. On success, call `vault.refresh_oauth()` with the new tokens
4. On failure (invalid_grant), emit `OAuthTokenExpired` audit event and mark credential as expired
5. Respect `CancellationToken` for clean shutdown

### 5. Context redactor

**File:** `crates/agentos-vault/src/redactor.rs` (new)

```rust
use regex::Regex;

pub struct ContextRedactor {
    patterns: Vec<Regex>,
}

impl ContextRedactor {
    pub fn new() -> Self {
        Self {
            patterns: vec![
                Regex::new(r"Bearer\s+[A-Za-z0-9\-._~+/]+=*").unwrap(),
                Regex::new(r"token[\"']?\s*[:=]\s*[\"'][A-Za-z0-9\-._~+/]{20,}[\"']").unwrap(),
                Regex::new(r"ghp_[A-Za-z0-9]{36}").unwrap(),       // GitHub PAT
                Regex::new(r"sk-[A-Za-z0-9]{32,}").unwrap(),       // OpenAI/Stripe keys
                Regex::new(r"xoxb-[A-Za-z0-9\-]+").unwrap(),       // Slack bot token
            ],
        }
    }

    /// Redact sensitive patterns from text, replacing with [REDACTED]
    pub fn redact(&self, text: &str) -> String;
}
```

### 6. Audit event types

**File:** `crates/agentos-audit/src/lib.rs`

Add variants to the audit event type enum:
- `OAuthCredentialStored` — connector_id, provider, scopes
- `OAuthTokenRefreshed` — connector_id, new expiry
- `OAuthTokenExpired` — connector_id, reason
- `OAuthFlowStarted` — connector_id, provider
- `OAuthFlowCompleted` — connector_id, provider

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-vault/src/oauth.rs` | **New** — `OAuthCredential`, `OAuthPendingFlow`, SQLite schema, vault methods |
| `crates/agentos-vault/src/token_refresh.rs` | **New** — background refresh loop |
| `crates/agentos-vault/src/redactor.rs` | **New** — `ContextRedactor` with token pattern matching |
| `crates/agentos-vault/src/vault.rs` | Add `store_oauth`, `get_oauth`, `delete_oauth`, `list_oauth`, `refresh_oauth`, `store_pending_flow`, `complete_pending_flow`, `sweep_expired_flows` methods |
| `crates/agentos-vault/src/lib.rs` | Re-export new modules |
| `crates/agentos-vault/Cargo.toml` | Add `reqwest`, `regex`, `chrono` deps (if not already present) |
| `crates/agentos-audit/src/lib.rs` | Add OAuth audit event variants |
| `crates/agentos-kernel/src/timeout_checker.rs` | Add `sweep_expired_flows()` call every 10min |

---

## Dependencies

- **Requires:** None (first phase)
- **Blocks:** Phase 2 (Connector Hub), Phase 3 (OAuth Web Flow)

---

## Test Plan

1. **Unit: store and retrieve OAuth credential**
   - Store a credential with `store_oauth()`, retrieve with `get_oauth()`, verify all fields match
   - Verify encrypted_payload in SQLite is not plaintext

2. **Unit: token refresh updates credential**
   - Store a credential, call `refresh_oauth()` with new access_token and expires_at
   - Verify `get_oauth()` returns updated values

3. **Unit: pending flow lifecycle**
   - Store pending flow, complete it by state param, verify it's deleted after completion
   - Verify expired flows are swept

4. **Unit: context redactor**
   - Feed strings containing `Bearer eyJ...`, GitHub PATs, Slack tokens
   - Verify all are replaced with `[REDACTED]`
   - Verify non-sensitive text is untouched

5. **Integration: refresh loop**
   - Use `mockito` or `wiremock` to mock a token endpoint
   - Store a credential expiring in 2 minutes
   - Start the refresh loop with a 1s check interval (test override)
   - Verify the token endpoint is called and the credential is updated

6. **Security: encrypted at rest**
   - Store a credential, read raw SQLite blob, verify it's not plaintext JSON

---

## Verification

```bash
cargo test -p agentos-vault -- --test-threads=1
cargo test -p agentos-audit
cargo clippy -p agentos-vault -p agentos-audit -- -D warnings
cargo fmt -p agentos-vault -p agentos-audit -- --check
```
