---
title: "Proxy Token Invalidation on Secret Rotation"
tags:
  - next-steps
  - security
  - vault
  - agentic-readiness
date: 2026-03-19
status: planned
effort: 2h
priority: high
---

# Proxy Token Invalidation on Secret Rotation

> When a secret is rotated in the vault, immediately invalidate all outstanding proxy tokens that reference the old value.

## What to Do

The vault's proxy token system (one-time use, 5-second TTL) is well-designed. However, when a secret is rotated via `rotate_secret()`, existing proxy tokens still resolve to the old value until they expire. During this race window, an agent could use a stale secret.

### Steps

1. **Track which secret a proxy token references** in the vault's proxy token store:
   - Add `secret_name: String` field to the proxy token metadata (in-memory `HashMap`)
   - When creating a proxy token, record which secret it's for

2. **On `rotate_secret()`:**
   - After updating the secret value, iterate all outstanding proxy tokens
   - Revoke any token whose `secret_name` matches the rotated secret
   - Log `SecurityEvent::ProxyTokensRevokedOnRotation { secret_name, count }`

3. **On `delete_secret()`:**
   - Same invalidation — revoke all proxy tokens for that secret name

4. **Add test** — create proxy token → rotate secret → attempt to redeem proxy token → error

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-vault/src/lib.rs` | Add `secret_name` tracking to proxy tokens, invalidate on rotation/deletion |

## Prerequisites

None.

## Verification

```bash
cargo test -p agentos-vault
cargo clippy --workspace -- -D warnings
```
