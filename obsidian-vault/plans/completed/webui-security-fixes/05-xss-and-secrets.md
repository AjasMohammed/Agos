---
title: "Phase 05 -- XSS Hardening and Secrets ZeroizingString"
tags:
  - webui
  - security
  - plan
  - v3
date: 2026-03-17
status: complete
effort: 4h
priority: critical
---

# Phase 05 -- XSS Hardening and Secrets ZeroizingString

> Verify and test that MiniJinja auto-escaping prevents XSS in all templates, and replace plain `String` with `ZeroizingString` for secret values at the web handler boundary.

---

## Why This Phase

Two critical security issues: (C4) MiniJinja auto-escapes `{{ }}` output by default for `.html` templates, but this has never been explicitly configured or tested for the AgentOS templates. If any template uses the `|safe` filter on user-controlled data, XSS is possible. The `task_detail.html` template renders `{{ prompt }}` and `{{ msg.content }}` which contain user-supplied data. (C5) The secrets `create` handler in `secrets.rs:50-74` receives the secret value as a plain `String` from the form, passes it to `vault.set()` as `&str`, and the original `String` remains in heap memory until garbage-collected -- it is never zeroized. The CLAUDE.md conventions require `ZeroizingString` for all secret values.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| MiniJinja auto-escape | On by default for `.html` extensions, but never explicitly enabled via `set_auto_escape_callback()`, configured, or tested | Explicitly enabled via `env.set_auto_escape_callback()` in `templates.rs`; integration tests confirm `<script>` tags are escaped |
| Template raw output | Unknown -- no audit of `\|safe` filter usage across all 15 template files | All templates audited; no `\|safe` filter used on user-controlled data |
| `CreateForm.value` in `secrets.rs:46` | `pub value: String` -- plain `String`, never zeroized | `String` received from form, immediately wrapped in `ZeroizingString` via `std::mem::take()`; passed as `&str` slice to `vault.set()`; zeroed on drop |
| Secret value in handler body (`secrets.rs:64`) | `&form.value` passed directly to `vault.set()` -- `form.value` lives on heap for handler duration | Wrapped immediately: `let secret_value = ZeroizingString::new(std::mem::take(&mut form.value));` |

---

## Subtasks

### 1. Explicitly enable MiniJinja auto-escaping

**File:** `crates/agentos-web/src/templates.rs`

MiniJinja enables auto-escaping by default for templates with `.html` extensions. However, being explicit prevents accidental breakage if the library's default changes. Add the callback after creating the `Environment`:

```rust
pub fn build_template_engine() -> Environment<'static> {
    let mut env = Environment::new();

    // Explicitly enable HTML auto-escaping for all .html templates.
    env.set_auto_escape_callback(|template_name| {
        if template_name.ends_with(".html") {
            minijinja::AutoEscape::Html
        } else {
            minijinja::AutoEscape::None
        }
    });

    // ... rest of template loading unchanged ...
    env
}
```

Insert this call before the first `env.add_template(...)` call (before line 6 in the current file).

### 2. Audit all templates for unsafe output

**Files:** All 15 `.html` files in `crates/agentos-web/src/templates/` and `crates/agentos-web/src/templates/partials/`

Search every template for patterns that bypass auto-escaping:
- `|safe` filter -- any use on user-controlled data is an XSS vulnerability
- `{% autoescape false %}` blocks
- Direct `{{ variable }}` output where the variable could contain user-controlled HTML

Run:
```bash
grep -rn '|safe\|autoescape false' crates/agentos-web/src/templates/
```

**Key template to audit: `task_detail.html`**

Lines 22 and 30 output `{{ prompt }}` and `{{ msg.content }}` inside `<pre>` tags. These contain:
- `prompt` -- the user's original task prompt text
- `msg.content` -- serialized JSON of intent payloads (from `serde_json::to_string(&msg.payload)`)

Both could contain `<script>` tags if an attacker injects them. With auto-escaping, `{{ prompt }}` renders `<script>` as `&lt;script&gt;`. No change needed unless `|safe` is found.

If any `|safe` usage is found on user-controlled data, remove it.

### 3. Add XSS integration tests

**New file:** `crates/agentos-web/tests/xss_tests.rs`

Create tests that render templates with XSS payloads and verify the payloads are escaped in the output:

```rust
use agentos_web::templates::build_template_engine;
use minijinja::context;

#[test]
fn test_task_detail_escapes_prompt_xss() {
    let env = build_template_engine();
    let tmpl = env.get_template("task_detail.html").unwrap();

    let xss_payload = "<script>alert('xss')</script>";
    let ctx = context! {
        page_title => "Test Task",
        task_id => "12345678-1234-1234-1234-123456789abc",
        state => "Running",
        agent_id => "abcdefab-1234-1234-1234-123456789abc",
        prompt => xss_payload,
        created_at => "2026-03-17 12:00:00",
        priority => 5,
        history => Vec::<minijinja::Value>::new(),
    };

    let rendered = tmpl.render(ctx).unwrap();

    // The raw <script> tag must NOT appear in output
    assert!(
        !rendered.contains("<script>alert('xss')</script>"),
        "XSS payload was not escaped in task_detail prompt"
    );
    // The escaped version must appear
    assert!(
        rendered.contains("&lt;script&gt;"),
        "Escaped script tag not found in output"
    );
}

#[test]
fn test_agent_name_escapes_xss() {
    let env = build_template_engine();
    let tmpl = env.get_template("agents.html").unwrap();

    let xss_name = "<img src=x onerror=alert(1)>";
    let agents = vec![context! {
        id => "12345678-1234-1234-1234-123456789abc",
        name => xss_name,
        provider => "Ollama",
        model => "llama3",
        status => "Idle",
        description => "test",
        roles => Vec::<String>::new(),
        current_task => Option::<String>::None,
        created_at => "2026-03-17",
        last_active => "2026-03-17",
    }];

    let ctx = context! {
        page_title => "Agents",
        agents,
    };

    let rendered = tmpl.render(ctx).unwrap();
    assert!(
        !rendered.contains("<img src=x onerror=alert(1)>"),
        "XSS payload was not escaped in agent name"
    );
    assert!(
        rendered.contains("&lt;img"),
        "Escaped img tag not found in output"
    );
}

#[test]
fn test_audit_details_escapes_xss() {
    let env = build_template_engine();
    let tmpl = env.get_template("audit.html").unwrap();

    let xss_details = "<script>document.cookie</script>";
    let entries = vec![context! {
        timestamp => "2026-03-17 12:00:00",
        event_type => "TaskStarted",
        severity => "Info",
        agent_id => Option::<String>::None,
        task_id => Option::<String>::None,
        tool_id => Option::<String>::None,
        details => xss_details,
    }];

    let ctx = context! {
        page_title => "Audit Log",
        entries,
        total_count => 1u64,
    };

    let rendered = tmpl.render(ctx).unwrap();
    assert!(
        !rendered.contains("<script>document.cookie</script>"),
        "XSS payload was not escaped in audit details"
    );
}
```

### 4. Wrap secret value in ZeroizingString at HTTP boundary

**File:** `crates/agentos-web/src/handlers/secrets.rs`

The `CreateForm` struct has `pub value: String`. Serde cannot deserialize directly into `ZeroizingString`, but we can wrap the value immediately after deserialization and take ownership to clear the original:

```rust
use agentos_vault::ZeroizingString;

pub async fn create(
    State(state): State<AppState>,
    axum::Form(mut form): axum::Form<CreateForm>,
) -> Response {
    use agentos_types::{SecretOwner, SecretScope};

    // Immediately wrap secret value in ZeroizingString and clear the original.
    // std::mem::take replaces form.value with an empty String, and the
    // ZeroizingString takes ownership of the original allocation.
    let secret_value = ZeroizingString::new(std::mem::take(&mut form.value));

    // ... scope parsing (from Phase 01) ...

    match state
        .kernel
        .vault
        .set(&form.name, secret_value.as_str(), SecretOwner::Kernel, scope)
        .await
    {
        Ok(_) => axum::response::Redirect::to("/secrets").into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to create secret: {}", e),
        )
            .into_response(),
    }
    // secret_value is dropped here -> ZeroizingString zeros the memory
}
```

**API compatibility:** The `vault.set()` method signature is `pub async fn set(&self, name: &str, value: &str, owner: SecretOwner, scope: SecretScope) -> Result<SecretID, AgentOSError>` (in `crates/agentos-vault/src/vault.rs:163`). It takes `&str`, so `secret_value.as_str()` works. `ZeroizingString` is re-exported from `crates/agentos-vault/src/lib.rs:5` as `pub use master_key::{MasterKey, ZeroizingString}`.

**Note on `mut form`:** The `axum::Form(mut form)` binding is necessary because `std::mem::take(&mut form.value)` requires a mutable reference.

### 5. Verify ZeroizingString API compatibility

Before implementing, verify that `ZeroizingString` supports `as_str()` or `Deref<Target=str>`:

```bash
grep -A 10 "impl.*Deref.*ZeroizingString\|pub fn as_str\|fn deref" crates/agentos-vault/src/master_key.rs
```

If `ZeroizingString` wraps `zeroize::Zeroizing<String>`, it has `Deref<Target=str>` automatically. If it is a custom type, check the available accessor.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-web/src/templates.rs` | Add `set_auto_escape_callback` before template loading |
| `crates/agentos-web/src/handlers/secrets.rs` | Add `use agentos_vault::ZeroizingString`; change `axum::Form(form)` to `axum::Form(mut form)`; wrap `form.value` in `ZeroizingString` via `std::mem::take`; use `secret_value.as_str()` in vault call |
| `crates/agentos-web/tests/xss_tests.rs` | **New file** -- 3 XSS escape tests for task_detail, agents, audit templates |

---

## Dependencies

None -- this phase can be done independently. The scope parsing in the `create` handler assumes Phase 01 is done. If Phase 01 is not yet complete, the scope parsing fix from Phase 01 should be included in this subtask as well.

---

## Test Plan

1. **Auto-escape callback test:** Verify `build_template_engine()` returns an environment where `<script>` in `{{ variable }}` is rendered as `&lt;script&gt;`.

2. **XSS test -- task prompt:** Render `task_detail.html` with `prompt = "<script>alert('xss')</script>"`. Assert raw `<script>` tag absent; `&lt;script&gt;` present.

3. **XSS test -- agent name:** Render `agents.html` with agent `name = "<img src=x onerror=alert(1)>"`. Assert raw `<img` tag absent.

4. **XSS test -- audit details:** Render `audit.html` with `details = "<script>document.cookie</script>"`. Assert escaping.

5. **Template audit grep:** `grep -rn '|safe\|autoescape false' crates/agentos-web/src/templates/` returns zero matches.

6. **ZeroizingString boundary test:** Write a unit test that constructs a `String`, wraps it in `ZeroizingString::new(std::mem::take(&mut original))`, and asserts `original.is_empty()` and `secret_value.as_str()` returns the original content.

7. **Compilation test:** `cargo build -p agentos-web` must pass with the `ZeroizingString` import from `agentos_vault`.

---

## Verification

```bash
# Must compile
cargo build -p agentos-web

# All tests pass including new XSS tests
cargo test -p agentos-web

# Verify auto-escape callback is set
grep -n "set_auto_escape_callback" crates/agentos-web/src/templates.rs
# Expected: 1 match

# Verify ZeroizingString usage in secrets handler
grep -n "ZeroizingString" crates/agentos-web/src/handlers/secrets.rs
# Expected: at least 1 match

# Verify no unsafe template output
grep -rn '|safe\|autoescape false' crates/agentos-web/src/templates/
# Expected: 0 matches

# Verify XSS test file exists
test -f crates/agentos-web/tests/xss_tests.rs && echo "XSS tests exist"
```

---

## Related

- [[WebUI Security Fixes Plan]] -- Master plan
- [[WebUI Security Fixes Data Flow]] -- Secrets data flow before/after diagram
