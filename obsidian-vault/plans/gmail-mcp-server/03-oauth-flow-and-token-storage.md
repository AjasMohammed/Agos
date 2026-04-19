---
title: Phase 3 — OAuth Flow & Keychain-Backed Token Storage
tags:
  - oauth
  - security
  - keychain
  - phase-3
date: 2026-04-18
status: planned
effort: 2d
priority: high
---

# Phase 3 — OAuth Flow & Keychain-Backed Token Storage

> Implement the desktop loopback OAuth flow, device code flow, and service account flow. Store tokens in the OS keychain with an encrypted file fallback. Zeroize secrets in memory. No plaintext JSON.

---

## Why this phase

This is the security lynchpin. A single mistake here — storing a refresh token in the wrong place or failing to zeroize a buffer — undoes every other security control.

---

## Deliverables

- `gmail_mcp_core::auth` module with:
  - `OAuthClient` supporting loopback, device, and service-account flows.
  - `TokenStore` trait with `KeychainStore` + `EncryptedFileStore` impls.
  - `TokenManager` — thin front for auth tools, pre-emptive refresh, zeroization.
- `gmail-mcp auth` subcommand with flags: `--scopes read|write|full`, `--device`, `--service-account`, `--account <name>`, `--client-id <id>`.
- `gmail-mcp logout --account <name>` — wipes tokens, emits audit event.
- `gmail-mcp accounts list` — shows which accounts have tokens (names only, never the secret).

---

## OAuth flows

### Desktop loopback (default)

```rust
// crates/gmail-mcp-core/src/auth/oauth/loopback.rs
pub struct LoopbackFlow {
    client_id: String,
    client_secret: Option<String>,   // None for public client
    scopes: Vec<String>,
}

impl LoopbackFlow {
    pub async fn run(&self) -> Result<TokenSet, AuthError> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let redirect = format!("http://127.0.0.1:{port}/");

        let pkce = PkcePair::generate();       // verifier + S256 challenge
        let state = random_state();            // 32 bytes, base64url

        let auth_url = build_auth_url(
            "https://accounts.google.com/o/oauth2/v2/auth",
            &self.client_id,
            &redirect,
            &self.scopes,
            &pkce.challenge,
            &state,
        );

        // Open browser (best-effort — fall back to prompting user).
        let _ = webbrowser::open(&auth_url);
        println!("If the browser didn't open, visit:\n  {auth_url}");

        // Wait for callback with 5-minute timeout.
        let (code, returned_state) = tokio::time::timeout(
            Duration::from_secs(300),
            wait_for_callback(listener),
        ).await.map_err(|_| AuthError::Timeout)??;

        if returned_state != state {
            return Err(AuthError::StateMismatch);
        }

        // Exchange code for tokens.
        let resp: GoogleTokenResponse = reqwest::Client::new()
            .post("https://oauth2.googleapis.com/token")
            .form(&[
                ("code", code.as_str()),
                ("client_id", &self.client_id),
                ("redirect_uri", &redirect),
                ("grant_type", "authorization_code"),
                ("code_verifier", &pkce.verifier),
            ])
            .send().await?
            .error_for_status()?
            .json().await?;

        Ok(TokenSet::from(resp))
    }
}

async fn wait_for_callback(listener: TcpListener) -> Result<(String, String), AuthError> {
    // Use a tiny hand-rolled HTTP parser — don't pull in axum for a single request.
    let (mut stream, _) = listener.accept().await?;
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf).await?;
    let request = std::str::from_utf8(&buf[..n]).map_err(|_| AuthError::MalformedCallback)?;

    // GET /?code=...&state=... HTTP/1.1
    let path = request.lines().next()
        .and_then(|l| l.split_whitespace().nth(1))
        .ok_or(AuthError::MalformedCallback)?;

    let url = url::Url::parse(&format!("http://127.0.0.1{path}"))?;
    let code = url.query_pairs().find(|(k, _)| k == "code").map(|(_, v)| v.to_string());
    let state = url.query_pairs().find(|(k, _)| k == "state").map(|(_, v)| v.to_string());

    let body = "Authentication complete. You can close this tab.";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
        body.len(), body
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;

    match (code, state) {
        (Some(c), Some(s)) => Ok((c, s)),
        _ => Err(AuthError::MalformedCallback),
    }
}
```

### Device code flow

```rust
// crates/gmail-mcp-core/src/auth/oauth/device.rs
pub struct DeviceFlow {
    client_id: String,
    scopes: Vec<String>,
}

impl DeviceFlow {
    pub async fn run(&self) -> Result<TokenSet, AuthError> {
        let http = reqwest::Client::new();
        let init: DeviceCodeResponse = http.post("https://oauth2.googleapis.com/device/code")
            .form(&[("client_id", self.client_id.as_str()),
                    ("scope", &self.scopes.join(" "))])
            .send().await?.error_for_status()?.json().await?;

        eprintln!("Visit {} and enter code: {}", init.verification_url, init.user_code);

        let deadline = Instant::now() + Duration::from_secs(init.expires_in);
        let mut interval = init.interval.max(5);

        while Instant::now() < deadline {
            tokio::time::sleep(Duration::from_secs(interval)).await;
            let poll = http.post("https://oauth2.googleapis.com/token")
                .form(&[("client_id", self.client_id.as_str()),
                        ("device_code", &init.device_code),
                        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code")])
                .send().await?;
            if poll.status().is_success() {
                return Ok(TokenSet::from(poll.json::<GoogleTokenResponse>().await?));
            }
            let err: GoogleOAuthError = poll.json().await?;
            match err.error.as_str() {
                "authorization_pending" => continue,
                "slow_down" => { interval += 5; continue; }
                "access_denied" => return Err(AuthError::UserDenied),
                "expired_token" => return Err(AuthError::Timeout),
                other => return Err(AuthError::Provider(other.into())),
            }
        }
        Err(AuthError::Timeout)
    }
}
```

### Service account flow

```rust
// crates/gmail-mcp-core/src/auth/oauth/service_account.rs
pub struct ServiceAccountFlow {
    key_json: ServiceAccountKey,     // email, private_key (PEM)
    impersonate: String,              // subject email (domain-wide delegation)
    scopes: Vec<String>,
}

impl ServiceAccountFlow {
    pub async fn access_token(&self) -> Result<TokenSet, AuthError> {
        let now = chrono::Utc::now().timestamp();
        let claims = jwt::Claims {
            iss: self.key_json.client_email.clone(),
            sub: self.impersonate.clone(),
            aud: "https://oauth2.googleapis.com/token".into(),
            scope: self.scopes.join(" "),
            iat: now,
            exp: now + 3600,
        };
        let header = jwt::Header::new(jwt::Algorithm::RS256);
        let key = jwt::EncodingKey::from_rsa_pem(self.key_json.private_key.as_bytes())?;
        let assertion = jwt::encode(&header, &claims, &key)?;

        let resp: GoogleTokenResponse = reqwest::Client::new()
            .post("https://oauth2.googleapis.com/token")
            .form(&[("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                    ("assertion", &assertion)])
            .send().await?.error_for_status()?.json().await?;

        // Note: service account responses have no refresh_token — we re-mint JWTs.
        Ok(TokenSet::from(resp))
    }
}
```

---

## TokenStore trait

```rust
// crates/gmail-mcp-core/src/auth/store.rs
#[async_trait::async_trait]
pub trait TokenStore: Send + Sync {
    async fn put(&self, account: &str, tokens: &TokenSet) -> Result<(), AuthError>;
    async fn get(&self, account: &str) -> Result<Option<TokenSet>, AuthError>;
    async fn delete(&self, account: &str) -> Result<(), AuthError>;
    async fn list_accounts(&self) -> Result<Vec<String>, AuthError>;
}
```

### KeychainStore (default)

```rust
pub struct KeychainStore {
    service: &'static str,    // "gmail-mcp"
}

#[async_trait::async_trait]
impl TokenStore for KeychainStore {
    async fn put(&self, account: &str, tokens: &TokenSet) -> Result<(), AuthError> {
        let bytes = serde_json::to_vec(tokens)?;
        let entry = keyring::Entry::new(self.service, account)?;
        tokio::task::spawn_blocking(move || entry.set_secret(&bytes)).await??;
        bytes.zeroize();   // best-effort; serde already allocated
        Ok(())
    }

    async fn get(&self, account: &str) -> Result<Option<TokenSet>, AuthError> {
        let entry = keyring::Entry::new(self.service, account)?;
        let bytes_res = tokio::task::spawn_blocking(move || entry.get_secret()).await?;
        match bytes_res {
            Ok(bytes) => {
                let tokens: TokenSet = serde_json::from_slice(&bytes)?;
                Ok(Some(tokens))
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn delete(&self, account: &str) -> Result<(), AuthError> {
        let entry = keyring::Entry::new(self.service, account)?;
        tokio::task::spawn_blocking(move || entry.delete_credential()).await??;
        Ok(())
    }

    async fn list_accounts(&self) -> Result<Vec<String>, AuthError> {
        // `keyring` doesn't provide enumeration. Maintain a side index file
        // at $XDG_CONFIG_HOME/gmail-mcp/accounts.list with just account names.
        read_account_index().await
    }
}
```

### EncryptedFileStore (fallback)

```rust
// crates/gmail-mcp-core/src/auth/store/encrypted_file.rs
pub struct EncryptedFileStore {
    path: PathBuf,
    passphrase: Zeroizing<String>,
}

impl EncryptedFileStore {
    pub async fn put(&self, account: &str, tokens: &TokenSet) -> Result<(), AuthError> {
        let mut map = self.load().await.unwrap_or_default();
        map.insert(account.to_string(), tokens.clone());
        let plaintext = serde_json::to_vec(&map)?;
        let sealed = seal(&plaintext, self.passphrase.as_bytes())?;  // Argon2id + AES-GCM
        tokio::fs::write(&self.path, sealed).await?;
        plaintext.zeroize();
        Ok(())
    }

    async fn load(&self) -> Result<HashMap<String, TokenSet>, AuthError> {
        let sealed = match tokio::fs::read(&self.path).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
            Err(e) => return Err(e.into()),
        };
        let plaintext = open(&sealed, self.passphrase.as_bytes())?;
        let map: HashMap<String, TokenSet> = serde_json::from_slice(&plaintext)?;
        Ok(map)
    }
    // delete, list_accounts, get — all operate on the decrypted map then re-seal
}

fn seal(plaintext: &[u8], passphrase: &[u8]) -> Result<Vec<u8>, AuthError> {
    let salt = rand_bytes(16);
    let mut key = Zeroizing::new([0u8; 32]);
    Argon2::default().hash_password_into(passphrase, &salt, key.as_mut())?;
    let cipher = aes_gcm::Aes256Gcm::new_from_slice(&*key)?;
    let nonce = rand_bytes(12);
    let ct = cipher.encrypt(nonce.as_slice().into(), plaintext)?;
    // Wire format: [salt(16)][nonce(12)][ct]
    Ok([salt, nonce, ct].concat())
}
```

---

## TokenSet with zeroization

```rust
#[derive(Clone, serde::Serialize, serde::Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: i64,         // unix seconds
    pub scopes: Vec<String>,
    pub account_email: String,
}
```

---

## TokenManager

```rust
// crates/gmail-mcp-core/src/auth/manager.rs
pub struct TokenManager {
    store: Arc<dyn TokenStore>,
    client_id: String,
    account: String,
    cached: tokio::sync::RwLock<Option<TokenSet>>,
}

impl TokenManager {
    pub async fn access_token(&self) -> Result<String, AuthError> {
        {
            let guard = self.cached.read().await;
            if let Some(t) = guard.as_ref() {
                if !self.is_near_expiry(t) {
                    return Ok(t.access_token.clone());
                }
            }
        }
        self.refresh().await
    }

    fn is_near_expiry(&self, t: &TokenSet) -> bool {
        chrono::Utc::now().timestamp() + 60 >= t.expires_at   // 60s headroom
    }

    async fn refresh(&self) -> Result<String, AuthError> {
        let mut guard = self.cached.write().await;
        // Reload from store in case another instance refreshed since we checked.
        let current = self.store.get(&self.account).await?
            .ok_or(AuthError::NoCredentials)?;

        if !self.is_near_expiry(&current) {
            *guard = Some(current.clone());
            return Ok(current.access_token);
        }

        let refresh_token = current.refresh_token.as_ref()
            .ok_or(AuthError::NoRefreshToken)?;

        let resp: GoogleTokenResponse = reqwest::Client::new()
            .post("https://oauth2.googleapis.com/token")
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("refresh_token", refresh_token),
                ("grant_type", "refresh_token"),
            ])
            .send().await?
            .error_for_status()
            .map_err(|e| {
                if e.status() == Some(StatusCode::BAD_REQUEST) {
                    AuthError::Revoked
                } else { e.into() }
            })?
            .json().await?;

        let updated = TokenSet {
            access_token: resp.access_token,
            refresh_token: resp.refresh_token.or(current.refresh_token.clone()),
            expires_at: chrono::Utc::now().timestamp() + resp.expires_in,
            scopes: current.scopes.clone(),
            account_email: current.account_email.clone(),
        };
        self.store.put(&self.account, &updated).await?;
        let token = updated.access_token.clone();
        *guard = Some(updated);
        Ok(token)
    }
}
```

---

## CLI — `gmail-mcp auth`

```rust
#[derive(clap::Parser)]
struct AuthArgs {
    /// Scope preset: read, write, full.
    #[arg(long, default_value = "read")]
    scopes: String,

    /// Use device code flow (headless).
    #[arg(long)]
    device: bool,

    /// Use service account JSON file.
    #[arg(long, value_name = "PATH")]
    service_account: Option<PathBuf>,

    /// Account name for multi-account setups.
    #[arg(long, default_value = "default")]
    account: String,

    /// Override the OAuth client ID (enterprises BYO).
    #[arg(long, env = "GMAIL_MCP_CLIENT_ID")]
    client_id: Option<String>,

    /// Use encrypted file store instead of OS keychain.
    /// Requires --passphrase or prompts interactively.
    #[arg(long)]
    file_store: bool,
}
```

---

## Scope presets

| Preset | Google scopes |
|--------|---------------|
| `read` | `https://www.googleapis.com/auth/gmail.readonly`, `https://www.googleapis.com/auth/userinfo.email` |
| `write` | `read` plus `https://www.googleapis.com/auth/gmail.modify` |
| `full` | `write` plus `https://www.googleapis.com/auth/gmail.settings.basic` |

At tool-call time, tools declare `required_scopes()`. The server compares against the granted scopes in the active `TokenSet`; missing scopes produce a typed `ScopeMissing` error.

---

## Dependencies added

```toml
# crates/gmail-mcp-core/Cargo.toml
keyring = "3"
rand = "0.8"
url = "2"
webbrowser = "1"
reqwest = { version = "0.12", features = ["json", "rustls-tls"], default-features = false }
chrono = { version = "0.4", default-features = false, features = ["clock", "serde"] }
zeroize = { version = "1", features = ["derive"] }
aes-gcm = "0.10"
argon2 = "0.5"
jsonwebtoken = "9"   # for service account JWT signing
```

---

## Test plan

1. Loopback flow with mock OAuth server → token roundtrip + state validation.
2. Device flow with mock OAuth server → pending/expired/approved branches.
3. Service account JWT → token obtained and exposed via `access_token()`.
4. State mismatch in loopback callback → `AuthError::StateMismatch`.
5. Callback timeout (no browser callback in 5 min) → `AuthError::Timeout`.
6. `KeychainStore` round-trip on CI matrix (Linux, macOS, Windows). On headless Linux, expect fallback to file store when Secret Service missing.
7. `EncryptedFileStore`: put/get/delete round-trip with correct passphrase; wrong passphrase → `AuthError::Decrypt`.
8. `TokenManager` refresh: token near expiry → calls refresh endpoint; on success updates cache and store.
9. `TokenManager` refresh: 400 invalid_grant → `AuthError::Revoked`; cache cleared.
10. Concurrent access: two `access_token()` calls race — only one refresh hits the endpoint.
11. `Zeroize` confirmed with `miri` / sanitizer run — no plaintext survives `drop`.

---

## Verification

```bash
cargo test -p gmail-mcp-core auth::

# Manual smoke:
./target/debug/gmail-mcp auth --scopes read
# → browser pops up, user approves, prints "✓ Authorized"
./target/debug/gmail-mcp accounts list
# → "default (awin.neumeral@gmail.com, scopes: read)"
secret-tool lookup service gmail-mcp account default
# → binary blob (encrypted/opaque; Linux libsecret)
```

---

## Files changed

| File | Change |
|------|--------|
| `crates/gmail-mcp-core/src/auth/mod.rs` | New |
| `crates/gmail-mcp-core/src/auth/oauth/loopback.rs` | New |
| `crates/gmail-mcp-core/src/auth/oauth/device.rs` | New |
| `crates/gmail-mcp-core/src/auth/oauth/service_account.rs` | New |
| `crates/gmail-mcp-core/src/auth/store/keychain.rs` | New |
| `crates/gmail-mcp-core/src/auth/store/encrypted_file.rs` | New |
| `crates/gmail-mcp-core/src/auth/manager.rs` | New |
| `crates/gmail-mcp-core/src/auth/errors.rs` | New |
| `crates/gmail-mcp/src/cli/auth.rs` | New CLI subcommand |
| `crates/gmail-mcp/src/main.rs` | Route `auth` / `accounts` / `logout` subcommands |
| `crates/gmail-mcp-core/Cargo.toml` | Dependencies |

---

## Dependencies

- **Requires:** Phase 1.
- **Blocks:** Phase 4 (Gmail client reads tokens via `TokenManager`), Phase 7 (audit logs auth events), Phase 8 (HTTP transport needs multi-account).

---

## Related

- [[Gmail MCP Server Plan]]
- [[Gmail MCP Server Data Flow]]
- [[04-gmail-client-and-readonly-tools]]
- [[07-production-hardening]]
