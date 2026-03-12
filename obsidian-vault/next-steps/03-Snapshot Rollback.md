---
title: Checkpoint / Snapshot / Rollback System — Spec #5
tags:
  - next-steps
  - audit
  - rollback
  - spec-5
date: 2026-03-11
status: partial
effort: 8h
priority: medium
spec-ref: "Spec §5 — Immutable Merkle Audit Trail with Checkpoint/Rollback"
---

# Checkpoint / Snapshot / Rollback System

> Addresses the MoltMatch incident — agents acting irreversibly without user awareness, with no way to undo.

---

## Current State

The audit schema already has rollback foundations:
- `AuditEntry.reversible: bool` — marks whether an action can be undone
- `AuditEntry.rollback_ref: Option<String>` — references a snapshot ID

But **no `SnapshotManager` exists** — snapshots are never taken, never stored, and `agentrollback` CLI doesn't exist.

---

## Architecture Overview

```
Agent calls a reversible action (e.g. fs.write)
    ↓
kernel checks ActionRiskLevel via RiskClassifier
    ↓
If Level ≥ 1 and reversible=true:
    SnapshotManager::take_snapshot(task_id, action)
    → stores filesystem diff + context state
    → returns snap_id (e.g. "snap_4821")
    ↓
AuditEntry written with rollback_ref: Some("snap_4821")
    ↓
Action executes
    ↓
On agentrollback --task=<id>:
    SnapshotManager::restore(snap_id)
    → reverses filesystem changes
    → marks task as Rolled Back in audit
```

---

## What Needs to Be Built

### Step 1 — `SnapshotManager` Struct

**New file:** `crates/agentos-kernel/src/snapshot.rs`

```rust
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::sync::RwLock;
use serde::{Deserialize, Serialize};
use agentos_types::TaskID;

/// A single file captured before modification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSnapshot {
    pub path: String,
    pub existed_before: bool,
    pub original_content: Option<Vec<u8>>,  // None if file didn't exist
    pub captured_at: chrono::DateTime<chrono::Utc>,
}

/// A snapshot of system state before a reversible action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub snap_id: String,          // e.g. "snap_4821"
    pub task_id: TaskID,
    pub agent_id: String,
    pub action_type: String,      // e.g. "fs.write"
    pub files: Vec<FileSnapshot>,
    pub taken_at: chrono::DateTime<chrono::Utc>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub restored: bool,
}

pub struct SnapshotManager {
    /// In-memory snapshot store (keyed by snap_id)
    snapshots: RwLock<HashMap<String, Snapshot>>,
    /// Root directory for any on-disk snapshot blobs
    storage_dir: PathBuf,
    /// How long snapshots are retained (default: 72 hours)
    retention_hours: u64,
}

impl SnapshotManager {
    pub fn new(storage_dir: PathBuf, retention_hours: u64) -> Self { ... }

    /// Capture filesystem state before a reversible action.
    /// Returns the snap_id to store in AuditEntry.rollback_ref.
    pub async fn take_snapshot(
        &self,
        task_id: &TaskID,
        agent_id: &str,
        action_type: &str,
        paths: Vec<String>,        // files that will be modified
    ) -> anyhow::Result<String> { ... }

    /// Restore filesystem state from a snapshot.
    pub async fn restore(&self, snap_id: &str) -> anyhow::Result<()> { ... }

    /// Find all snapshots for a given task (for `agentrollback --task=<id>`).
    pub async fn snapshots_for_task(&self, task_id: &TaskID) -> Vec<Snapshot> { ... }

    /// Delete expired snapshots (call from health check loop).
    pub async fn sweep_expired(&self) -> usize { ... }
}
```

**Snap ID generation:**
```rust
fn new_snap_id() -> String {
    format!("snap_{}", uuid::Uuid::new_v4().simple())
}
```

### Step 2 — Wire SnapshotManager into the Kernel

**File:** `crates/agentos-kernel/src/kernel.rs`

```rust
pub struct Kernel {
    // ... existing fields ...
    pub snapshot_manager: Arc<crate::snapshot::SnapshotManager>,
}

// In Kernel::boot():
snapshot_manager: Arc::new(crate::snapshot::SnapshotManager::new(
    data_dir.join("snapshots"),
    72, // hours
)),
```

**File:** `crates/agentos-kernel/src/lib.rs`

```rust
pub mod snapshot;
```

### Step 3 — Pre-Action Snapshot in `task_executor.rs`

**File:** `crates/agentos-kernel/src/task_executor.rs`

After the tool call passes validation but before execution, check if it's reversible and take a snapshot:

```rust
// After risk classification:
let risk = self.risk_classifier.classify(&tool_call.tool_name, &tool_call.payload);
let is_reversible = risk.level >= ActionRiskLevel::Level1Notify;

let snap_id = if is_reversible {
    // Extract file paths from the payload (tool-specific heuristic)
    let paths = extract_affected_paths(&tool_call);
    let sid = self.snapshot_manager
        .take_snapshot(&task.id, task.agent_id.as_str(), &tool_call.tool_name, paths)
        .await
        .ok()
        .map(|s| s);
    sid
} else {
    None
};

// Then execute tool...

// Then write audit entry with snap_id:
audit_entry.reversible = is_reversible;
audit_entry.rollback_ref = snap_id;
```

### Step 4 — CLI: `agentctl rollback`

**New file:** `crates/agentos-cli/src/commands/rollback.rs`

```rust
#[derive(Subcommand)]
pub enum RollbackCommands {
    /// List all snapshots for a task
    List {
        #[arg(long)]
        task: String,
    },
    /// Restore system state to a specific snapshot
    Restore {
        #[arg(long)]
        snap_id: String,
    },
    /// Undo all reversible actions for a task
    Task {
        #[arg(long)]
        task: String,
    },
}
```

**CLI usage:**
```bash
agentctl rollback list --task t_9f3k2
agentctl rollback restore --snap-id snap_4821
agentctl rollback task --task t_9f3k2   # restores all in reverse order
```

### Step 5 — `KernelCommand` Variants

**File:** `crates/agentos-bus/src/message.rs`

```rust
// In KernelCommand:
ListSnapshots { task_id: String },
RestoreSnapshot { snap_id: String },

// In KernelResponse:
SnapshotList(Vec<serde_json::Value>),
SnapshotRestored { snap_id: String },
```

### Step 6 — Audit Chain: `SnapshotRestored` Event

When a rollback is performed, write an immutable audit entry:

```rust
AuditEntry {
    event_type: AuditEventType::SnapshotRestored,
    detail: json!({
        "snap_id": snap_id,
        "task_id": task_id,
        "files_restored": count,
    }),
    reversible: false,   // rollback itself cannot be undone
    rollback_ref: None,
    ..
}
```

---

## Snapshot Retention Policy

| Policy | Value |
|---|---|
| Default retention | 72 hours |
| Max snapshots per task | 50 |
| On retention expiry | `AuditEventType::SnapshotExpired` written |
| On disk | Compressed with `zstd` (optional) |

---

## Reversibility Classification

| Action | Reversible | Notes |
|---|---|---|
| `fs.write` | ✅ Yes | Capture original file content |
| `fs.delete` | ✅ Yes | Capture file content before deletion |
| `fs.read` | ❌ No | Read-only, nothing to undo |
| `email.send` | ❌ No | External side effect, flag as `reversible: false` |
| `net.post` | ❌ No | API call with external state |
| `agent.spawn` | ⚠️ Partial | Can terminate spawned agent |
| `pipeline.start` | ⚠️ Partial | Can cancel if no external effects yet |

---

## Testing Plan

| Test | Verifies |
|---|---|
| `test_snapshot_taken_and_restored` | File content restored after rollback |
| `test_irreversible_action_no_snapshot` | Email send produces no snapshot |
| `test_expired_snapshots_swept` | `sweep_expired()` cleans old entries |
| `test_task_rollback_reverses_all` | All snaps for a task restored in reverse order |
| `test_rollback_audit_entry_written` | `SnapshotRestored` event in audit chain |

---

## Files Changed

| File | Change |
|---|---|
| `crates/agentos-kernel/src/snapshot.rs` | **NEW** — `SnapshotManager` |
| `crates/agentos-kernel/src/lib.rs` | Add `pub mod snapshot;` |
| `crates/agentos-kernel/src/kernel.rs` | Add `snapshot_manager` to `Kernel` struct + boot |
| `crates/agentos-kernel/src/task_executor.rs` | Pre-action snapshot, post-action audit entry |
| `crates/agentos-bus/src/message.rs` | Add `ListSnapshots`, `RestoreSnapshot` commands |
| `crates/agentos-cli/src/commands/rollback.rs` | **NEW** — CLI handler |
| `crates/agentos-cli/src/main.rs` | Add `Rollback` variant to `Commands` |

---

## Related

- [[01-Critical Build Fix]] — AuditEntry fields that back this system
- [[reference/Audit System]] — Existing audit system documentation
- [[Index]] — Back to dashboard