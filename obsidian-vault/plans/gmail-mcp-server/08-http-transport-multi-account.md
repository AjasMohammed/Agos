---
title: Phase 8 — HTTP/SSE Transport & Multi-Account
tags:
  - mcp
  - http
  - sse
  - multi-account
  - phase-8
  - stretch
date: 2026-04-18
status: planned
effort: 2d
priority: low
---

# Phase 8 — HTTP/SSE Transport & Multi-Account (stretch)

> Optional transport layer that lets a single `gmail-mcp` instance serve many MCP clients over HTTP/SSE, and lets each client pick which Gmail account to operate against. Skip this for v1 if time is tight.

---

## Why this phase

Stdio is the default and always works. But it ties one MCP server process to one host. For:

- Hosted / remote deployments (team of agents sharing a Gmail helper)
- Multi-account scenarios (one server, N accounts, client picks at call time)
- Dev environments where a long-lived daemon is easier than per-session stdio

…HTTP/SSE is the right transport. MCP spec defines it; we implement it.

---

## Deliverables

- `gmail-mcp serve --transport http --listen 127.0.0.1:8443` — HTTP/SSE server per MCP spec.
- Authentication: bearer token, issued via `gmail-mcp issue-token --account default`.
- Multi-account: `--account default` on stdio, `X-MCP-Account` header on HTTP.
- TLS via rustls (require via `--tls-cert` / `--tls-key` or run behind a reverse proxy).

---

## MCP HTTP/SSE protocol

Per spec:
- Client opens SSE stream: `GET /sse`, server streams events.
- Client sends requests: `POST /message` with JSON body.
- Request/response correlation via request id.
- Server notifications pushed as SSE events.

```rust
// crates/gmail-mcp-core/src/mcp/transport/http.rs
use axum::{Router, routing::{get, post}, extract::State, response::sse::{Event, KeepAlive, Sse}};

pub fn router(state: Arc<HttpState>) -> Router {
    Router::new()
        .route("/sse", get(handle_sse))
        .route("/message", post(handle_message))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(state)
}

async fn handle_sse(State(state): State<Arc<HttpState>>, auth: BearerAuth) -> impl IntoResponse {
    let session = state.sessions.create(auth.account.clone()).await;
    let rx = session.event_rx;
    Sse::new(tokio_stream::wrappers::ReceiverStream::new(rx)
        .map(|resp| Ok(Event::default().data(serde_json::to_string(&resp).unwrap_or_default()))))
    .keep_alive(KeepAlive::default())
}

async fn handle_message(
    State(state): State<Arc<HttpState>>,
    auth: BearerAuth,
    Json(req): Json<JsonRpcRequest>,
) -> impl IntoResponse {
    let session = state.sessions.get(&auth.session_id).ok_or(StatusCode::UNAUTHORIZED)?;
    let resp = state.server.dispatch_for(session.account.clone(), req).await;
    match resp {
        Some(r) => (StatusCode::OK, Json(r)).into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
    }
}
```

`McpServer::dispatch_for` is a generalization of `dispatch` that takes an explicit account name, so we can route a single request to the right per-account Gmail client.

---

## Multi-account manager

```rust
pub struct AccountManager {
    store: Arc<dyn TokenStore>,
    clients: RwLock<HashMap<String, Arc<Client>>>,
}

impl AccountManager {
    pub async fn client(&self, account: &str) -> Result<Arc<Client>, AuthError> {
        {
            if let Some(c) = self.clients.read().await.get(account) { return Ok(c.clone()); }
        }
        let tokens = TokenManager::new(self.store.clone(), self.client_id.clone(), account.into());
        let limiter = Arc::new(RateLimiter::new(200));
        let client = Arc::new(Client::new(Arc::new(tokens), limiter).await);
        self.clients.write().await.insert(account.into(), client.clone());
        Ok(client)
    }
}
```

Each tool holds `Arc<AccountManager>` instead of `Arc<Client>` and resolves the right client at call time.

---

## Auth tokens for HTTP

Each MCP client needs to authenticate with the HTTP server (different from the *Gmail* OAuth credentials).

```bash
gmail-mcp issue-token --account default --valid-for 24h --description "Cursor on my laptop"
# → prints: gmt_abc123…def456
```

Tokens are:
- 32-byte random, base64url, prefix `gmt_`.
- Stored **hashed** (SHA-256) in `~/.config/gmail-mcp/http-tokens.json` with metadata (account, created, expires, description).
- Validated per request; revoke with `gmail-mcp revoke-token <id>`.
- Each token bound to one account — the client cannot use it for another account.

Tokens are the auth primitive because:
- Simpler than client certs.
- Rotatable.
- Scopable (one token = one account).
- Easy to audit.

---

## TLS

Run behind a reverse proxy (nginx / Caddy / Cloudflare Tunnel) for most deployments. For direct-facing use, accept `--tls-cert` / `--tls-key` with `rustls`. Default: refuse to listen on non-loopback without TLS. `--listen 127.0.0.1:8443` works plaintext because it's loopback; `--listen 0.0.0.0:8443` without TLS flags is a startup error.

---

## `gmail-mcp issue-token` CLI

```rust
#[derive(clap::Parser)]
struct IssueTokenArgs {
    #[arg(long, default_value = "default")]
    account: String,
    #[arg(long, default_value = "24h", value_parser = humantime::parse_duration)]
    valid_for: Duration,
    #[arg(long)]
    description: Option<String>,
}
```

Output: the generated token printed once, with a warning that it's shown only here.

---

## Test plan

1. Issue token → hash stored, plaintext printed once; token not recoverable after.
2. `GET /sse` with valid `Authorization: Bearer gmt_…` → opens SSE stream.
3. `GET /sse` with invalid token → `401 Unauthorized`.
4. `POST /message` with valid token → routed to the right account's tools.
5. Request with `X-MCP-Account: personal` on a token scoped to `work` → `403 Forbidden`.
6. Bind to `0.0.0.0:8443` without TLS → startup error.
7. Token expiry → `401` after `valid_for`.
8. `revoke-token` → subsequent requests with that token → `401`.
9. Two accounts sharing a single server process: tool call result for `personal` reflects personal's Gmail, not work's.

---

## Verification

```bash
cargo test -p gmail-mcp-core transport::http
cargo test -p gmail-mcp-core account_manager

# Manual:
./target/debug/gmail-mcp auth --account personal --scopes read
./target/debug/gmail-mcp auth --account work --scopes full
TOKEN=$(./target/debug/gmail-mcp issue-token --account personal --valid-for 1h)
./target/debug/gmail-mcp serve --transport http --listen 127.0.0.1:8443 &

# Cursor/AgentOS connects via HTTP with bearer token.
curl -N -H "Authorization: Bearer $TOKEN" http://127.0.0.1:8443/sse   # keeps connection open
```

---

## Files changed

| File | Change |
|------|--------|
| `crates/gmail-mcp-core/src/mcp/transport/mod.rs` | Add `Transport` abstraction |
| `crates/gmail-mcp-core/src/mcp/transport/http.rs` | New (axum + SSE) |
| `crates/gmail-mcp-core/src/mcp/server.rs` | `dispatch_for(account, req)` |
| `crates/gmail-mcp-core/src/auth/account_manager.rs` | New |
| `crates/gmail-mcp-core/src/auth/http_tokens.rs` | Token issue/validate/revoke |
| `crates/gmail-mcp/src/cli/issue_token.rs` | New |
| `crates/gmail-mcp/src/cli/revoke_token.rs` | New |
| `crates/gmail-mcp/src/serve.rs` | HTTP transport branch |
| `crates/gmail-mcp-core/Cargo.toml` | `axum`, `tokio-stream`, `rustls`, `humantime` |

---

## Dependencies

- **Requires:** Phases 2 (MCP core), 3 (auth), 7 (audit — HTTP events need to be audited).
- **Blocks:** Nothing critical — stretch.

---

## Related

- [[Gmail MCP Server Plan]]
- [[02-mcp-protocol-and-stdio]]
- [[03-oauth-flow-and-token-storage]]
