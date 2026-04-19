---
title: Phase 5 — Write Tools (Send / Draft / Modify / Delete)
tags:
  - gmail
  - tools
  - write
  - phase-5
date: 2026-04-18
status: planned
effort: 1.5d
priority: high
---

# Phase 5 — Write Tools

> Add the tools that mutate state: compose+send, create/update drafts, apply labels, move to trash, delete, mark read/unread. Each requires an elevated scope and is subject to host-side approval.

---

## Why this phase

Read-only is half the product. Agents need to be able to draft, send, and triage mail — but these are also the tools where mistakes have blast radius. This phase gets the write surface right: typed errors, scope enforcement, MIME correctness, and clear audit trails.

---

## Deliverables

- 8 write tools: `gmail_send`, `gmail_create_draft`, `gmail_update_draft`, `gmail_send_draft`, `gmail_delete_draft`, `gmail_modify_labels`, `gmail_trash`, `gmail_untrash`.
- MIME assembly: multipart with attachments, inline images, thread replies.
- Pre-flight scope check at call time.
- Typed errors when send is blocked by DMARC, size, or recipient limits.
- Integration tests against a Gmail test account.

---

## MIME assembly

`gmail_send` and `gmail_create_draft` both need RFC 5322 messages. Use `mail-builder` crate — covers MIME multipart correctly and handles RFC 2047 encoded-word headers.

```rust
// crates/gmail-mcp-core/src/gmail/mime.rs
pub struct Compose {
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    pub attachments: Vec<ComposeAttachment>,
    pub thread_id: Option<String>,          // for replies
    pub in_reply_to: Option<String>,        // RFC 5322 Message-ID
    pub references: Vec<String>,
}

pub struct ComposeAttachment {
    pub filename: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
    pub content_id: Option<String>,          // for inline images (cid:…)
}

pub fn render(msg: &Compose) -> Result<Vec<u8>, GmailError> {
    let mut builder = mail_builder::MessageBuilder::new()
        .to(msg.to.iter().map(|s| s.as_str()).collect::<Vec<_>>())
        .subject(&msg.subject);

    if !msg.cc.is_empty() { builder = builder.cc(msg.cc.iter().map(|s| s.as_str()).collect::<Vec<_>>()); }
    if !msg.bcc.is_empty() { builder = builder.bcc(msg.bcc.iter().map(|s| s.as_str()).collect::<Vec<_>>()); }
    if let Some(r) = &msg.in_reply_to { builder = builder.in_reply_to(r.as_str()); }
    if !msg.references.is_empty() { builder = builder.references(msg.references.iter().map(|s| s.as_str()).collect::<Vec<_>>()); }

    if let Some(t) = &msg.body_text { builder = builder.text_body(t); }
    if let Some(h) = &msg.body_html { builder = builder.html_body(h); }

    for a in &msg.attachments {
        builder = builder.attachment(&a.content_type, &a.filename, a.bytes.clone());
    }

    let mut buf = Vec::new();
    builder.write_to(&mut buf)?;
    Ok(buf)
}
```

Gmail send endpoint expects `raw` as base64url-encoded RFC 5322. Wrap `render` output accordingly:

```rust
pub fn to_gmail_raw(bytes: &[u8]) -> String {
    base64::prelude::BASE64_URL_SAFE_NO_PAD.encode(bytes)
}
```

---

## Client additions

```rust
impl Client {
    pub async fn messages_send(&self, raw: &str, thread_id: Option<&str>) -> Result<MessageRef, GmailError> {
        #[derive(Serialize)]
        struct Body<'a> { raw: &'a str, #[serde(skip_serializing_if = "Option::is_none")] thread_id: Option<&'a str> }
        self.request(Method::POST, "users/me/messages/send", None::<&()>, Some(&Body { raw, thread_id }), 100).await
    }
    pub async fn drafts_create(&self, raw: &str) -> Result<Draft, GmailError> {
        #[derive(Serialize)]
        struct Body<'a> { message: Message<'a> }
        #[derive(Serialize)]
        struct Message<'a> { raw: &'a str }
        self.request(Method::POST, "users/me/drafts", None::<&()>, Some(&Body { message: Message { raw } }), 10).await
    }
    pub async fn drafts_update(&self, id: &str, raw: &str) -> Result<Draft, GmailError> {
        #[derive(Serialize)]
        struct Body<'a> { message: Message<'a> }
        #[derive(Serialize)]
        struct Message<'a> { raw: &'a str }
        self.request(Method::PUT, &format!("users/me/drafts/{id}"), None::<&()>, Some(&Body { message: Message { raw } }), 10).await
    }
    pub async fn drafts_send(&self, id: &str) -> Result<MessageRef, GmailError> {
        #[derive(Serialize)]
        struct Body<'a> { id: &'a str }
        self.request(Method::POST, "users/me/drafts/send", None::<&()>, Some(&Body { id }), 100).await
    }
    pub async fn drafts_delete(&self, id: &str) -> Result<(), GmailError> {
        self.request_empty(Method::DELETE, &format!("users/me/drafts/{id}"), 10).await
    }
    pub async fn messages_modify(&self, id: &str, add: &[String], remove: &[String]) -> Result<Message, GmailError> {
        #[derive(Serialize)]
        struct Body<'a> { #[serde(rename = "addLabelIds")] add: &'a [String], #[serde(rename = "removeLabelIds")] remove: &'a [String] }
        self.request(Method::POST, &format!("users/me/messages/{id}/modify"), None::<&()>, Some(&Body { add, remove }), 5).await
    }
    pub async fn messages_trash(&self, id: &str) -> Result<Message, GmailError> {
        self.request(Method::POST, &format!("users/me/messages/{id}/trash"), None::<&()>, None::<&()>, 5).await
    }
    pub async fn messages_untrash(&self, id: &str) -> Result<Message, GmailError> {
        self.request(Method::POST, &format!("users/me/messages/{id}/untrash"), None::<&()>, None::<&()>, 5).await
    }
}
```

---

## `gmail_send` tool

```rust
pub struct GmailSendTool { client: Arc<Client> }

#[async_trait]
impl Tool for GmailSendTool {
    fn name(&self) -> &str { "gmail_send" }
    fn description(&self) -> &str { "Compose and send an email. Requires gmail.modify or gmail.send scope." }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "to": { "type": "array", "items": { "type": "string", "format": "email" }, "minItems": 1, "maxItems": 100 },
                "cc": { "type": "array", "items": { "type": "string", "format": "email" }, "maxItems": 100 },
                "bcc": { "type": "array", "items": { "type": "string", "format": "email" }, "maxItems": 100 },
                "subject": { "type": "string", "maxLength": 998 },   // RFC 5322 line limit
                "body_text": { "type": "string", "maxLength": 2000000 },
                "body_html": { "type": "string", "maxLength": 2000000 },
                "attachments": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "filename": { "type": "string", "maxLength": 255 },
                            "content_type": { "type": "string", "maxLength": 128 },
                            "content_base64": { "type": "string" },
                            "content_id": { "type": "string", "maxLength": 128 }
                        },
                        "required": ["filename", "content_type", "content_base64"],
                        "additionalProperties": false
                    },
                    "maxItems": 20
                },
                "reply_to_message_id": { "type": "string", "maxLength": 128 }
            },
            "required": ["to", "subject"],
            "oneOf": [
                { "required": ["body_text"] },
                { "required": ["body_html"] }
            ],
            "additionalProperties": false
        })
    }

    async fn call(&self, args: serde_json::Value) -> Result<serde_json::Value, McpError> {
        let a: SendArgs = serde_json::from_value(args).map_err(|e| McpError::InvalidParams(e.to_string()))?;

        // If replying, fetch thread + original message to populate In-Reply-To / References.
        let (thread_id, in_reply_to, references) = match a.reply_to_message_id.as_deref() {
            Some(id) => {
                let orig = self.client.messages_get(id, MessageFormat::Metadata).await.map_err(ToolError::from)?;
                let msgid = orig.headers.get("Message-ID").cloned();
                let refs: Vec<String> = orig.headers.get_all("References").cloned().collect();
                (Some(orig.thread_id), msgid, refs)
            }
            None => (None, None, vec![]),
        };

        let mut atts = Vec::new();
        for a in a.attachments.unwrap_or_default() {
            atts.push(ComposeAttachment {
                filename: a.filename,
                content_type: a.content_type,
                bytes: base64::decode(&a.content_base64).map_err(|_| McpError::InvalidParams("invalid base64".into()))?,
                content_id: a.content_id,
            });
        }

        let total = atts.iter().map(|a| a.bytes.len()).sum::<usize>();
        if total > 25 * 1024 * 1024 {
            return Err(ToolError::AttachmentTooLarge { size: total as u64, limit: 25 * 1024 * 1024 }.into());
        }

        let raw = to_gmail_raw(&render(&Compose {
            to: a.to, cc: a.cc.unwrap_or_default(), bcc: a.bcc.unwrap_or_default(),
            subject: a.subject,
            body_text: a.body_text, body_html: a.body_html,
            attachments: atts,
            thread_id: thread_id.clone(),
            in_reply_to, references,
        })?);

        let sent = self.client.messages_send(&raw, thread_id.as_deref()).await.map_err(ToolError::from)?;
        Ok(serde_json::to_value(sent)?)
    }
}
```

Scope check happens in a shared middleware layer — every write tool declares:

```rust
impl GmailSendTool {
    pub fn required_scopes() -> &'static [&'static str] {
        &["https://www.googleapis.com/auth/gmail.send"]
    }
}
```

The MCP server wraps tool dispatch with a pre-flight scope probe:

```rust
// in McpServer::on_call_tool (Phase 2 additions)
if let Some(required) = tool_required_scopes(&tool) {
    let granted = self.tokens.granted_scopes().await?;
    let missing: Vec<_> = required.iter()
        .filter(|r| !granted.iter().any(|g| g == *r))
        .collect();
    if !missing.is_empty() {
        return Err(McpError::ToolError(
            ToolError::ScopeMissing { required: missing.into_iter().cloned().collect() }.into()
        ));
    }
}
```

---

## Remaining write tools

- **`gmail_create_draft`** — same schema as `gmail_send` minus `reply_to_message_id` oddities; calls `drafts_create`.
- **`gmail_update_draft`** — takes `draft_id` + compose fields.
- **`gmail_send_draft`** — takes `draft_id`, calls `drafts_send`.
- **`gmail_delete_draft`** — takes `draft_id`, calls `drafts_delete`.
- **`gmail_modify_labels`** — takes `message_id`, `add_label_ids[]`, `remove_label_ids[]`. Used for archive (remove `INBOX`), mark-read (remove `UNREAD`), star (add `STARRED`).
- **`gmail_trash`** — takes `message_id`, calls `messages_trash`.
- **`gmail_untrash`** — takes `message_id`, calls `messages_untrash`.

Each follows the same structure as `gmail_send`: schema → deserialize → client call → map errors.

---

## Scope & cost summary

| Tool | Cost | Scope |
|------|------|-------|
| `gmail_send` | 100 | `gmail.send` |
| `gmail_create_draft` | 10 | `gmail.compose` or `gmail.modify` |
| `gmail_update_draft` | 10 | `gmail.compose` or `gmail.modify` |
| `gmail_send_draft` | 100 | `gmail.send` |
| `gmail_delete_draft` | 10 | `gmail.compose` or `gmail.modify` |
| `gmail_modify_labels` | 5 | `gmail.modify` |
| `gmail_trash` | 5 | `gmail.modify` |
| `gmail_untrash` | 5 | `gmail.modify` |

Narrower scopes (`gmail.send`, `gmail.compose`) are preferred over `gmail.modify`. The scope preset table in [[03-oauth-flow-and-token-storage]] maps the `write` preset to the *combination* users need for the full write surface.

---

## Typed errors surfaced

```rust
pub enum ToolError {
    ScopeMissing { required: Vec<String> },
    AttachmentTooLarge { size: u64, limit: u64 },
    RecipientLimitExceeded { count: usize, limit: usize },
    RateLimited,
    QuotaExhausted,
    InvalidArgs(String),
    Gmail(GmailError),
}
```

Each maps to a distinct JSON-RPC error `code` in the MCP response so clients can render remediation UI.

---

## Test plan

Unit tests with wiremock:

1. `gmail_send` with text body → POST `/users/me/messages/send` with base64url raw that round-trips back to the same MIME.
2. `gmail_send` with an attachment → multipart MIME generated correctly.
3. `gmail_send` with `reply_to_message_id` → fetches original, sets `In-Reply-To` + `threadId`.
4. `gmail_send` 26 MB attachment → `AttachmentTooLarge` before hitting the API.
5. `gmail_create_draft` + `gmail_update_draft` → draft id round-trip.
6. `gmail_send_draft` → returns the sent message id.
7. `gmail_modify_labels` with `add=INBOX, remove=UNREAD` → POST `/modify` with correct body.
8. `gmail_trash` → returns updated Message with TRASH label.
9. Scope missing → `ScopeMissing` returned without hitting the API.
10. DMARC / recipient-limit 403 from Gmail → specific typed error.

Integration tests (CI-gated):

1. Compose + send to the test account's own inbox; read back and assert subject/body.
2. Create draft → update draft → send draft → read the sent message.
3. Modify labels: archive a message, confirm INBOX label removed.

---

## Verification

```bash
cargo test -p gmail-mcp-core tools::send
cargo test -p gmail-mcp-core tools::drafts
cargo test -p gmail-mcp-core tools::modify

# Manual:
./target/debug/gmail-mcp auth --scopes write
./target/debug/gmail-mcp serve &
# Send a probe:
printf '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"gmail_send","arguments":{"to":["me@example.com"],"subject":"test","body_text":"hi"}}}\n' > /proc/$!/fd/0
```

---

## Files changed

| File | Change |
|------|--------|
| `crates/gmail-mcp-core/src/gmail/mime.rs` | New |
| `crates/gmail-mcp-core/src/gmail/client.rs` | Add 8 endpoints |
| `crates/gmail-mcp-core/src/tools/send.rs` | New |
| `crates/gmail-mcp-core/src/tools/drafts.rs` | New (4 tools in one file) |
| `crates/gmail-mcp-core/src/tools/modify.rs` | New (modify + trash + untrash) |
| `crates/gmail-mcp-core/src/tools/errors.rs` | Add write-specific variants |
| `crates/gmail-mcp-core/src/mcp/server.rs` | Scope pre-flight in dispatch |
| `crates/gmail-mcp/src/serve.rs` | Register 8 new tools |
| `crates/gmail-mcp-core/Cargo.toml` | `mail-builder = "0.5"` |

---

## Dependencies

- **Requires:** Phase 4.
- **Blocks:** Phase 7 (audit events for every write), Phase 9 (release must include both read and write).

---

## Related

- [[Gmail MCP Server Plan]]
- [[04-gmail-client-and-readonly-tools]]
- [[06-filters-batch-attachments]]
- [[07-production-hardening]]
