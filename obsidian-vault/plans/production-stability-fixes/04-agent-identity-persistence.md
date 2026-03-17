---
title: Agent Identity Persistence
tags:
  - kernel
  - security
  - reliability
  - plan
  - v3
date: 2026-03-17
status: complete
effort: 1d
priority: high
---

# Phase 04 -- Agent Identity Persistence

> Preserve agent UUID and Ed25519 identity across kernel restarts by reusing persisted `AgentProfile` entries when the same agent name reconnects.

---

## Why This Phase

Audit log shows agent "mavrick" got a new UUID after a kernel restart (417d1bc9 -> 9810a79b). This happens because `cmd_connect_agent()` always calls `AgentID::new()` regardless of whether an agent with that name already exists in the persisted `agents.json`. The consequences are:

1. **Memory fragmentation** -- episodic and semantic memories are keyed by `AgentID`. A new UUID means the agent loses all prior context.
2. **Audit trail discontinuity** -- the old UUID's history becomes orphaned.
3. **Event subscription loss** -- all standing subscriptions are tied to the old UUID.
4. **Cost tracker state loss** -- budget usage resets to zero.

The fix is simple: in `cmd_connect_agent()`, check `agent_registry.get_by_name()` before creating a new UUID. If the name matches an existing persisted agent, reuse its `AgentID` and `public_key_hex`.

## Sub-tasks

| # | Task | Files | Detail Doc |
|---|------|-------|------------|
| 06 | Agent identity reuse on reconnect | `commands/agent.rs`, `agent_registry.rs` | [[24-06-Agent Identity Reuse on Reconnect]] |

## Dependencies

Phase 03 (graceful shutdown) should be complete first so that the agent registry is properly persisted on clean shutdown. However, the agent registry already auto-saves on every mutation (`save_to_disk()`), so this is a soft dependency.

## Test Plan

- Unit test: `AgentRegistry::with_persistence()` loads agents from disk and `get_by_name()` returns the persisted agent
- Integration test: connect agent "test-agent", disconnect, reconnect with same name -- the returned `agent_id` is identical
- Test: reconnect with same name but different provider/model -- a new UUID is issued (different agent)

## Verification

```bash
cargo test -p agentos-kernel -- agent
cargo clippy -p agentos-kernel -- -D warnings
```
