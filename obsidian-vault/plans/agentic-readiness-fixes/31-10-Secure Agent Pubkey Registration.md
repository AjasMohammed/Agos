---
title: "Secure Agent Pubkey Registration"
tags:
  - next-steps
  - security
  - agent-messaging
  - agentic-readiness
date: 2026-03-19
status: planned
effort: 4h
priority: critical
---

# Secure Agent Pubkey Registration

> Lock down `register_pubkey()` to prevent unauthenticated key registration that enables agent impersonation.

## What to Do

`agent_message_bus.rs` has `register_pubkey(agent_id, pubkey)` with **no authentication**. Any component can register an attacker's key for any agent and then forge messages as that agent. This is a critical authentication bypass.

**Attack vector:**
1. Malicious component calls `bus.register_pubkey(alice_id, attacker_pubkey)`
2. Attacker crafts message signed with attacker's private key
3. Signature verifies — message accepted as from Alice
4. Audit log shows Alice as sender

### Steps

1. **Move pubkey registration to kernel boot only:**
   - Remove public `register_pubkey()` from `AgentMessageBus`
   - Add `register_pubkey_internal()` that is only callable from `kernel.rs` during agent registration
   - The kernel already validates agent identity during registration — tie pubkey registration to that flow

2. **Make agent_id → pubkey immutable after registration:**
   - Once a pubkey is set for an agent, reject any attempt to change it
   - Only way to re-register: deregister agent + re-register (audited operation)
   - Add `AgentOSError::PubkeyAlreadyRegistered` variant

3. **Persist keys to vault:**
   - Store `agent_id → pubkey` mapping in the encrypted vault (scope: Kernel)
   - On boot: reload pubkeys from vault
   - This fixes the "keys lost on restart" issue too

4. **Add audit logging:**
   - Log `SecurityEvent::PubkeyRegistered { agent_id, pubkey_fingerprint }` on registration
   - Log `SecurityEvent::PubkeyRegistrationDenied { agent_id, reason }` on rejection

5. **Check expiry before signature verification** (optimization):
   - In `send_message()`, check `message.is_expired()` BEFORE `verify()` to avoid paying crypto cost on expired messages

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/agent_message_bus.rs` | Remove public `register_pubkey`, add `register_pubkey_internal`, make immutable, persist to vault |
| `crates/agentos-kernel/src/kernel.rs` | Call `register_pubkey_internal` during agent registration |
| `crates/agentos-types/src/error.rs` | Add `PubkeyAlreadyRegistered` variant |

## Prerequisites

None — independent security fix.

## Verification

```bash
cargo test -p agentos-kernel
cargo clippy --workspace -- -D warnings
```

Test: register agent with pubkey → attempt to re-register with different pubkey → error. Restart kernel → pubkey still valid. Send message with unregistered key → rejected.
