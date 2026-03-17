---
title: "Phase 06 -- Tool Install Path Security"
tags:
  - webui
  - security
  - plan
  - v3
date: 2026-03-17
status: complete
effort: 3h
priority: critical
---

# Phase 06 -- Tool Install Path Security

> Restrict the web UI tool install handler to only read manifest files from an allowlist of directories, with full path canonicalization to prevent symlink and traversal bypasses.

---

## Why This Phase

Issue C6: The `tools::install` handler in `crates/agentos-web/src/handlers/tools.rs:64-108` accepts an arbitrary `manifest_path` from a form submission. It blocks `..` in the raw string (line 69: `if form.manifest_path.contains("..") { return 400 }`), but this is trivially bypassable:
- **Symlinks:** A symlink inside an allowed directory can point to `/etc/passwd` without containing `..`
- **Absolute paths:** `/etc/passwd` does not contain `..` and passes the check
- **No allowlist:** Any readable file on the filesystem can be loaded and parsed as a TOML manifest

The kernel config already defines `core_tools_dir` and `user_tools_dir` for tool installation directories. The web handler should restrict to these directories and use `std::fs::canonicalize()` (which resolves symlinks and `..` components) before checking the allowlist.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Path validation (`tools.rs:69-71`) | `if form.manifest_path.contains("..") { return 400 }` -- only blocks literal `..` substring | Full canonicalization via `std::fs::canonicalize()` + check that resolved path starts with an allowed directory |
| Allowed directories | None -- any path on the filesystem accepted | Only paths under `core_tools_dir` and `user_tools_dir` from kernel config |
| Config access in handler | Handler has `state.kernel` but not the config's tool directories | Add `allowed_tool_dirs: Vec<PathBuf>` to `AppState`, populated from config at startup |
| File extension check | None -- any file type accepted | Must have `.toml` extension (defense in depth) |
| Audit logging | No audit entry for blocked attempts | Write `SecurityViolation` audit entry when a path is blocked |

---

## Subtasks

### 1. Add allowed tool directories to AppState

**File:** `crates/agentos-web/src/state.rs`

Add a field for the allowed tool installation directories:

```rust
use std::path::PathBuf;

#[derive(Clone)]
pub struct AppState {
    pub kernel: Arc<Kernel>,
    pub templates: Arc<Environment<'static>>,
    pub allowed_tool_dirs: Vec<PathBuf>,
    // ... other fields from previous phases (csrf_tokens, etc.)
}
```

**File:** `crates/agentos-web/src/server.rs`

Accept and store `allowed_tool_dirs` in `AppState`. The caller (CLI `handle_serve`) passes these from the kernel config:

```rust
impl WebServer {
    pub fn new(bind_addr: SocketAddr, kernel: Arc<Kernel>, allowed_tool_dirs: Vec<PathBuf>) -> Self {
        let templates = Arc::new(build_template_engine());
        let state = AppState {
            kernel,
            templates,
            allowed_tool_dirs,
            // ... other fields ...
        };
        Self { bind_addr, state }
    }
}
```

**File:** `crates/agentos-cli/src/commands/web.rs`

In `handle_serve`, extract tool directories from the kernel config and pass them to `WebServer::new()`. The kernel's `data_dir` is available as `kernel.data_dir`. The default tool directories are `tools/core/` and `tools/user/` under the data directory:

```rust
let allowed_tool_dirs = vec![
    kernel.data_dir.join("tools").join("core"),
    kernel.data_dir.join("tools").join("user"),
];
let server = WebServer::new(addr, kernel.clone(), allowed_tool_dirs);
```

### 2. Replace path validation with canonicalization + allowlist

**File:** `crates/agentos-web/src/handlers/tools.rs`

Replace the current `install` handler (lines 64-108) with a secure version:

```rust
pub async fn install(
    State(state): State<AppState>,
    axum::Form(form): axum::Form<InstallForm>,
) -> Response {
    let requested_path = std::path::Path::new(&form.manifest_path);

    // Step 1: Canonicalize to resolve symlinks and relative components.
    let canonical_path = match std::fs::canonicalize(requested_path) {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("Cannot resolve manifest path: {}", e),
            )
                .into_response();
        }
    };

    // Step 2: Check that canonicalized path starts with an allowed directory.
    let allowed = state.allowed_tool_dirs.iter().any(|allowed_dir| {
        // Canonicalize the allowed dir too, in case it has symlinks
        match std::fs::canonicalize(allowed_dir) {
            Ok(canonical_allowed) => canonical_path.starts_with(&canonical_allowed),
            Err(_) => false,
        }
    });

    if !allowed {
        tracing::warn!(
            path = %canonical_path.display(),
            "Tool install blocked: path not in allowed directories"
        );

        // Audit log the blocked attempt
        if let Err(e) = state.kernel.audit.append(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: agentos_types::TraceID::new(),
            event_type: agentos_audit::AuditEventType::SecurityViolation,
            agent_id: None,
            task_id: None,
            tool_id: None,
            details: serde_json::json!({
                "action": "tool_install_blocked",
                "requested_path": form.manifest_path,
                "canonical_path": canonical_path.display().to_string(),
                "reason": "path_not_in_allowed_dirs",
            }),
            severity: agentos_audit::AuditSeverity::Warn,
            reversible: false,
            rollback_ref: None,
        }) {
            tracing::error!(error = %e, "Failed to write tool install audit entry");
        }

        return (
            StatusCode::FORBIDDEN,
            format!(
                "Manifest path '{}' is not in an allowed tool directory",
                canonical_path.display()
            ),
        )
            .into_response();
    }

    // Step 3: Verify .toml extension (defense in depth)
    if canonical_path.extension().map(|e| e != "toml").unwrap_or(true) {
        return (
            StatusCode::BAD_REQUEST,
            "Manifest file must have a .toml extension",
        )
            .into_response();
    }

    // Step 4: Read and parse the manifest
    match std::fs::read_to_string(&canonical_path) {
        Ok(content) => match toml::from_str::<agentos_types::ToolManifest>(&content) {
            Ok(manifest) => {
                match state
                    .kernel
                    .tool_registry
                    .write()
                    .await
                    .register(manifest)
                {
                    Ok(_) => axum::response::Redirect::to("/tools").into_response(),
                    Err(e) => (
                        StatusCode::BAD_REQUEST,
                        format!("Failed to register tool: {}", e),
                    )
                        .into_response(),
                }
            }
            Err(e) => (
                StatusCode::BAD_REQUEST,
                format!("Invalid manifest: {}", e),
            )
                .into_response(),
        },
        Err(e) => (
            StatusCode::BAD_REQUEST,
            format!("Failed to read file: {}", e),
        )
            .into_response(),
    }
}
```

**Note on `SecurityViolation`:** Verify this `AuditEventType` variant exists:
```bash
grep "SecurityViolation" crates/agentos-audit/src/
```
If it does not exist, use `AuditEventType::PermissionDenied` or `AuditEventType::RiskEscalation` instead.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-web/src/state.rs` | Add `allowed_tool_dirs: Vec<PathBuf>` field to `AppState` |
| `crates/agentos-web/src/server.rs` | Accept `allowed_tool_dirs` in `WebServer::new()` constructor |
| `crates/agentos-web/src/handlers/tools.rs` | Replace `contains("..")` check with canonicalization + allowlist + `.toml` extension check + audit logging |
| `crates/agentos-cli/src/commands/web.rs` | Pass tool directories from `kernel.data_dir` to `WebServer::new()` |

---

## Dependencies

None -- this phase can be done independently. However, if Phase 08 (kernel dispatch) is done first, the tool install handler would route through `kernel.api_install_tool()` and the path validation would happen in the kernel command handler. Doing it in the web handler adds defense-in-depth.

---

## Test Plan

1. **Allowed path test:** Create a temp directory, add it to `allowed_tool_dirs`, place a valid `.toml` manifest file in it. POST to `/tools` with the path. Verify 302 redirect (success).

2. **Blocked path test:** POST to `/tools` with `manifest_path=/etc/passwd`. Verify 403 Forbidden.

3. **Symlink bypass test:** Create a symlink inside an allowed directory that points to `/etc/passwd`. POST to `/tools` with the symlink path. Verify 403 Forbidden (because `canonicalize()` resolves the symlink to `/etc/passwd` which is outside allowed dirs).

4. **Path traversal test:** POST to `/tools` with `manifest_path=<allowed_dir>/../../../etc/passwd`. Verify 403 Forbidden (canonicalization resolves `..` before allowlist check).

5. **Extension check test:** POST to `/tools` with a path to a `.json` file inside an allowed directory. Verify 400 Bad Request with "must have a .toml extension".

6. **Audit log test:** After a blocked attempt, query the audit log and verify a `SecurityViolation` entry exists with `action: "tool_install_blocked"` and the canonical path.

7. **Non-existent path test:** POST to `/tools` with a path that does not exist. Verify 400 Bad Request ("Cannot resolve manifest path").

---

## Verification

```bash
# Must compile
cargo build -p agentos-web

# Tests pass
cargo test -p agentos-web

# Verify canonicalize is used
grep -n "canonicalize" crates/agentos-web/src/handlers/tools.rs
# Expected: at least 2 matches (requested path and allowed dir)

# Verify allowed_tool_dirs in state
grep -n "allowed_tool_dirs" crates/agentos-web/src/state.rs

# Verify old '.contains("..")' check is removed
grep -c 'contains("..")' crates/agentos-web/src/handlers/tools.rs
# Expected: 0

# Verify .toml extension check
grep -n '"toml"' crates/agentos-web/src/handlers/tools.rs
```

---

## Related

- [[WebUI Security Fixes Plan]] -- Master plan
- [[WebUI Security Fixes Data Flow]] -- Tool install path security flow diagram
