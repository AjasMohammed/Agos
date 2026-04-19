---
title: "Phase 3: Managed Storage Zones"
tags:
  - kernel
  - capabilities
  - security
  - v4
  - phase-3
date: 2026-04-12
status: planned
effort: 2d
priority: high
---

# Phase 3: Managed Storage Zones (`storage.*`)

> Expand agent filesystem access beyond `data_dir` through policy-controlled, audited storage zones — without removing path traversal protections or Landlock enforcement.

---

## Why This Phase

Agents are currently confined to `data_dir` (plus `workspace_paths` from config). For software engineering, an agent needs to read and write files in the user's actual project directory — `/home/user/projects/myapp/src/main.rs`, not `/opt/agentos/data/agents/<id>/copy-of-main.rs`.

The current workaround (`workspace_paths` in config) is static — set at kernel startup, applies to all agents equally. Storage zones are dynamic, per-agent, audited, and policy-controlled.

---

## Current State

- `ToolExecutionContext.data_dir` — per-agent data directory (`traits.rs:40`)
- `ToolExecutionContext.workspace_paths` — additional allowed paths from config (`traits.rs:59`)
- Landlock restricts writes to `data_dir` only (`crates/agentos-sandbox/src/executor.rs`)
- Path traversal blocked — any path containing `..` is rejected by file tools
- File tools (reader, writer, editor) check paths against allowed list

## Target State

- `StorageProvider` implements `CapabilityProvider` for domain `"storage"`
- Agents can request access to specific directories (subject to policy)
- Zone grants are per-agent, time-limited, auditable, revocable
- Landlock rules expand dynamically when zones are granted
- Zone policy configurable: path allowlist patterns, quota limits

---

## Detailed Subtasks

### 1. Define zone model

**File:** `crates/agentos-kernel/src/managed_storage.rs` (new)

```rust
use agentos_types::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A filesystem zone granting an agent access to a specific directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageZone {
    pub zone_id: String,
    pub agent_id: AgentID,
    pub path: PathBuf,
    pub access: ZoneAccess,
    pub quota_bytes: Option<u64>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub granted_by: ZoneGrantSource,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum ZoneAccess {
    ReadOnly,
    ReadWrite,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ZoneGrantSource {
    /// Granted by policy (path matched allowlist pattern).
    Policy,
    /// Granted by operator approval via escalation.
    OperatorApproval { escalation_id: u64 },
    /// Granted at task start via config workspace_paths.
    Config,
}
```

### 2. Implement `StorageProvider`

Actions:
- **`zone.create`** — Request access to a directory:
  1. Canonicalize path (resolve symlinks, reject `..`)
  2. Check path against zone policy patterns (e.g., `/home/*/projects/**` allowed)
  3. If policy matches: grant zone immediately
  4. If policy requires approval: create `PendingEscalation`
  5. If policy denies: return `CapabilityDenied` with reason
  6. Add zone to agent's zone table
  7. Expand file tool path checking to include new zone
  8. Audit: `StorageZoneCreated`

- **`zone.list`** — List agent's active zones

- **`zone.revoke`** — Remove a zone grant:
  1. Remove from zone table
  2. Audit: `StorageZoneRevoked`

### 3. Zone policy configuration

**File:** `config/default.toml` (add section)

```toml
[capabilities.storage]
# Path patterns that agents may request access to without operator approval.
# Uses glob syntax. Paths not matching any pattern require escalation.
allowed_zone_patterns = [
    "/home/*/projects/**",
    "/tmp/agentos-*/**",
]

# Paths that are NEVER accessible, regardless of policy or approval.
# Takes absolute precedence (deny > allow).
denied_zone_patterns = [
    "/etc/**",
    "/root/**",
    "/home/*/.ssh/**",
    "/home/*/.gnupg/**",
    "/home/*/.aws/**",
    "/var/**",
    "/usr/**",
    "/boot/**",
]

# Default quota per zone (0 = unlimited)
default_zone_quota_bytes = 0

# Maximum number of active zones per agent
max_zones_per_agent = 10
```

### 4. Integrate with file tools

**File:** `crates/agentos-tools/src/file_reader.rs`, `file_writer.rs`, `file_editor.rs`

The path validation logic in file tools currently checks `data_dir` and `workspace_paths`. Extend it to also check the agent's active storage zones:

```rust
// Existing: check data_dir and workspace_paths
// New: also check active storage zones via capability_registry
fn is_path_allowed(path: &Path, context: &ToolExecutionContext) -> bool {
    // 1. Always allow data_dir
    if path.starts_with(&context.data_dir) { return true; }
    // 2. Check static workspace_paths
    if context.workspace_paths.iter().any(|wp| path.starts_with(wp)) { return true; }
    // 3. Check dynamic storage zones (new)
    if let Some(ref registry) = context.capability_registry {
        if registry.is_path_in_zone(&context.agent_id, path) { return true; }
    }
    false
}
```

### 5. Convenience tools

- `storage-zone-create` — `{ "path": "/home/user/projects/myapp", "access": "rw" }`
- `storage-zone-list` — List active zones for this agent
- `storage-zone-revoke` — `{ "zone_id": "..." }`

### 6. Tool manifests

- `storage-zone-create.toml` — `risk_class = "write_scoped"`, permissions: `storage.zone.create:x`
- `storage-zone-list.toml` — `risk_class = "readonly_scoped"`, permissions: `storage.zone.list:r`
- `storage-zone-revoke.toml` — `risk_class = "write_scoped"`, permissions: `storage.zone.revoke:x`

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/managed_storage.rs` | NEW — `StorageZone`, `StorageProvider` |
| `crates/agentos-kernel/src/lib.rs` | Add `pub mod managed_storage;` |
| `crates/agentos-kernel/src/kernel.rs` | Register `StorageProvider` at boot |
| `crates/agentos-tools/src/storage_tools.rs` | NEW — 3 convenience tools |
| `crates/agentos-tools/src/file_reader.rs` | Extend path validation for zones |
| `crates/agentos-tools/src/file_writer.rs` | Extend path validation for zones |
| `crates/agentos-tools/src/file_editor.rs` | Extend path validation for zones |
| `crates/agentos-tools/src/factory.rs` | Register storage tools |
| `config/default.toml` | Add `[capabilities.storage]` section |
| `tools/core/storage-zone-*.toml` | NEW — 3 manifests |

---

## Dependencies

- **Requires:** Phase 1 (capability provider trait)
- **Blocks:** Phase 6 (builds need storage zones for project directories)

---

## Test Plan

- [ ] Zone creation succeeds for paths matching `allowed_zone_patterns`
- [ ] Zone creation for denied paths returns error (never allowed, even with approval)
- [ ] Zone creation for paths not matching any pattern creates escalation
- [ ] Path traversal in zone path (`../`) rejected at canonicalization step
- [ ] Symlink following in zone path works (resolved to real path)
- [ ] File tools (reader, writer, editor) can access files within active zones
- [ ] File tools deny access to paths outside zones
- [ ] Zone revocation removes access — file tools reject subsequent access
- [ ] Zone quota enforcement: writes fail when quota exceeded
- [ ] Per-agent isolation: agent B cannot use agent A's zones
- [ ] `max_zones_per_agent` enforced
- [ ] Audit events: `StorageZoneCreated`, `StorageZoneRevoked`

---

## Verification

```bash
cargo build --workspace
cargo test -p agentos-kernel -- managed_storage
cargo test -p agentos-tools -- storage_tools
cargo test -p agentos-tools -- file_reader
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```

---

## Related

- [[01-capability-provider-trait]] — prerequisite
- [[06-managed-builds]] — builds use storage zones for project access
- [[Kernel Mediated Capabilities Plan]]
