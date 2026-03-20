---
title: "Audit Hash Chain Verification at Startup"
tags:
  - next-steps
  - security
  - audit
  - agentic-readiness
date: 2026-03-19
status: complete
effort: 3h
priority: high
---

# Audit Hash Chain Verification at Startup

> Verify the SHA256 hash chain integrity of the audit log on kernel startup to detect tampering.

## What to Do

The audit log uses an append-only SQLite database with SHA256 hash chain for tamper detection. However, there is no `verify_chain()` method — the chain is never verified. Tampering goes undetected until a manual audit.

### Steps

1. **Add `verify_chain()` method** to `AuditLog` in `crates/agentos-audit/src/log.rs`:
   ```rust
   pub fn verify_chain(&self) -> Result<AuditChainStatus> {
       // 1. Read all entries ordered by sequence number
       // 2. For each entry, recompute hash from: previous_hash + event_data
       // 3. Compare computed hash with stored hash
       // 4. Return Ok(Valid) or Err with first broken link
   }
   ```

2. **Define `AuditChainStatus` enum:**
   ```rust
   pub enum AuditChainStatus {
       Valid { entries_verified: u64 },
       Broken { at_sequence: u64, expected_hash: String, actual_hash: String },
       Empty,
   }
   ```

3. **Call `verify_chain()` during kernel boot** in `kernel.rs`:
   - Run verification after opening the audit DB
   - If `Broken`: log a critical security warning, emit `SecurityEvent::AuditChainTampered`
   - If `Valid`: log info with count
   - Don't block boot — verification is diagnostic, not a hard gate (operator decides)

4. **Add incremental verification option:**
   - For large audit logs, verify only the last N entries (configurable, default: 1000)
   - Full verification available via CLI command: `agentctl audit verify`

5. **Handle silent audit write failures:**
   - In `log_event()`, if the SQLite write fails, return the error instead of silently dropping
   - Callers should log the error but not crash (audit failure shouldn't kill task execution)

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-audit/src/log.rs` | Add `verify_chain()`, `AuditChainStatus`, fix silent write failures |
| `crates/agentos-kernel/src/kernel.rs` | Call `verify_chain()` at boot |

## Prerequisites

None.

## Verification

```bash
cargo test -p agentos-audit
cargo test -p agentos-kernel
cargo clippy --workspace -- -D warnings
```

Test: write 100 audit entries → verify chain → Valid. Manually corrupt one entry's hash → verify chain → Broken with correct sequence number.
