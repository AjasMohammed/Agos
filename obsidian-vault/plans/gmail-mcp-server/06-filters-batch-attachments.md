---
title: Phase 6 — Filters, Batch Operations, Attachments
tags:
  - gmail
  - tools
  - filters
  - batch
  - phase-6
date: 2026-04-18
status: planned
effort: 1d
priority: medium
---

# Phase 6 — Filters, Batch Operations, Attachments

> Add the remaining surface that's not on the most-wanted read or write path but matters for real-world mailbox automation: label/filter management, batch modify/trash, and send-with-attachments via staged upload.

---

## Why this phase

With Phase 4+5 an agent can search, read, send, and triage messages one at a time. This phase covers the "power user" tools that make it practical for an agent to actually tidy a mailbox, automate routing, or handle multi-recipient campaigns without burning quota.

---

## Deliverables

- Label CRUD: `gmail_create_label`, `gmail_update_label`, `gmail_delete_label`, `gmail_get_or_create_label`.
- Filter CRUD: `gmail_create_filter`, `gmail_delete_filter`, `gmail_create_filter_from_template`.
- Batch ops: `gmail_batch_modify_labels`, `gmail_batch_trash`, `gmail_batch_delete`.
- Attachment upload via resumable upload for >5 MB attachments.
- Hard caps on batch sizes and per-call recipients.

---

## Label tools

Gmail label endpoints:

| Verb | Path | Cost |
|------|------|------|
| POST | `users/me/labels` | 5 |
| PATCH | `users/me/labels/{id}` | 5 |
| DELETE | `users/me/labels/{id}` | 5 |

```rust
// tool signatures (schemas elided for brevity)
gmail_create_label(name: String, label_color?: {text_color, background_color}) -> Label
gmail_update_label(id: String, name?: String, label_color?: ...) -> Label
gmail_delete_label(id: String) -> ()
gmail_get_or_create_label(name: String) -> Label   // looks up or creates; idempotent
```

Use for: agent wants to ensure a "Processed by AI" label exists before tagging messages.

---

## Filter tools

Gmail filters are declarative: criteria + action. They enable server-side automation without the agent running 24/7.

```rust
gmail_create_filter(criteria: Criteria, action: Action) -> Filter
gmail_delete_filter(id: String) -> ()
gmail_create_filter_from_template(template: String, params: Value) -> Filter
```

The `create_filter_from_template` tool exposes 4 built-in templates:

| Template | Parameters | Effect |
|----------|-----------|--------|
| `auto_label_from` | `from_addr`, `label_name` | Apply label to all messages from an address |
| `archive_list` | `list_id`, `never_mark_important` | Auto-archive mailing list; optional `markAsRead`; never-important |
| `forward_to` | `from_addr` (optional), `to_addr` | Auto-forward matching mail (requires forwarding alias already verified) |
| `delete_promotional` | `sender_domain` | Delete anything `from:<domain> category:promotions` older than 30d |

Templates reduce foot-guns; the agent supplies semantic intent, the server builds correct criteria JSON.

---

## Batch operations

### `gmail_batch_modify_labels`

```rust
pub struct BatchModifyArgs {
    pub message_ids: Vec<String>,    // max 1000 per Gmail API
    pub add_label_ids: Vec<String>,
    pub remove_label_ids: Vec<String>,
}
```

One API call: POST `users/me/messages/batchModify` with `ids` + `addLabelIds` + `removeLabelIds`. Cost: 50 units for the call.

We cap at 500 per tool call (Gmail allows 1000 but performance degrades). If the client passes more, we reject at schema validation — no client-side splitting surprises.

### `gmail_batch_trash`

Gmail has no `batchTrash`. Implement as parallel `trash` calls with a bounded concurrency of 10:

```rust
use futures::stream::{FuturesUnordered, StreamExt};

async fn batch_trash(client: &Client, ids: Vec<String>) -> Vec<BatchResult> {
    let mut stream = futures::stream::iter(ids.into_iter().map(|id| {
        let client = client.clone();
        async move {
            let res = client.messages_trash(&id).await;
            BatchResult { id, success: res.is_ok(), error: res.err().map(|e| e.to_string()) }
        }
    })).buffer_unordered(10);

    let mut out = Vec::new();
    while let Some(r) = stream.next().await { out.push(r); }
    out
}
```

Returns per-id success/failure so the agent can act on partial failure.

### `gmail_batch_delete`

Uses `users/me/messages/batchDelete` — same shape as `batchModify`. **Permanent.** Requires `gmail.modify` scope and confirmation at the tool layer:

```rust
pub struct BatchDeleteArgs {
    pub message_ids: Vec<String>,
    pub confirm: bool,    // must be true; tool rejects otherwise
}
```

Hosts should route this through their approval UI. On AgentOS it lands in the `exec_capable` risk class by default.

---

## Attachments — resumable upload for large messages

Gmail `messages.send` maxes out around 25 MB over the standard JSON endpoint. For larger messages (up to 35 MB total, RFC 5322 limit), use the resumable upload endpoint:

```
POST https://gmail.googleapis.com/upload/gmail/v1/users/me/messages/send?uploadType=resumable
```

```rust
async fn send_large(&self, raw: &[u8], thread_id: Option<&str>) -> Result<MessageRef, GmailError> {
    // 1. Initiate.
    let init = self.http
        .post("https://gmail.googleapis.com/upload/gmail/v1/users/me/messages/send?uploadType=resumable")
        .bearer_auth(self.tokens.access_token().await?)
        .header("X-Upload-Content-Type", "message/rfc822")
        .header("X-Upload-Content-Length", raw.len())
        .send().await?.error_for_status()?;
    let session = init.headers().get("Location").ok_or(GmailError::Transport("no upload session".into()))?
        .to_str().unwrap().to_string();

    // 2. Upload in 5 MB chunks with Content-Range for resumability.
    for (start, end, chunk) in chunks_of(raw, 5 * 1024 * 1024) {
        // (send chunk with Content-Range: bytes {start}-{end}/{total})
    }

    // 3. Final chunk returns 200 with the created message.
    //    Earlier chunks return 308 (Resume Incomplete).
    // …
}
```

We cap total attachment size at 25 MB by default and gate the resumable-upload path behind an opt-in `--allow-large-send` flag, since agents sending 30 MB emails is rare and a notable risk vector.

---

## Test plan

Unit tests with wiremock:

1. `gmail_create_label` with name "AgentProcessed" → POST with expected body.
2. `gmail_delete_label` on system label (id = "INBOX") → Gmail rejects → typed error; our tool surfaces it cleanly.
3. `gmail_get_or_create_label` twice → first creates, second returns existing id (no second POST).
4. `gmail_create_filter_from_template`, template = `auto_label_from` → filter POST with expected criteria.
5. `gmail_batch_modify_labels` with 501 ids → schema rejection.
6. `gmail_batch_trash` with 50 ids, 3 simulated 429s → 3 retries succeed, all 50 in results.
7. `gmail_batch_delete` with `confirm: false` → rejected with typed error.
8. Resumable upload: 20 MB message in 4 × 5 MB chunks, final chunk returns 200.
9. Resumable upload: chunk 3 returns 500 → we retry it once before giving up.

---

## Verification

```bash
cargo test -p gmail-mcp-core tools::labels
cargo test -p gmail-mcp-core tools::filters
cargo test -p gmail-mcp-core tools::batch
cargo test -p gmail-mcp-core gmail::resumable

# Manual, after --scopes full auth:
# create + use + delete a label round-trip via the MCP CLI harness
```

---

## Files changed

| File | Change |
|------|--------|
| `crates/gmail-mcp-core/src/gmail/client.rs` | Add label + filter + batch + resumable endpoints |
| `crates/gmail-mcp-core/src/gmail/resumable.rs` | New resumable upload helper |
| `crates/gmail-mcp-core/src/tools/labels.rs` | New (4 label tools) |
| `crates/gmail-mcp-core/src/tools/filters.rs` | New (3 filter tools) |
| `crates/gmail-mcp-core/src/tools/batch.rs` | New (3 batch tools) |
| `crates/gmail-mcp/src/serve.rs` | Register new tools |

---

## Dependencies

- **Requires:** Phase 4 (client + readonly tools), Phase 5 (write tools).
- **Blocks:** Phase 7 (rate-limiter needs to know about batch costs).

---

## Related

- [[Gmail MCP Server Plan]]
- [[05-write-tools-send-draft-modify]]
- [[07-production-hardening]]
