---
title: Agent Identity Reuse on Reconnect
tags:
  - kernel
  - security
  - reliability
  - next-steps
  - v3
date: 2026-03-17
status: planned
effort: 4h
priority: high
---

# Agent Identity Reuse on Reconnect

> When an agent with an existing name reconnects, reuse its persisted `AgentID` and Ed25519 identity instead of generating new ones, preserving memory history and audit continuity.

---

## Why This Subtask

Currently `cmd_connect_agent()` in `crates/agentos-kernel/src/commands/agent.rs` always calls `AgentID::new()` (line 19), generating a fresh UUID for every connection. The `AgentRegistry` is persisted to `agents.json` (via `with_persistence()` in `agent_registry.rs` line 27), but when a kernel restart causes an agent to re-register with the same name, it gets a new UUID. This breaks:

1. **Episodic memory continuity** -- memories are keyed by `AgentID`
2. **Audit trail linkage** -- old UUID's audit entries become orphaned
3. **Event subscriptions** -- old subscriptions are tied to the old UUID
4. **Cost tracker state** -- budget resets on new UUID

The fix checks `agent_registry.get_by_name()` before creating a new UUID. If a matching agent exists with compatible provider and model, its identity is reused.

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `cmd_connect_agent` UUID | Always `AgentID::new()` | Check `get_by_name()` first; reuse if provider+model match |
| Ed25519 identity | Always `generate_identity()` | Reuse `public_key_hex` from persisted profile if present |
| Status on reconnect | N/A (always new agent) | Set to `AgentStatus::Online`, update `last_active` |
| Provider/model mismatch | N/A | Treat as new agent (new UUID), log the override |

## What to Do

1. Open `crates/agentos-kernel/src/commands/agent.rs`

2. At the beginning of `cmd_connect_agent()`, before line 19 (`let agent_id = AgentID::new();`), check for an existing agent with the same name:

```rust
// Check if an agent with this name already exists (e.g., reconnecting after restart).
// Reuse the existing AgentID to preserve memory, audit, and subscription continuity.
let existing = {
    let registry = self.agent_registry.read().await;
    registry.get_by_name(&name).cloned()
};

let (agent_id, reusing_identity) = if let Some(ref existing_agent) = existing {
    // Only reuse identity if the provider and model match.
    // A different provider/model means this is effectively a new agent
    // that happens to share a name.
    if existing_agent.provider == provider && existing_agent.model == model {
        tracing::info!(
            agent_id = %existing_agent.id,
            name = %name,
            "Reusing persisted agent identity on reconnect"
        );
        (existing_agent.id, true)
    } else {
        tracing::info!(
            name = %name,
            old_provider = ?existing_agent.provider,
            new_provider = ?provider,
            "Agent name reused with different provider/model, creating new identity"
        );
        // Remove the old entry to avoid name conflicts
        let mut registry = self.agent_registry.write().await;
        registry.remove(&existing_agent.id);
        drop(registry);
        (AgentID::new(), false)
    }
} else {
    (AgentID::new(), false)
};
```

3. Update the Ed25519 identity generation block (lines 113-122) to skip generation when reusing:

```rust
let public_key_hex = if reusing_identity {
    // Reuse the persisted Ed25519 public key
    existing.as_ref().and_then(|a| a.public_key_hex.clone())
} else {
    match self.identity_manager.generate_identity(&agent_id).await {
        Ok(pk) => {
            tracing::info!(agent_id = %agent_id, "Generated Ed25519 identity for agent");
            Some(pk)
        }
        Err(e) => {
            tracing::warn!(agent_id = %agent_id, error = %e, "Failed to generate agent identity");
            None
        }
    }
};
```

4. When reusing identity, also re-register the public key with the message bus (the bus clears state on kernel restart):

```rust
if let Some(ref pk) = public_key_hex {
    self.message_bus.register_pubkey(agent_id, pk.clone()).await;
}
```
(This block already exists at lines 125-127, so no change is needed here.)

5. When building the `AgentProfile` struct, preserve the `created_at` timestamp from the existing agent if reusing:

```rust
let profile = AgentProfile {
    id: agent_id,
    name,
    provider,
    model,
    status: AgentStatus::Online,
    permissions: if reusing_identity {
        existing.as_ref().map(|a| a.permissions.clone()).unwrap_or_default()
    } else {
        PermissionSet::new()
    },
    roles: resolved_roles,
    current_task: None,
    description: String::new(),
    created_at: if reusing_identity {
        existing.as_ref().map(|a| a.created_at).unwrap_or(now)
    } else {
        now
    },
    last_active: now,
    public_key_hex,
};
```

6. Add an `AgentRegistry` helper method to make lookups cleaner. In `crates/agentos-kernel/src/agent_registry.rs`, the `get_by_name()` method already returns `Option<&AgentProfile>` (line 121-123) -- no change needed.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/commands/agent.rs` | Check for existing agent before creating new UUID; reuse identity when provider+model match |

## Prerequisites

[[24-05-Graceful Shutdown Audit Trail]] should be complete so that the agent registry is properly persisted on shutdown. This is a soft dependency -- the registry auto-saves on every mutation already.

## Test Plan

- `cargo test -p agentos-kernel -- agent` passes
- Add unit test in `agent_registry.rs`: register agent "alice", retrieve by name, confirm ID matches
- Add integration-style test: create a profile, register it, call `get_by_name()` -- confirm the returned profile has the same `id`, `provider`, `model`, and `public_key_hex`
- Edge case test: register "alice" with Ollama, then connect "alice" with OpenAI -- confirm a new UUID is generated (not reused)

## Verification

```bash
cargo build -p agentos-kernel
cargo test -p agentos-kernel -- agent --nocapture
cargo clippy -p agentos-kernel -- -D warnings
```
