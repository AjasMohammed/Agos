---
title: Phase 03 — Template Rendering & JSON Formatting
tags:
  - webui
  - templates
  - minijinja
  - phase-03
date: 2026-04-11
status: planned
effort: 1.5d
priority: high
---

# Phase 03 — Template Rendering & JSON Formatting

> Stop dumping raw JSON strings into templates. Add a small library of MiniJinja filters (`pretty_json`, `markdown`, `human_role`, `humanize_event_type`, `relative_time`, `bytes_human`, `truncate_middle`) and apply them to every place where the UI currently shows raw serialized data. Migrate chat tool messages from a stringified-JSON column to a structured `chat_tool_calls` table.

---

## Why this phase

User-reported: "the text contents are not shown as parsed it is showing as raw json objects in some places. it should be user friendly. and also check for template rendering issues."

The audit identified concrete locations:

1. **Chat tool results** ([crates/agentos-web/src/chat_store.rs:226](crates/agentos-web/src/chat_store.rs#L226)) — `serde_json::json!({...}).to_string()` stuffed into `chat_messages.content`, rendered as `<pre>{stringified JSON}</pre>`.
2. **Task context window** ([crates/agentos-web/src/handlers/tasks.rs:169](crates/agentos-web/src/handlers/tasks.rs#L169)) — `serde_json::to_string(&msg.payload)` rendered as `<pre>{stringified JSON}</pre>`.
3. **Task context role** ([crates/agentos-web/src/handlers/tasks.rs:168](crates/agentos-web/src/handlers/tasks.rs#L168)) — `format!("{:?}", msg.intent_type)` shows Debug output (`USER_MESSAGE`).
4. **Audit log details** — events have a JSON `details` blob; the audit list and detail templates show it as raw text.
5. **Cost dashboard** — `payload.to_string()` for cost update event in `events.rs:236`.
6. **Trace input preview** — `tasks.rs:244` JSON-encodes input previews.

The fix is centralized: implement the filters once in `templates.rs`, apply them everywhere, and add a new storage path for chat tool calls so they don't go through the stringify-then-display antipattern at all.

## Current → Target State

| Concern | Current | Target |
|---------|---------|--------|
| `pretty_json` filter | None (only `truncate`) | Pretty-prints any value (JSON value, string-of-JSON, or `serde_json::Value`) with indentation; HTML-escapes; wraps in syntax-class spans for client-side highlighting |
| `markdown` filter | None | Server-side `pulldown-cmark` rendering with sanitisation via allowlist; used for stored chat assistant messages and any static markdown |
| `human_role` filter | None | Maps `IntentType` Debug strings to user-friendly labels |
| `humanize_event_type` filter | None | Maps `AuditEventType` enum strings to "Tool Executed" / "Memory Written" |
| `relative_time` filter | Hand-formatted strings in handlers | "2 minutes ago" / "yesterday" / "Apr 9 2026" |
| `truncate_middle` filter | None | "abc…xyz" for long IDs |
| `bytes_human` filter | None | "4.2 KB" / "1.7 MB" |
| Chat tool storage | `chat_messages` row with stringified JSON | New `chat_tool_calls` table with FK to message; structured columns |
| `get_messages` return type | `Vec<ChatMessage>` | `Vec<TimelineEntry>` (sum type with user/assistant/tool variants) |

## Detailed subtasks

### 1. Add MiniJinja filters in [src/templates.rs](crates/agentos-web/src/templates.rs)

```rust
use minijinja::value::{Value, ValueKind};
use minijinja::{Environment, Error, ErrorKind};

pub fn build_environment() -> Environment<'static> {
    let mut env = Environment::new();
    // ... existing setup ...

    env.add_filter("truncate", truncate_filter);
    env.add_filter("pretty_json", pretty_json_filter);
    env.add_filter("markdown", markdown_filter);
    env.add_filter("human_role", human_role_filter);
    env.add_filter("humanize_event_type", humanize_event_type_filter);
    env.add_filter("relative_time", relative_time_filter);
    env.add_filter("bytes_human", bytes_human_filter);
    env.add_filter("truncate_middle", truncate_middle_filter);

    env
}

fn pretty_json_filter(value: Value) -> Result<String, Error> {
    const MAX_INPUT: usize = 256 * 1024;

    // Accept either:
    //  - a JSON string (parse, re-format)
    //  - a serde_json::Value passed via context!
    //  - any minijinja value (serialize then format)
    let pretty = match value.kind() {
        ValueKind::String => {
            let s = value.as_str().unwrap_or("");
            if s.len() > MAX_INPUT {
                let head = &s[..s.char_indices().nth(MAX_INPUT).map(|(i,_)|i).unwrap_or(MAX_INPUT)];
                return Ok(format!("{}\n\n…[truncated, {} bytes total]…", head, s.len()));
            }
            match serde_json::from_str::<serde_json::Value>(s) {
                Ok(v) => serde_json::to_string_pretty(&v).unwrap_or_else(|_| s.to_string()),
                Err(_) => s.to_string(),  // Not JSON, return as-is
            }
        }
        _ => {
            let v = serde_json::to_value(&value).unwrap_or(serde_json::Value::Null);
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| String::from("null"))
        }
    };
    Ok(pretty)
}

fn markdown_filter(value: String) -> Result<String, Error> {
    use pulldown_cmark::{html, Options, Parser};

    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_GFM);

    let parser = Parser::new_ext(&value, options);
    let mut out = String::new();
    html::push_html(&mut out, parser);

    // Note: we trust the LLM output enough to render markdown but the *template*
    // engine still auto-escapes anything that isn't marked as safe. We mark this
    // output as safe ONLY because the input has already been processed for safety
    // upstream (via DOMPurify in the chat JS streaming path, OR by being a known
    // markdown source like a system message).
    Ok(out)
}

fn human_role_filter(value: String) -> Result<String, Error> {
    let s = value.to_uppercase();
    let label = match s.as_str() {
        "USER_MESSAGE" | "USERMESSAGE" | "USER" => "User",
        "ASSISTANT_MESSAGE" | "ASSISTANTMESSAGE" | "ASSISTANT" => "Assistant",
        "SYSTEM_MESSAGE" | "SYSTEMMESSAGE" | "SYSTEM" => "System",
        "TOOL_RESULT" | "TOOLRESULT" | "TOOL" => "Tool",
        "INTENT" => "Intent",
        _ => return Ok(s.replace('_', " ").to_lowercase()),
    };
    Ok(label.to_string())
}

fn humanize_event_type_filter(value: String) -> Result<String, Error> {
    // Convert "ToolExecuted" / "TOOL_EXECUTED" → "Tool Executed"
    let mut out = String::new();
    let s = value.replace('_', "");
    let mut prev_lower = false;
    for c in s.chars() {
        if c.is_uppercase() {
            if prev_lower { out.push(' '); }
            out.push(c);
            prev_lower = false;
        } else {
            out.push(c);
            prev_lower = true;
        }
    }
    Ok(out)
}

fn relative_time_filter(value: String) -> Result<String, Error> {
    use chrono::{DateTime, Utc, Duration};

    let dt: DateTime<Utc> = match DateTime::parse_from_rfc3339(&value) {
        Ok(d) => d.with_timezone(&Utc),
        Err(_) => return Ok(value),  // pass through
    };
    let now = Utc::now();
    let diff = now.signed_duration_since(dt);
    let label = if diff < Duration::seconds(10) { "just now".to_string() }
        else if diff < Duration::minutes(1) { format!("{}s ago", diff.num_seconds()) }
        else if diff < Duration::hours(1) { format!("{}m ago", diff.num_minutes()) }
        else if diff < Duration::hours(24) { format!("{}h ago", diff.num_hours()) }
        else if diff < Duration::days(7) { format!("{}d ago", diff.num_days()) }
        else { dt.format("%b %-d %Y").to_string() };
    Ok(label)
}

fn bytes_human_filter(value: u64) -> Result<String, Error> {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    let s = if value >= GB { format!("{:.1} GB", value as f64 / GB as f64) }
        else if value >= MB { format!("{:.1} MB", value as f64 / MB as f64) }
        else if value >= KB { format!("{:.1} KB", value as f64 / KB as f64) }
        else { format!("{} B", value) };
    Ok(s)
}

fn truncate_middle_filter(value: String, length: usize) -> Result<String, Error> {
    if value.chars().count() <= length { return Ok(value); }
    let half = length / 2 - 1;
    let chars: Vec<char> = value.chars().collect();
    let head: String = chars[..half].iter().collect();
    let tail: String = chars[chars.len() - half..].iter().collect();
    Ok(format!("{}…{}", head, tail))
}
```

### 2. Add `pulldown-cmark` to web crate

```toml
# crates/agentos-web/Cargo.toml
[dependencies]
pulldown-cmark = { version = "0.10", default-features = false, features = ["html"] }
```

It's already in the workspace (used by scratchpad), so this is a one-line add.

### 3. New `chat_tool_calls` table + migration

Add to [crates/agentos-web/src/chat_store.rs](crates/agentos-web/src/chat_store.rs):

```rust
const SCHEMA_V3: &str = r#"
CREATE TABLE IF NOT EXISTS chat_tool_calls (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL REFERENCES chat_sessions(id) ON DELETE CASCADE,
    after_message_id INTEGER REFERENCES chat_messages(id) ON DELETE SET NULL,
    iteration INTEGER NOT NULL DEFAULT 1,
    tool_name TEXT NOT NULL,
    intent_type TEXT,
    payload_json TEXT NOT NULL,
    result_json TEXT NOT NULL,
    success INTEGER NOT NULL DEFAULT 1,
    duration_ms INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_chat_tool_calls_session ON chat_tool_calls(session_id, created_at);
"#;

const MIGRATE_V2_TO_V3: &str = r#"
INSERT INTO chat_tool_calls (session_id, tool_name, intent_type, payload_json, result_json, duration_ms, created_at)
SELECT
    session_id,
    COALESCE(tool_name, 'unknown'),
    json_extract(content, '$.intent_type'),
    COALESCE(json_extract(content, '$.payload'), '{}'),
    COALESCE(json_extract(content, '$.result'), '{}'),
    COALESCE(tool_duration_ms, 0),
    created_at
FROM chat_messages
WHERE role = 'tool';

DELETE FROM chat_messages WHERE role = 'tool';
"#;
```

Run schema bumps inside a `migration_version` PRAGMA so re-runs are idempotent.

### 4. New `TimelineEntry` type and `get_timeline` query

```rust
// crates/agentos-web/src/chat_store.rs
#[derive(Debug, Clone)]
pub enum TimelineEntry {
    User { id: i64, content: String, created_at: String },
    Assistant { id: i64, content: String, created_at: String, tokens_used: Option<u64> },
    Tool {
        id: i64,
        after_message_id: Option<i64>,
        tool_name: String,
        intent_type: Option<String>,
        payload_json: String,
        result_json: String,
        success: bool,
        duration_ms: i64,
        created_at: String,
    },
}

impl ChatStore {
    pub fn get_timeline(&self, session_id: &str) -> Result<Vec<TimelineEntry>> {
        let conn = self.conn.lock().unwrap();

        // Two queries, then merge by created_at:
        let mut msgs: Vec<(String, TimelineEntry)> = conn
            .prepare("SELECT id, role, content, created_at FROM chat_messages WHERE session_id = ?1 ORDER BY id ASC")?
            .query_map(...)?
            .map(|r| {
                let (id, role, content, created_at) = r?;
                let entry = match role.as_str() {
                    "user" => TimelineEntry::User { id, content, created_at: created_at.clone() },
                    "assistant" => TimelineEntry::Assistant { id, content, created_at: created_at.clone(), tokens_used: None },
                    other => return Err(...)
                };
                Ok((created_at, entry))
            })
            .collect::<Result<_>>()?;

        let tools: Vec<(String, TimelineEntry)> = conn
            .prepare("SELECT id, after_message_id, tool_name, intent_type, payload_json, result_json, success, duration_ms, created_at FROM chat_tool_calls WHERE session_id = ?1 ORDER BY id ASC")?
            .query_map(...)?
            .map(|r| { /* build TimelineEntry::Tool */ })
            .collect::<Result<_>>()?;

        msgs.extend(tools);
        msgs.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(msgs.into_iter().map(|(_, e)| e).collect())
    }

    pub fn add_tool_call(&self, session_id: &str, after_message_id: Option<i64>, call: &ChatToolCallRecord) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO chat_tool_calls (session_id, after_message_id, iteration, tool_name, intent_type, payload_json, result_json, success, duration_ms, created_at) VALUES (?,?,?,?,?,?,?,?,?,?)",
            params![
                session_id,
                after_message_id,
                call.iteration as i64,
                call.tool_name,
                call.intent_type,
                serde_json::to_string(&call.payload).unwrap_or_default(),
                serde_json::to_string(&call.result).unwrap_or_default(),
                if call.success { 1 } else { 0 },
                call.duration_ms as i64,
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }
}
```

The existing `add_tool_calls` is replaced with a loop calling `add_tool_call`.

### 5. Update task detail handler

In [crates/agentos-web/src/handlers/tasks.rs](crates/agentos-web/src/handlers/tasks.rs#L160):

```rust
let history: Vec<_> = task.history.iter().map(|msg| {
    context! {
        role => format!("{:?}", msg.intent_type),     // → human_role filter in template
        payload => msg.payload.clone(),               // pass raw Value, NOT serialised
        timestamp => msg.timestamp.to_rfc3339(),
    }
}).collect();
```

And in `task_detail.html`:

```html
<details class="context-entry">
    <summary class="context-summary">
        <span class="role-badge role-{{ msg.role|lower }}">{{ msg.role|human_role }}</span>
        <span class="muted context-preview">{{ msg.payload|pretty_json|truncate(80) }}</span>
        <span class="muted">{{ msg.timestamp|relative_time }}</span>
    </summary>
    <pre class="context-content"><code class="language-json">{{ msg.payload|pretty_json }}</code></pre>
</details>
```

### 6. Update audit log templates

[src/templates/audit.html](crates/agentos-web/src/templates/audit.html) — replace `{{ entry.event_type }}` with `{{ entry.event_type|humanize_event_type }}`. Replace `<pre>{{ entry.details }}</pre>` with `<pre>{{ entry.details|pretty_json }}</pre>`.

### 7. Update cost dashboard

[src/handlers/events.rs:236](crates/agentos-web/src/handlers/events.rs#L236) — emit a structured payload, not stringified.

### 8. Update chat conversation handler

The `conversation()` handler now passes `timeline` instead of `messages`:

```rust
let timeline = store.get_timeline(&session_id)?;
let timeline_ctx: Vec<_> = timeline.into_iter().map(|e| match e {
    TimelineEntry::User { id, content, created_at } => context! {
        kind => "user", id, content, created_at,
    },
    TimelineEntry::Assistant { id, content, created_at, tokens_used } => context! {
        kind => "assistant", id, content, created_at, tokens_used,
    },
    TimelineEntry::Tool { id, tool_name, intent_type, payload_json, result_json, success, duration_ms, created_at, .. } => context! {
        kind => "tool", id, tool_name, intent_type, payload_json, result_json, success, duration_ms, created_at,
    },
}).collect();
```

### 9. Add `relative_time` everywhere

Sweep the templates and replace hand-formatted timestamps with `{{ ts|relative_time }}`. Quick targets:
- `chat_user_msg.html`, `chat_assistant_msg.html` (chat)
- `tasks.html` (task list)
- `audit.html` (audit list)
- `agents.html` (agent list — show "connected 2h ago")
- `notifications/inbox.html`

## Files changed

| File | Change |
|------|--------|
| [crates/agentos-web/src/templates.rs](crates/agentos-web/src/templates.rs) | Add 7 new filters |
| [crates/agentos-web/Cargo.toml](crates/agentos-web/Cargo.toml) | Add `pulldown-cmark` dep |
| [crates/agentos-web/src/chat_store.rs](crates/agentos-web/src/chat_store.rs) | New `chat_tool_calls` schema + migration; new `TimelineEntry`; new `get_timeline`/`add_tool_call`; deprecate `add_tool_calls`/role='tool' path |
| [crates/agentos-web/src/handlers/chat.rs](crates/agentos-web/src/handlers/chat.rs) | Use `get_timeline` in `conversation()`; persist tool calls via `add_tool_call` in spawned task |
| [crates/agentos-web/src/handlers/tasks.rs](crates/agentos-web/src/handlers/tasks.rs) | Pass raw `payload` Value to template, not stringified |
| [crates/agentos-web/src/handlers/events.rs](crates/agentos-web/src/handlers/events.rs) | Cost SSE event uses structured payload |
| [crates/agentos-web/src/templates/task_detail.html](crates/agentos-web/src/templates/task_detail.html) | Use `human_role`, `pretty_json`, `relative_time` filters |
| [crates/agentos-web/src/templates/audit.html](crates/agentos-web/src/templates/audit.html) | Use `humanize_event_type`, `pretty_json` |
| [crates/agentos-web/src/templates/audit_detail.html](crates/agentos-web/src/templates/audit_detail.html) | Same |
| [crates/agentos-web/src/templates/tasks.html](crates/agentos-web/src/templates/tasks.html) | Use `relative_time` |

## Dependencies

- Independent of: 01, 04, 05
- Blocks: [[02-chat-ui-redesign]] (markdown filter), [[06-cli-parity-management-pages]] (pretty_json filter), [[07-cli-parity-observability]]

## Test plan

1. **Unit: filters**
   - `crates/agentos-web/src/templates.rs#tests`
   - `pretty_json_filter`: round-trip a `serde_json::Value::Object`, assert indented output. Pass a malformed JSON string, assert returned as-is. Pass 300 KB string, assert truncated.
   - `markdown_filter`: input `"# Hello\n\n* a\n* b"`, assert output contains `<h1>Hello</h1><ul><li>a</li>`.
   - `human_role_filter`: input "USER_MESSAGE", output "User". Input "TOOL_RESULT", output "Tool".
   - `humanize_event_type_filter`: input "ToolExecuted", output "Tool Executed". Input "MEMORY_WRITTEN", output "Memory Written".
   - `relative_time_filter`: input now-30s, output "30s ago". Input now-2d, output "2d ago".
   - `bytes_human_filter`: 1500 → "1.5 KB", 1572864 → "1.5 MB".
   - `truncate_middle_filter`: "abcdefghijkl", 6 → "ab…kl".
2. **Integration: chat_tool_calls migration**
   - Seed a fresh DB with old-schema tool messages, run migration, assert rows moved to `chat_tool_calls` and removed from `chat_messages`.
3. **Integration: timeline ordering**
   - Insert user msg, then tool call, then assistant msg, then tool call, then assistant msg. `get_timeline` should return them in order.
4. **Integration: task detail rendering**
   - Create a task with 3 history messages of varying intent_type. Render `/tasks/{id}`. Assert HTML contains "User", "Assistant", "Tool" labels (not "USER_MESSAGE"). Assert payloads are pretty-printed (multiple lines with indentation).
5. **Manual: audit log**
   - Open `/audit`, assert event types are formatted ("Tool Executed" not "ToolExecuted"). Open a detail page, assert details JSON is pretty-printed.

## Verification

```bash
cargo build -p agentos-web
cargo test -p agentos-web templates::filters
cargo test -p agentos-web chat_store::migration
cargo test -p agentos-web chat_store::timeline
cargo clippy -p agentos-web -- -D warnings
cargo fmt --all -- --check

# Manual: open /chat/{existing-session}, verify pre-existing tool messages now render as cards
# Open /tasks/{any-task}, verify context window is human-readable
# Open /audit, verify event types are spaced
```

## Related

- [[WebUI Overhaul Plan]]
- [[02-chat-ui-redesign]]
- [[06-cli-parity-management-pages]]
