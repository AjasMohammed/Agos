---
title: "PermissionOp and IntentType Alignment"
tags:
  - next-steps
  - types
  - capability
  - agentic-readiness
date: 2026-03-19
status: planned
effort: 2h
priority: medium
---

# PermissionOp and IntentType Alignment

> Add `Query` and `Observe` to `PermissionOp` to match the `IntentType` variants that have no permission representation.

## What to Do

`PermissionOp` has Read/Write/Execute. `IntentType` has Read/Write/Execute/Query/Observe/Delegate/Message/Broadcast/Escalate/Subscribe/Unsubscribe. The gap means `IntentType::Query` and `IntentType::Observe` have no natural permission mapping — capability tokens can't specifically grant or deny these operations.

### Steps

1. **Add variants** to `PermissionOp` in `crates/agentos-types/src/capability.rs`:
   ```rust
   pub enum PermissionOp {
       Read,
       Write,
       Execute,
       Query,    // NEW — for IntentType::Query, Subscribe, Unsubscribe
       Observe,  // NEW — for IntentType::Observe
   }
   ```

2. **Map IntentType to PermissionOp** — add or update a mapping function:
   - `IntentType::Query` → `PermissionOp::Query`
   - `IntentType::Observe` → `PermissionOp::Observe`
   - `IntentType::Subscribe` / `Unsubscribe` → `PermissionOp::Query`
   - `IntentType::Delegate` → `PermissionOp::Execute`
   - `IntentType::Message` / `Broadcast` → `PermissionOp::Write`
   - `IntentType::Escalate` → `PermissionOp::Execute`

3. **Update permission string parsing** — support `"query"` and `"observe"` in capability strings (e.g., `"memory:query"`)

4. **Update tests** for the new ops and mappings

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-types/src/capability.rs` | Add `Query`, `Observe` to `PermissionOp`, add mapping function |
| Any files matching on `PermissionOp` | Add arms |

## Prerequisites

None.

## Verification

```bash
cargo test -p agentos-types
cargo build --workspace
cargo clippy --workspace -- -D warnings
```
