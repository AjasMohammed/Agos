---
title: "Phase 7: Bus Dispatch Migration"
tags:
  - api
  - kernel
  - bus
  - v3
  - phase-7
date: 2026-03-30
status: complete
effort: 2d
priority: medium
---

# Phase 7: Bus Dispatch Migration

> Migrate the kernel's bus command dispatch (`run_loop.rs`) from internal `cmd_*()` methods to `KernelService` trait calls, ensuring CLI and API share the same codepath.

---

## Why This Phase

After Phases 1-6, we have three codepaths doing the same thing:
1. REST/WebSocket → `KernelService` → kernel internals
2. HTML handlers → `KernelService` → kernel internals
3. CLI → bus → `run_loop.rs` → `cmd_*()` → kernel internals directly

Path 3 still bypasses the `KernelService` trait, meaning bug fixes in the service layer don't apply to CLI users. This phase unifies all three paths through the trait.

## Current State

- `crates/agentos-kernel/src/run_loop.rs` has a `handle_command()` method with a ~96-arm match statement
- Each arm calls a `cmd_*()` private method (e.g., `cmd_connect_agent()`, `cmd_list_tasks()`)
- These `cmd_*()` methods are in `crates/agentos-kernel/src/commands/*.rs`
- `KernelService` trait exists with matching methods (Phase 1)
- `impl KernelService for Kernel` exists in `agentos-api/src/kernel_impl.rs`

## Target State

- `handle_command()` match arms call `KernelService` methods instead of `cmd_*()` for migrated commands
- Response conversion: `KernelService` returns API DTOs → converted to `KernelResponse` for bus transport
- `cmd_*()` methods gradually removed as each is replaced
- Commands not yet on `KernelService` (roles, HAL, event subscriptions, etc.) keep using `cmd_*()` — no forced migration

## Detailed Subtasks

### 1. Add KernelService as a self-reference in run_loop

The kernel already implements `KernelService` via `impl KernelService for Kernel`. In the run loop, we can call `self` as a `KernelService`:

```rust
// crates/agentos-kernel/src/run_loop.rs

use agentos_api::KernelService;

impl Kernel {
    async fn handle_command(&self, cmd: KernelCommand) -> KernelResponse {
        match cmd {
            // --- Migrated to KernelService ---
            KernelCommand::ListAgents => {
                match KernelService::list_agents(self).await {
                    Ok(agents) => {
                        // Convert Vec<AgentSummary> → KernelResponse::AgentList
                        let profiles = agents.iter().map(|a| AgentProfile::from(a)).collect();
                        KernelResponse::AgentList(profiles)
                    }
                    Err(e) => KernelResponse::Error { message: e.to_string() },
                }
            }

            KernelCommand::ConnectAgent { name, provider, model, base_url, roles, .. } => {
                let req = ConnectAgentRequest { name, provider: format!("{:?}", provider), model, base_url, roles };
                match KernelService::connect_agent(self, req).await {
                    Ok(_) => KernelResponse::Success { data: None },
                    Err(e) => KernelResponse::Error { message: e.to_string() },
                }
            }

            // ... etc for migrated commands

            // --- Not yet migrated (keep existing dispatch) ---
            KernelCommand::CreateRole { .. } => self.cmd_create_role(/*...*/).await,
            KernelCommand::HalListDevices => self.cmd_hal_list_devices().await,
            // ...
        }
    }
}
```

### 2. Response conversion layer

The bus protocol uses `KernelResponse` variants with internal types (`AgentProfile`, `TaskSummary` from bus, `ToolManifest`, etc.). The `KernelService` returns API DTOs. We need converters:

**New file: `crates/agentos-kernel/src/commands/response_convert.rs`**

```rust
use agentos_api::types::*;
use agentos_bus::*;

/// Convert API AgentSummary → bus AgentProfile
impl From<&AgentSummary> for AgentProfile {
    fn from(a: &AgentSummary) -> Self {
        AgentProfile {
            id: a.id,
            name: a.name.clone(),
            provider: parse_provider(&a.provider),
            model: a.model.clone(),
            roles: a.roles.clone(),
            connected_at: a.connected_at,
        }
    }
}

/// Convert API TaskSummary → bus TaskSummary
impl From<&api_types::TaskSummary> for bus_types::TaskSummary { ... }

/// Convert API ToolSummary → bus ToolManifest (subset)
impl From<&ToolSummary> for ToolManifest { ... }

// etc. for each type that crosses the boundary
```

### 3. Migrate commands in batches

**Batch 1 — Agent commands (already have service methods):**
- `ConnectAgent` → `service.connect_agent()`
- `ListAgents` → `service.list_agents()`
- `DisconnectAgent` → `service.disconnect_agent()`

**Batch 2 — Task commands:**
- `ListTasks` → `service.list_tasks()`
- `CancelTask` → `service.cancel_task()`
- `TaskGetTrace` → `service.get_task_trace()`
- `RunTask` → `service.run_task()`

**Batch 3 — Tool commands:**
- `ListTools` → `service.list_tools()`
- `InstallTool` → `service.install_tool()`
- `RemoveTool` → `service.remove_tool()`

**Batch 4 — Secret commands:**
- `ListSecrets` → `service.list_secrets()`
- `SetSecret` → `service.set_secret()`
- `RevokeSecret` → `service.revoke_secret()`

**Batch 5 — Remaining covered commands:**
- `GetAuditLogs` → `service.query_audit()`
- `GetCostReport` → `service.get_cost_summary()`
- `ListNotifications` → `service.list_notifications()`
- `GetStatus` → `service.get_status()`
- `PipelineList` → `service.list_pipelines()`
- `RunPipeline` → `service.run_pipeline()`

**NOT migrated (no service method yet):**
- Role management (7 commands)
- Permission profiles (5 commands)
- Agent messaging (4 commands)
- Scheduling (7 commands)
- Resource locks (3 commands)
- Snapshots (2 commands)
- HAL devices (5 commands)
- Event system (7 commands)
- Context memory (6 commands)
- Scratchpad (4 commands)
- Misc: `SetLogLevel`, `VaultLockdown`, `IdentityShow`, `VerifyAuditChain`, etc.

These keep using `cmd_*()` until their service methods are added on demand.

### 4. Remove migrated cmd_* methods

After each batch is verified, remove the corresponding `cmd_*()` method from `commands/*.rs`. If the method is only called from `handle_command()`, it's safe to delete.

Check each with:
```bash
cargo grep "cmd_connect_agent" --  # ensure no other callers
```

### 5. Add agentos-api dependency to kernel

**Edit: `crates/agentos-kernel/Cargo.toml`**

```toml
agentos-api = { path = "../agentos-api" }
```

Note: This creates a cycle concern — `agentos-api` depends on `agentos-kernel` (for `impl KernelService for Kernel`), and now `agentos-kernel` depends on `agentos-api` (for DTO types in run_loop).

**Resolution:** Extract the `KernelService` trait and DTOs into a separate `agentos-api-types` crate, or use the trait via a feature flag. The simplest approach:
- `agentos-api` has the trait + types + REST/WS (depends on kernel)
- `agentos-kernel` uses `agentos-api`'s types via a `types-only` feature that doesn't pull in the kernel impl

Alternative: define the response conversion in `agentos-api` instead of `agentos-kernel`, and have the run_loop call through an `Arc<dyn KernelService>` stored on the kernel struct.

**Recommended approach:**
```rust
// In Kernel struct, add:
pub service_self: Arc<dyn KernelService>,

// In Kernel::boot():
let kernel = Arc::new(kernel_inner);
// Store self-reference for bus dispatch
kernel.service_self = kernel.clone() as Arc<dyn KernelService>;
```

Then `handle_command()` calls `self.service_self.list_agents().await` — no circular dependency needed. The run_loop only needs the API DTO types, which can be re-exported from `agentos-types` if we want to avoid the cycle entirely.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/run_loop.rs` | Migrate match arms to KernelService calls |
| `crates/agentos-kernel/src/commands/response_convert.rs` | API DTO → bus response converters |
| `crates/agentos-kernel/src/commands/agent.rs` | Remove migrated `cmd_*` methods |
| `crates/agentos-kernel/src/commands/task.rs` | Remove migrated `cmd_*` methods |
| `crates/agentos-kernel/src/commands/tool.rs` | Remove migrated `cmd_*` methods |
| `crates/agentos-kernel/src/commands/secret.rs` | Remove migrated `cmd_*` methods |
| `crates/agentos-kernel/src/kernel.rs` | Add `service_self: Arc<dyn KernelService>` if needed |
| `crates/agentos-kernel/Cargo.toml` | Potentially add agentos-api types dependency |

## Dependencies

- **Requires:** Phase 1 (KernelService trait + impl exist)
- **Blocks:** Nothing (this is an optimization/unification phase)
- **Can run in parallel with:** Phases 4, 5, 6 (independent work)

## Test Plan

1. **CLI smoke tests:** `agentctl agent list`, `task list`, `tool list`, `secret list` — all still work
2. **CLI CRUD:** connect agent → list → disconnect via CLI — same behavior
3. **Bus round-trip:** send `KernelCommand::ListAgents` via bus client → receive `KernelResponse::AgentList` — same format
4. **Response compatibility:** verify bus responses haven't changed format (CLI parsers expect specific shapes)
5. **Workspace tests:** `cargo test --workspace` all green
6. **No regressions:** existing CLI integration tests pass

## Verification

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check

# Verify CLI still works
cargo run -p agentos-cli -- agent list
cargo run -p agentos-cli -- tool list
cargo run -p agentos-cli -- status
```
