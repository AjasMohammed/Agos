---
title: "Phase 08 -- Kernel Dispatch Integration"
tags:
  - webui
  - security
  - plan
  - v3
date: 2026-03-17
status: complete
effort: 6h
priority: high
---

# Phase 08 -- Kernel Dispatch Integration

> Route all web UI mutation handlers through the kernel's command dispatch path so every operation is audit-logged, capability-checked, and follows the same code path as CLI commands.

---

## Why This Phase

Issue S3: The web UI handlers for agents, tools, and secrets bypass kernel command dispatch. They directly call `agent_registry.write().register()` (agents.rs:86), `agent_registry.write().remove()` (agents.rs:98), `tool_registry.write().register()` (tools.rs:86), `tool_registry.write().remove()` (tools.rs:115), `vault.set()` (secrets.rs:64), and `vault.revoke()` (secrets.rs:80). This means:

1. **No audit logging** -- these operations are invisible in the audit trail
2. **No capability checking** -- any authenticated web user can perform any operation
3. **No event emission** -- the event trigger system never fires for these operations
4. **No security enforcement** -- trust tier validation, injection scanning, and scope resolution are skipped

The kernel already has `cmd_connect_agent`, `cmd_disconnect_agent`, `cmd_install_tool`, `cmd_remove_tool`, `cmd_set_secret`, and `cmd_revoke_secret` methods that implement all checks. The web handlers should be thin HTTP adapters that translate form data into kernel calls.

This phase depends on Phase 03 because the auth middleware must be in place before restructuring handlers. Without authentication, there is no identity to bind to kernel operations.

---

## Current -> Target State

| Handler | Current (Direct Registry/Vault) | Target (Kernel Dispatch) |
|---------|--------------------------------|--------------------------|
| `agents::connect` (agents.rs:57-89) | Constructs `AgentProfile` manually; calls `agent_registry.write().register(profile)` directly -- no audit, no event | Route through `kernel.api_connect_agent(name, provider, model, base_url, roles)` which calls `cmd_connect_agent` -- full audit + AgentAdded event |
| `agents::disconnect` (agents.rs:91-103) | Calls `registry.get_by_name(&name)` then `registry.remove(&id)` -- no audit, no AgentRemoved event | Route through `kernel.api_disconnect_agent(agent_id)` which calls `cmd_disconnect_agent` |
| `tools::install` (tools.rs:64-108) | Reads file, parses TOML, calls `tool_registry.write().register(manifest)` -- no audit, no trust tier check | Route through `kernel.api_install_tool(manifest_path)` which calls `cmd_install_tool` -- trust tier + signing validation + audit |
| `tools::remove` (tools.rs:110-119) | Calls `registry.remove(&name)` -- no audit | Route through `kernel.api_remove_tool(name)` |
| `secrets::create` (secrets.rs:50-74) | Calls `vault.set(name, value, ...)` -- no audit, no scope resolution | Route through `kernel.api_set_secret(name, value, scope)` -- audit + scope resolution |
| `secrets::revoke` (secrets.rs:76-84) | Calls `vault.revoke(&name)` -- no audit | Route through `kernel.api_revoke_secret(name)` |

---

## Subtasks

### 1. Expose kernel command methods as public API

**File:** `crates/agentos-kernel/src/kernel.rs`

The kernel's `cmd_*` methods are `pub(crate)`. The web crate holds an `Arc<Kernel>` and needs to call them. Add public wrapper methods. Note that `run_pipeline` is already public (line 505 of `kernel.rs`). Add wrappers for the remaining operations:

```rust
impl Kernel {
    /// Public API: Connect a new agent.
    pub async fn api_connect_agent(
        &self,
        name: String,
        provider: LLMProvider,
        model: String,
        base_url: Option<String>,
        roles: Vec<String>,
    ) -> KernelResponse {
        self.cmd_connect_agent(name, provider, model, base_url, roles).await
    }

    /// Public API: Disconnect an agent by ID.
    pub async fn api_disconnect_agent(&self, agent_id: AgentID) -> KernelResponse {
        self.cmd_disconnect_agent(agent_id).await
    }

    /// Public API: Install a tool from a manifest path.
    pub async fn api_install_tool(&self, manifest_path: String) -> KernelResponse {
        self.cmd_install_tool(manifest_path).await
    }

    /// Public API: Remove a tool by name.
    pub async fn api_remove_tool(&self, tool_name: String) -> KernelResponse {
        self.cmd_remove_tool(tool_name).await
    }

    /// Public API: Set a secret.
    pub async fn api_set_secret(
        &self,
        name: String,
        value: String,
        scope: SecretScope,
    ) -> KernelResponse {
        self.cmd_set_secret(name, value, scope, None).await
    }

    /// Public API: Revoke a secret.
    pub async fn api_revoke_secret(&self, name: String) -> KernelResponse {
        self.cmd_revoke_secret(name).await
    }
}
```

**Note:** Verify the exact signatures of `cmd_connect_agent`, `cmd_install_tool`, `cmd_set_secret`, `cmd_revoke_secret`, etc. by reading `crates/agentos-kernel/src/commands/agent.rs`, `commands/tool.rs`, and `commands/secret.rs`. The wrapper methods must match the parameter types exactly.

### 2. Rewrite `agents::connect` handler

**File:** `crates/agentos-web/src/handlers/agents.rs`

Replace the direct registry call (lines 57-89) with kernel dispatch:

```rust
pub async fn connect(
    State(state): State<AppState>,
    axum::Form(form): axum::Form<ConnectForm>,
) -> Response {
    use agentos_types::LLMProvider;

    let provider = match form.provider.to_lowercase().as_str() {
        "ollama" => LLMProvider::Ollama,
        "openai" => LLMProvider::OpenAI,
        "anthropic" => LLMProvider::Anthropic,
        "gemini" => LLMProvider::Gemini,
        other => LLMProvider::Custom(other.to_string()),
    };

    let response = state
        .kernel
        .api_connect_agent(
            form.name.clone(),
            provider,
            form.model.clone(),
            None,   // base_url
            vec![], // roles (could add form fields for this)
        )
        .await;

    match response {
        agentos_bus::KernelResponse::Success { .. } => {
            axum::response::Redirect::to("/agents").into_response()
        }
        agentos_bus::KernelResponse::Error { message } => {
            (StatusCode::BAD_REQUEST, format!("Failed to connect agent: {}", message))
                .into_response()
        }
        _ => {
            (StatusCode::INTERNAL_SERVER_ERROR, "Unexpected response").into_response()
        }
    }
}
```

### 3. Rewrite `agents::disconnect` handler

**File:** `crates/agentos-web/src/handlers/agents.rs`

```rust
pub async fn disconnect(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    // Look up agent ID by name first (read-only access is fine)
    let agent_id = {
        let registry = state.kernel.agent_registry.read().await;
        registry.get_by_name(&name).map(|a| a.id)
    };

    match agent_id {
        Some(id) => {
            let response = state.kernel.api_disconnect_agent(id).await;
            match response {
                agentos_bus::KernelResponse::Success { .. } => StatusCode::NO_CONTENT,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            }
        }
        None => StatusCode::NOT_FOUND,
    }
}
```

### 4. Rewrite `tools::install` handler

**File:** `crates/agentos-web/src/handlers/tools.rs`

After the path security validation from Phase 06 (canonicalization + allowlist), replace the direct registry call with kernel dispatch. Instead of reading the file and parsing the manifest in the handler, pass the canonical path to the kernel which handles loading, trust tier validation, signing verification, and audit:

```rust
// After path validation (Phase 06: canonicalization + allowlist + .toml check):
let response = state
    .kernel
    .api_install_tool(canonical_path.to_string_lossy().to_string())
    .await;

match response {
    agentos_bus::KernelResponse::Success { .. } => {
        axum::response::Redirect::to("/tools").into_response()
    }
    agentos_bus::KernelResponse::Error { message } => {
        (StatusCode::BAD_REQUEST, format!("Failed to install tool: {}", message))
            .into_response()
    }
    _ => {
        (StatusCode::INTERNAL_SERVER_ERROR, "Unexpected response").into_response()
    }
}
```

This removes the `std::fs::read_to_string()`, `toml::from_str()`, and `tool_registry.write().register()` calls from the handler, pushing them into the kernel command path where they belong.

### 5. Rewrite `tools::remove` handler

**File:** `crates/agentos-web/src/handlers/tools.rs`

```rust
pub async fn remove(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let response = state.kernel.api_remove_tool(name).await;
    match response {
        agentos_bus::KernelResponse::Success { .. } => StatusCode::NO_CONTENT,
        agentos_bus::KernelResponse::Error { .. } => StatusCode::NOT_FOUND,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
```

### 6. Rewrite `secrets::create` handler

**File:** `crates/agentos-web/src/handlers/secrets.rs`

Route through `api_set_secret` for audit logging and scope resolution:

```rust
pub async fn create(
    State(state): State<AppState>,
    axum::Form(mut form): axum::Form<CreateForm>,
) -> Response {
    use agentos_types::SecretScope;
    use agentos_vault::ZeroizingString;

    // Wrap secret value in ZeroizingString immediately (from Phase 05)
    let secret_value = ZeroizingString::new(std::mem::take(&mut form.value));

    // Parse scope (from Phase 01)
    let scope = match form.scope.as_deref() {
        Some("kernel") => SecretScope::Kernel,
        Some("global") | None => SecretScope::Global,
        Some(other) => {
            if let Some(agent_id_str) = other.strip_prefix("agent:") {
                match agent_id_str.parse::<agentos_types::AgentID>() {
                    Ok(id) => SecretScope::Agent(id),
                    Err(_) => {
                        return (StatusCode::BAD_REQUEST,
                            format!("Invalid agent ID in scope: {}", agent_id_str))
                            .into_response();
                    }
                }
            } else if let Some(tool_id_str) = other.strip_prefix("tool:") {
                match tool_id_str.parse::<agentos_types::ToolID>() {
                    Ok(id) => SecretScope::Tool(id),
                    Err(_) => {
                        return (StatusCode::BAD_REQUEST,
                            format!("Invalid tool ID in scope: {}", tool_id_str))
                            .into_response();
                    }
                }
            } else {
                return (StatusCode::BAD_REQUEST,
                    format!("Unrecognized scope: '{}'", other))
                    .into_response();
            }
        }
    };

    // Route through kernel command dispatch for audit logging
    let response = state
        .kernel
        .api_set_secret(
            form.name.clone(),
            secret_value.as_str().to_string(),
            scope,
        )
        .await;

    match response {
        agentos_bus::KernelResponse::Success { .. } => {
            axum::response::Redirect::to("/secrets").into_response()
        }
        agentos_bus::KernelResponse::Error { message } => {
            (StatusCode::BAD_REQUEST, format!("Failed to create secret: {}", message))
                .into_response()
        }
        _ => {
            (StatusCode::INTERNAL_SERVER_ERROR, "Unexpected response").into_response()
        }
    }
    // secret_value dropped here -> memory zeroed
}
```

**Note:** `api_set_secret` takes `value: String`, not `&str`. The `ZeroizingString` must be converted to `String` for the kernel call (`secret_value.as_str().to_string()`). This creates a temporary `String` that is dropped at end of scope. The `ZeroizingString` still zeros its copy. A future improvement could change the kernel command interface to accept `ZeroizingString` directly.

### 7. Rewrite `secrets::revoke` handler

**File:** `crates/agentos-web/src/handlers/secrets.rs`

```rust
pub async fn revoke(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let response = state.kernel.api_revoke_secret(name).await;
    match response {
        agentos_bus::KernelResponse::Success { .. } => StatusCode::NO_CONTENT,
        agentos_bus::KernelResponse::Error { .. } => StatusCode::NOT_FOUND,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
```

### 8. Remove unused direct imports

**Files:** All handler files

After switching to kernel dispatch, remove unused imports:
- `agents.rs`: Remove `use agentos_types::*;` if only the `LLMProvider` is needed (replace with `use agentos_types::LLMProvider;`)
- `tools.rs`: Remove `std::fs::read_to_string` and `toml::from_str` if the handler no longer reads/parses files directly (when combined with Phase 06)
- `secrets.rs`: Remove `SecretOwner` import if no longer constructed in the handler

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/kernel.rs` | Add `api_connect_agent`, `api_disconnect_agent`, `api_install_tool`, `api_remove_tool`, `api_set_secret`, `api_revoke_secret` public wrapper methods |
| `crates/agentos-web/src/handlers/agents.rs` | Replace `connect()` to use `kernel.api_connect_agent()`; replace `disconnect()` to use `kernel.api_disconnect_agent()` |
| `crates/agentos-web/src/handlers/tools.rs` | Replace `install()` to use `kernel.api_install_tool()` (after path validation); replace `remove()` to use `kernel.api_remove_tool()` |
| `crates/agentos-web/src/handlers/secrets.rs` | Replace `create()` to use `kernel.api_set_secret()`; replace `revoke()` to use `kernel.api_revoke_secret()` |

---

## Dependencies

**Requires:** [[03-cors-auth-csp-ratelimit]] must be complete. Auth middleware must be in place so the web layer has authenticated sessions.

**Benefits from:** [[06-tool-install-path-security]] should be done before this phase so the tool install handler has path security in place before the kernel dispatch is wired.

---

## Test Plan

1. **Agent connect audit test:** POST `/agents` to connect a new agent. Query the audit log. Verify an `AgentAdded` (or equivalent) audit entry exists with the agent name.

2. **Agent disconnect audit test:** DELETE `/agents/test-agent`. Query the audit log. Verify an `AgentRemoved` audit entry exists.

3. **Tool install audit test:** POST `/tools` with a valid manifest path (in an allowed directory). Query the audit log. Verify an audit entry exists for the tool registration. Verify the tool's trust tier was validated.

4. **Tool remove audit test:** DELETE `/tools/test-tool`. Query the audit log. Verify a removal audit entry exists.

5. **Secret create audit test:** POST `/secrets` with name and value. Query the audit log. Verify a `SecretsAccessAttempt` (or equivalent) audit entry exists with `action: "set"`.

6. **Secret revoke audit test:** DELETE `/secrets/test-secret`. Query the audit log. Verify a revocation audit entry exists.

7. **Error propagation test:** POST `/agents` with an invalid provider. Verify the handler returns a 400 error with a meaningful message from the kernel (not a panic or 500).

8. **Direct registry access removed test:** Verify no mutation handlers call registries directly:
   ```bash
   grep -n "agent_registry.write()" crates/agentos-web/src/handlers/agents.rs
   grep -n "tool_registry.write()" crates/agentos-web/src/handlers/tools.rs
   grep -n "vault.set(" crates/agentos-web/src/handlers/secrets.rs
   grep -n "vault.revoke(" crates/agentos-web/src/handlers/secrets.rs
   ```
   All should return 0 matches. (Read-only access via `agent_registry.read()` in list handlers is acceptable.)

9. **KernelResponse mapping test:** Verify `KernelResponse::Error` maps to 400 and `KernelResponse::Success` maps to 200 or 302 for each handler.

---

## Verification

```bash
# Must compile
cargo build -p agentos-kernel -p agentos-web

# All tests pass
cargo test -p agentos-kernel -p agentos-web

# Verify public API methods exist on Kernel
grep -n "pub async fn api_" crates/agentos-kernel/src/kernel.rs
# Expected: 6 methods (connect_agent, disconnect_agent, install_tool, remove_tool, set_secret, revoke_secret)

# Verify direct registry writes removed from web handlers
grep -c "agent_registry.write()" crates/agentos-web/src/handlers/agents.rs
# Expected: 0

grep -c "tool_registry.write()" crates/agentos-web/src/handlers/tools.rs
# Expected: 0

grep -c "vault.set(" crates/agentos-web/src/handlers/secrets.rs
# Expected: 0

grep -c "vault.revoke(" crates/agentos-web/src/handlers/secrets.rs
# Expected: 0

# Verify kernel dispatch calls exist
grep -rn "api_connect_agent\|api_install_tool\|api_set_secret" crates/agentos-web/src/handlers/
# Expected: matches in agents.rs, tools.rs, secrets.rs
```

---

## Related

- [[WebUI Security Fixes Plan]] -- Master plan
- [[WebUI Security Fixes Data Flow]] -- Flow diagrams showing kernel dispatch path
- [[03-cors-auth-csp-ratelimit]] -- prerequisite (auth middleware)
- [[06-tool-install-path-security]] -- recommended to complete first (path validation)
