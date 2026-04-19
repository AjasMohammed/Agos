---
title: Phase 4 — Gmail API Client & Readonly Tools
tags:
  - gmail
  - tools
  - phase-4
date: 2026-04-18
status: planned
effort: 2d
priority: high
---

# Phase 4 — Gmail API Client & Readonly Tools

> Wire the OAuth-authenticated Gmail client and ship the first set of tools — every tool in this phase is read-only, so we can merge and dogfood before touching anything that mutates state.

---

## Why this phase

Readonly tools are the lowest-risk, highest-value path to a usable MVP. Merged tools: search, read, list labels, list filters, get thread, list messages, get profile, download attachment. That's a useful assistant day one, and it validates the full stack (auth → API client → tool → MCP dispatch) before write tools add blast radius.

---

## Deliverables

- `gmail-mcp-core::gmail::client::Client` wrapping the Gmail REST API.
- 9 readonly tools: `gmail_search`, `gmail_read`, `gmail_get_thread`, `gmail_list_labels`, `gmail_list_filters`, `gmail_get_filter`, `gmail_list_drafts`, `gmail_get_profile`, `gmail_download_attachment`.
- Each tool: JSON schema, typed errors, required scopes, unit cost.
- CLI: `gmail-mcp serve` spawns the full stack with all tools registered.
- Integration tests against a real Gmail test account (gated behind `GMAIL_MCP_INT_TEST=1`).

---

## Gmail client

Decision: hand-rolled thin wrapper over `reqwest`, not `google-gmail1`. Why:

- `google-gmail1` is comprehensive but bulky and pulls in a generated surface we don't need.
- Our tool surface is narrow (~25 endpoints) — a focused client is ~400 lines and easier to audit.
- Fine-grained control over rate limiting, retries, and error mapping.

```rust
// crates/gmail-mcp-core/src/gmail/client.rs
pub struct Client {
    http: reqwest::Client,
    tokens: Arc<TokenManager>,
    limiter: Arc<RateLimiter>,       // from Phase 7
    base: &'static str,              // "https://gmail.googleapis.com/gmail/v1"
}

impl Client {
    pub async fn new(tokens: Arc<TokenManager>, limiter: Arc<RateLimiter>) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("gmail-mcp/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .https_only(true)
            .build()
            .expect("reqwest builder");
        Self { http, tokens, limiter, base: "https://gmail.googleapis.com/gmail/v1" }
    }

    async fn request<Q, B, R>(
        &self,
        method: reqwest::Method,
        path: &str,
        query: Option<&Q>,
        body: Option<&B>,
        cost: u32,
    ) -> Result<R, GmailError>
    where
        Q: serde::Serialize + ?Sized,
        B: serde::Serialize + ?Sized,
        R: for<'de> serde::Deserialize<'de>,
    {
        self.limiter.acquire(cost).await?;
        let token = self.tokens.access_token().await?;
        let url = format!("{}/{}", self.base, path.trim_start_matches('/'));
        let mut req = self.http.request(method, &url).bearer_auth(&token);
        if let Some(q) = query { req = req.query(q); }
        if let Some(b) = body { req = req.json(b); }
        let resp = req.send().await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp.json::<R>().await?);
        }
        // Retry 429 and 5xx at the rate-limiter layer (Phase 7 will thread that in).
        Err(map_gmail_error(status, resp).await)
    }

    pub async fn messages_list(&self, q: &MessagesListQuery)  -> Result<MessagesListResp, GmailError> {
        self.request(Method::GET, "users/me/messages", Some(q), None::<&()>, 5).await
    }
    pub async fn messages_get(&self, id: &str, fmt: MessageFormat) -> Result<Message, GmailError> {
        let q = [("format", fmt.as_str())];
        self.request(Method::GET, &format!("users/me/messages/{id}"), Some(&q), None::<&()>, 5).await
    }
    pub async fn threads_get(&self, id: &str) -> Result<Thread, GmailError> {
        self.request(Method::GET, &format!("users/me/threads/{id}"), None::<&()>, None::<&()>, 10).await
    }
    pub async fn labels_list(&self) -> Result<LabelsListResp, GmailError> {
        self.request(Method::GET, "users/me/labels", None::<&()>, None::<&()>, 1).await
    }
    pub async fn filters_list(&self) -> Result<FiltersListResp, GmailError> {
        self.request(Method::GET, "users/me/settings/filters", None::<&()>, None::<&()>, 1).await
    }
    pub async fn filters_get(&self, id: &str) -> Result<Filter, GmailError> {
        self.request(Method::GET, &format!("users/me/settings/filters/{id}"), None::<&()>, None::<&()>, 1).await
    }
    pub async fn drafts_list(&self) -> Result<DraftsListResp, GmailError> {
        self.request(Method::GET, "users/me/drafts", None::<&()>, None::<&()>, 5).await
    }
    pub async fn profile_get(&self) -> Result<Profile, GmailError> {
        self.request(Method::GET, "users/me/profile", None::<&()>, None::<&()>, 1).await
    }
    pub async fn attachment_get(&self, msg_id: &str, att_id: &str) -> Result<Attachment, GmailError> {
        self.request(
            Method::GET,
            &format!("users/me/messages/{msg_id}/attachments/{att_id}"),
            None::<&()>, None::<&()>, 5).await
    }
}
```

Types (`Message`, `Thread`, `Label`, `Filter`, `Profile`, `Attachment`) live in `crates/gmail-mcp-core/src/gmail/types.rs` — deserialize only the fields we actually surface to tools, drop the rest. That's a deliberate minimization: less data flowing out means fewer accidental leaks.

---

## Error mapping

```rust
pub async fn map_gmail_error(status: StatusCode, resp: reqwest::Response) -> GmailError {
    #[derive(Deserialize)]
    struct GoogleErr { error: GoogleErrInner }
    #[derive(Deserialize)]
    struct GoogleErrInner { code: i32, message: String, errors: Option<Vec<GoogleErrDetail>> }
    #[derive(Deserialize)]
    struct GoogleErrDetail { reason: Option<String>, domain: Option<String> }

    let body: GoogleErr = match resp.json().await {
        Ok(b) => b,
        Err(_) => return GmailError::Transport(format!("http {status}")),
    };
    let reason = body.error.errors.as_ref()
        .and_then(|v| v.first())
        .and_then(|d| d.reason.clone())
        .unwrap_or_default();

    match (status.as_u16(), reason.as_str()) {
        (401, _) => GmailError::AuthExpired,
        (403, "insufficientPermissions") => GmailError::ScopeMissing { reason },
        (403, "dailyLimitExceeded") | (403, "userRateLimitExceeded") => GmailError::QuotaExhausted,
        (404, _) => GmailError::NotFound(body.error.message),
        (429, _) => GmailError::RateLimited,
        (400, _) => GmailError::InvalidRequest(body.error.message),
        (500..=599, _) => GmailError::Transport(body.error.message),
        _ => GmailError::Other(body.error.message),
    }
}
```

---

## Tool implementations

Each tool is a separate file under `crates/gmail-mcp-core/src/tools/`. The shape:

```rust
// crates/gmail-mcp-core/src/tools/search.rs
pub struct GmailSearchTool {
    client: Arc<Client>,
}

#[async_trait]
impl Tool for GmailSearchTool {
    fn name(&self) -> &str { "gmail_search" }
    fn description(&self) -> &str {
        "Search messages in the authenticated mailbox using Gmail query syntax."
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Gmail query, e.g. 'from:boss@corp.com is:unread newer_than:7d'",
                    "maxLength": 2048
                },
                "max_results": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 500,
                    "default": 25
                },
                "page_token": { "type": "string", "maxLength": 4096 }
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }
    async fn call(&self, args: serde_json::Value) -> Result<serde_json::Value, McpError> {
        #[derive(Deserialize)]
        struct Args { query: String, max_results: Option<u32>, page_token: Option<String> }
        let a: Args = serde_json::from_value(args).map_err(|e| McpError::InvalidParams(e.to_string()))?;
        let resp = self.client.messages_list(&MessagesListQuery {
            q: Some(a.query),
            max_results: a.max_results.unwrap_or(25),
            page_token: a.page_token,
            ..Default::default()
        }).await.map_err(ToolError::from)?;
        Ok(serde_json::to_value(resp)?)
    }
}

impl GmailSearchTool {
    pub fn required_scopes() -> &'static [&'static str] {
        &["https://www.googleapis.com/auth/gmail.readonly"]
    }
}
```

Tools to ship in Phase 4 and their unit costs (rough; tweaked with real measurement):

| Tool | Cost | Required scope |
|------|------|----------------|
| `gmail_search` | 5 | `gmail.readonly` |
| `gmail_read` | 5 | `gmail.readonly` |
| `gmail_get_thread` | 10 | `gmail.readonly` |
| `gmail_list_labels` | 1 | `gmail.readonly` |
| `gmail_list_filters` | 1 | `gmail.readonly` |
| `gmail_get_filter` | 1 | `gmail.readonly` |
| `gmail_list_drafts` | 5 | `gmail.readonly` |
| `gmail_get_profile` | 1 | `gmail.readonly` |
| `gmail_download_attachment` | 5 | `gmail.readonly` |

---

## Tool registration wiring

```rust
// crates/gmail-mcp/src/serve.rs
pub async fn run_serve(args: ServeArgs) -> anyhow::Result<()> {
    let store = build_token_store(&args)?;
    let tokens = Arc::new(TokenManager::new(store, args.client_id(), args.account.clone()));
    let limiter = Arc::new(RateLimiter::new(200));   // 200 units/sec (Phase 7 config)
    let gmail = Arc::new(Client::new(tokens.clone(), limiter).await);

    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(GmailSearchTool { client: gmail.clone() }));
    registry.register(Arc::new(GmailReadTool { client: gmail.clone() }));
    registry.register(Arc::new(GmailGetThreadTool { client: gmail.clone() }));
    registry.register(Arc::new(GmailListLabelsTool { client: gmail.clone() }));
    registry.register(Arc::new(GmailListFiltersTool { client: gmail.clone() }));
    registry.register(Arc::new(GmailGetFilterTool { client: gmail.clone() }));
    registry.register(Arc::new(GmailListDraftsTool { client: gmail.clone() }));
    registry.register(Arc::new(GmailGetProfileTool { client: gmail.clone() }));
    registry.register(Arc::new(GmailDownloadAttachmentTool { client: gmail }));
    // Phases 5/6 append here.

    let server = McpServer::new(Arc::new(registry), ServerInfo {
        name: "gmail-mcp", version: env!("CARGO_PKG_VERSION"),
    });
    server.run(StdioTransport::new()).await
}
```

---

## Dependencies added

```toml
# crates/gmail-mcp-core/Cargo.toml
base64 = "0.22"          # attachment body decoding
mime = "0.3"             # content-type handling
# reqwest already added in Phase 3
```

---

## Test plan

Unit tests with mock Gmail API (wiremock crate):

1. `gmail_search` — valid query → list of message refs.
2. `gmail_search` — `query=""` (empty) → schema rejection.
3. `gmail_search` — 401 from API → `McpError::ToolError(AuthExpired)`.
4. `gmail_read` — 404 for unknown id → typed `NotFound`.
5. `gmail_read` — MIME multipart parsing exposes `text/plain` body correctly.
6. `gmail_download_attachment` — base64url body decoded to expected bytes.
7. `gmail_get_profile` — returns `email_address`, `messages_total`, `threads_total`.
8. `gmail_list_filters` — empty list returns `{ filters: [] }` not `null`.

Integration tests (CI-gated, require `GMAIL_MCP_INT_TEST_REFRESH_TOKEN`):

1. Full auth → search → read → matches expected subject.
2. Download attachment from a prepared test email → bytes match fixture.
3. Rate limiter delays the 251st call within a second.

---

## Verification

```bash
cargo test -p gmail-mcp-core gmail::
cargo test -p gmail-mcp-core tools::

# End-to-end smoke (after Phase 3 auth done):
./target/debug/gmail-mcp serve &
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"probe","version":"1"}}}' > /proc/$!/fd/0
echo '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' > /proc/$!/fd/0
echo '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"gmail_get_profile","arguments":{}}}' > /proc/$!/fd/0
# → responses include email address + counts
```

---

## Files changed

| File | Change |
|------|--------|
| `crates/gmail-mcp-core/src/gmail/mod.rs` | New |
| `crates/gmail-mcp-core/src/gmail/client.rs` | New |
| `crates/gmail-mcp-core/src/gmail/types.rs` | New |
| `crates/gmail-mcp-core/src/gmail/errors.rs` | New |
| `crates/gmail-mcp-core/src/tools/mod.rs` | New |
| `crates/gmail-mcp-core/src/tools/{search,read,get_thread,list_labels,list_filters,get_filter,list_drafts,get_profile,download_attachment}.rs` | New (9 files) |
| `crates/gmail-mcp/src/serve.rs` | New CLI serve wiring |
| `crates/gmail-mcp/src/main.rs` | Route `serve` subcommand |
| `crates/gmail-mcp-core/Cargo.toml` | Additional deps |

Count: 15 files. Acceptable since all are small and focused.

---

## Dependencies

- **Requires:** Phase 2 (MCP core), Phase 3 (auth).
- **Blocks:** Phase 5, Phase 6, Phase 7.

---

## Related

- [[Gmail MCP Server Plan]]
- [[05-write-tools-send-draft-modify]]
- [[07-production-hardening]]
