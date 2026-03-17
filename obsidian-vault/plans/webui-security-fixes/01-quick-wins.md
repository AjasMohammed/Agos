---
title: "Phase 01 -- Quick Wins and Dead Code Cleanup"
tags:
  - webui
  - security
  - plan
  - v3
date: 2026-03-17
status: complete
effort: 2h
priority: high
---

# Phase 01 -- Quick Wins and Dead Code Cleanup

> Remove dead `is_partial()` function, cap audit query limit to 1,000, fix secret scope parsing to handle all `SecretScope` variants, and deduplicate template markup using `{% include %}`.

---

## Why This Phase

These are low-risk, zero-dependency fixes that improve correctness immediately. Each is a small, isolated change that cannot break other functionality. Fixing them first reduces the surface area of bugs before tackling the harder security work in later phases.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `is_partial()` in `handlers/mod.rs:35-37` | Dead code; checks `query.contains("partial=")` but is never called -- all handlers use the typed `Query<ListQuery>` extractor and check `query.partial.as_deref() == Some("list")` directly | Removed entirely |
| Audit `limit` param (`audit.rs:18`) | `query.limit.unwrap_or(50)` with no upper bound; `?limit=999999999` causes `query_recent(999999999)` which allocates an unbounded `Vec<AuditEntry>` | Clamped to `query.limit.unwrap_or(50).min(1000)` |
| Secret scope (`secrets.rs:56-59`) | Match `_ => SecretScope::Global` silently discards valid scope values like `"kernel"`, `"agent:<uuid>"`, `"tool:<uuid>"` | Parse `Kernel`, `Agent(AgentID)`, `Tool(ToolID)` variants; return 400 for unrecognized values |
| Template markup duplication (S1) | Full page templates repeat the same HTML structure found in partials; no `{% include %}` usage | Full page templates use `{% include "partials/foo.html" %}` inside `{% for %}` loops |

---

## Subtasks

### 1. Remove dead `is_partial()` function

**File:** `crates/agentos-web/src/handlers/mod.rs`

Delete the `is_partial` function (lines 34-37):

```rust
// DELETE THIS:
/// Check if request is an HTMX partial request.
pub fn is_partial(query: &str) -> bool {
    query.contains("partial=")
}
```

This function is never called anywhere in the codebase. Every handler uses the typed `Query<ListQuery>` extractor which deserializes `?partial=list` into `query.partial == Some("list")`. Confirm zero callers with `grep -rn "is_partial" crates/agentos-web/` before removing.

The remaining content of `handlers/mod.rs` after removal should be:

```rust
pub mod agents;
pub mod audit;
pub mod dashboard;
pub mod pipelines;
pub mod secrets;
pub mod tasks;
pub mod tools;

use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use minijinja::Environment;

/// Render a template or return a 500 error.
pub fn render(
    env: &Environment<'_>,
    template_name: &str,
    ctx: minijinja::value::Value,
) -> Response {
    match env.get_template(template_name) {
        Ok(tmpl) => match tmpl.render(ctx) {
            Ok(html) => Html(html).into_response(),
            Err(e) => {
                tracing::error!(error = %e, template = template_name, "Template render error");
                (StatusCode::INTERNAL_SERVER_ERROR, "Template render error").into_response()
            }
        },
        Err(e) => {
            tracing::error!(error = %e, template = template_name, "Template not found");
            (StatusCode::INTERNAL_SERVER_ERROR, "Template not found").into_response()
        }
    }
}
```

### 2. Cap audit query limit to 1,000

**File:** `crates/agentos-web/src/handlers/audit.rs`

Change line 18 from:

```rust
let limit = query.limit.unwrap_or(50);
```

To:

```rust
let limit = query.limit.unwrap_or(50).min(1000);
```

The underlying `AuditLog::query_recent(limit: u32)` in `crates/agentos-audit/src/log.rs:468` passes the limit directly to `LIMIT ?1` in SQL. While SQLite handles large LIMIT values gracefully at the SQL level, the resulting `Vec<AuditEntry>` would consume unbounded memory. Capping at 1,000 is generous enough for the web UI while preventing DoS.

### 3. Fix secret scope parsing

**File:** `crates/agentos-web/src/handlers/secrets.rs`

Replace lines 56-59 (the scope match):

```rust
// CURRENT (buggy):
let scope = match form.scope.as_deref() {
    Some("global") | None => SecretScope::Global,
    _ => SecretScope::Global,  // <-- always Global!
};
```

With a correct parser that handles all `SecretScope` variants. The `SecretScope` enum is defined in `crates/agentos-types/src/secret.rs` with variants `Global`, `Kernel`, `Agent(AgentID)`, `Tool(ToolID)`. The `AgentID` and `ToolID` types implement `FromStr` via the `define_id!()` macro:

```rust
let scope = match form.scope.as_deref() {
    Some("global") | None => SecretScope::Global,
    Some("kernel") => SecretScope::Kernel,
    Some(other) => {
        if let Some(agent_id_str) = other.strip_prefix("agent:") {
            match agent_id_str.parse::<agentos_types::AgentID>() {
                Ok(id) => SecretScope::Agent(id),
                Err(_) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        format!("Invalid agent ID in scope: {}", agent_id_str),
                    )
                        .into_response();
                }
            }
        } else if let Some(tool_id_str) = other.strip_prefix("tool:") {
            match tool_id_str.parse::<agentos_types::ToolID>() {
                Ok(id) => SecretScope::Tool(id),
                Err(_) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        format!("Invalid tool ID in scope: {}", tool_id_str),
                    )
                        .into_response();
                }
            }
        } else {
            return (
                StatusCode::BAD_REQUEST,
                format!(
                    "Unrecognized scope: '{}'. Use 'global', 'kernel', 'agent:<uuid>', or 'tool:<uuid>'.",
                    other
                ),
            )
                .into_response();
        }
    }
};
```

### 4. Wrap blocking audit calls in `spawn_blocking`

**Files:** `crates/agentos-web/src/handlers/audit.rs` (line 19), `crates/agentos-web/src/handlers/dashboard.rs` (line 14)

`AuditLog::query_recent()` acquires a `std::sync::Mutex` (not tokio), which blocks the Tokio worker thread. Under load this causes head-of-line blocking.

**`audit.rs`** — wrap the query:
```rust
let entries = tokio::task::spawn_blocking({
    let audit = Arc::clone(&state.kernel.audit);
    move || audit.query_recent(limit as u64)
})
.await
.map_err(|e| {
    tracing::error!("audit query panicked: {e}");
    (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response()
})?
.unwrap_or_default();
```

**`dashboard.rs`** — same pattern for the `query_recent(10)` call on line 14.

The `audit` field on `AppState` is `Arc<AuditLog>` so it can be cloned into the closure.

---

### 5. Fix template `expect()` panics in `templates.rs`

**File:** `crates/agentos-web/src/templates.rs`

All `add_template(...)` calls use `.expect("failed to load ...")`. These only fail on invalid MiniJinja syntax, but the CLAUDE.md requires no `.unwrap()` in production paths.

Change `build_template_engine()` to return `Result<Environment<'static>, minijinja::Error>`:

```rust
pub fn build_template_engine() -> Result<Environment<'static>, minijinja::Error> {
    let mut env = Environment::new();
    env.add_template("base.html", include_str!("templates/base.html"))?;
    // ... all other add_template calls with `?` instead of `.expect(...)`
    Ok(env)
}
```

Update the call site in `server.rs` (or `lib.rs`) to propagate the error with `?`.

---

### 6. Deduplicate templates with `{% include %}`

**Files:** All full-page templates in `crates/agentos-web/src/templates/`

MiniJinja supports `{% include "partials/foo.html" %}` which inherits the parent template's context. For each full-page template that renders a list, replace the inline markup with an include of the corresponding partial inside the `{% for %}` loop.

Example for `agents.html` -- replace the inline agent card markup loop body with:

```jinja
{% for agent in agents %}
  {% include "partials/agent_card.html" %}
{% endfor %}
```

Apply the same pattern to:
- `tasks.html` -- use `{% include "partials/task_row.html" %}`
- `tools.html` -- use `{% include "partials/tool_card.html" %}`
- `secrets.html` -- use `{% include "partials/secret_row.html" %}`
- `pipelines.html` -- use `{% include "partials/pipeline_row.html" %}`
- `audit.html` -- use `{% include "partials/log_line.html" %}`

**Important:** MiniJinja `{% include %}` inherits the parent context, so the loop variable (e.g., `agent`) is available inside the partial. Verify each partial template references the correct variable name that matches the `{% for %}` loop variable.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-web/src/handlers/mod.rs` | Remove `is_partial()` function (lines 34-37) |
| `crates/agentos-web/src/handlers/audit.rs` | Add `.min(1000)` to limit; wrap `query_recent` in `spawn_blocking` |
| `crates/agentos-web/src/handlers/dashboard.rs` | Wrap `query_recent(10)` in `spawn_blocking` |
| `crates/agentos-web/src/templates.rs` | Change `build_template_engine()` to return `Result<_, minijinja::Error>`; replace `.expect()` with `?` |
| `crates/agentos-web/src/handlers/secrets.rs` | Replace scope match arms (lines 56-59) to parse `Kernel`, `Agent(id)`, `Tool(id)` |
| `crates/agentos-web/src/templates/agents.html` | Replace inline agent card markup with `{% include %}` |
| `crates/agentos-web/src/templates/tasks.html` | Replace inline task row markup with `{% include %}` |
| `crates/agentos-web/src/templates/tools.html` | Replace inline tool card markup with `{% include %}` |
| `crates/agentos-web/src/templates/secrets.html` | Replace inline secret row markup with `{% include %}` |
| `crates/agentos-web/src/templates/pipelines.html` | Replace inline pipeline row markup with `{% include %}` |
| `crates/agentos-web/src/templates/audit.html` | Replace inline log line markup with `{% include %}` |

---

## Dependencies

None -- this is the first phase. No other phase depends on these changes and these changes have no prerequisites.

---

## Test Plan

1. **Compile check:** `cargo build -p agentos-web` must pass. Removing `is_partial()` should cause no compile errors since it is dead code. If any caller exists, the compiler will catch it.

2. **Audit limit test:** Add a unit test or integration test that verifies:
   - `GET /audit?limit=5` returns at most 5 entries
   - `GET /audit?limit=999999999` returns at most 1,000 entries
   - `GET /audit` (no limit) returns at most 50 entries (default)

3. **Secret scope test:** Add tests for the `create` handler verifying:
   - `scope=global` produces `SecretScope::Global`
   - `scope=kernel` produces `SecretScope::Kernel`
   - `scope=agent:<valid-uuid>` produces `SecretScope::Agent(AgentID)`
   - `scope=tool:<valid-uuid>` produces `SecretScope::Tool(ToolID)`
   - `scope=nonsense` returns 400 Bad Request with descriptive message
   - No scope (None) defaults to `SecretScope::Global`

4. **Template rendering test:** Render each full-page template and verify the output contains the expected HTML from the included partials.

5. **Dead code grep:** `grep -rn "is_partial" crates/agentos-web/` should return zero results after removal.

---

## Verification

```bash
# Must compile
cargo build -p agentos-web

# All tests pass
cargo test -p agentos-web

# Confirm dead code removed
grep -rn "is_partial" crates/agentos-web/src/
# Expected: 0 matches

# Confirm limit clamp exists
grep -n "min(1000)" crates/agentos-web/src/handlers/audit.rs
# Expected: 1 match

# Confirm scope parsing handles all variants
grep -n "SecretScope::Kernel" crates/agentos-web/src/handlers/secrets.rs
grep -n "SecretScope::Agent" crates/agentos-web/src/handlers/secrets.rs
grep -n "SecretScope::Tool" crates/agentos-web/src/handlers/secrets.rs
# Expected: 1 match each
```

---

## Related

- [[WebUI Security Fixes Plan]] -- Master plan
- [[WebUI Security Fixes Data Flow]] -- Flow diagrams
