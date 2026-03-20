---
title: "Add Missing HardwareResource Variants"
tags:
  - next-steps
  - types
  - hal
  - agentic-readiness
date: 2026-03-19
status: complete
effort: 1h
priority: high
---

# Add Missing HardwareResource Variants

> Add GPU, Storage, and Sensor variants to `HardwareResource` enum so agents can target all HAL devices via intents.

## What to Do

`HardwareResource` in `agentos-types/src/intent.rs` only has 4 variants: System, Process, Network, LogReader. But the HAL has 7 drivers including GPU, Storage, and Sensor. Agents cannot target these devices via intents.

### Steps

1. **Add variants** to `HardwareResource` in `crates/agentos-types/src/intent.rs`:
   ```rust
   pub enum HardwareResource {
       System,
       Process,
       Network,
       LogReader,
       Gpu,       // NEW
       Storage,   // NEW
       Sensor,    // NEW
   }
   ```

2. **Update any `match` statements** on `HardwareResource` throughout the codebase (search for exhaustive matches)

3. **Map to HAL driver names** — ensure the string representation matches what the HAL registry expects (e.g., `"gpu"`, `"storage"`, `"sensor"`)

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-types/src/intent.rs` | Add `Gpu`, `Storage`, `Sensor` to `HardwareResource` |
| Any files matching on `HardwareResource` | Add match arms |

## Prerequisites

None.

## Verification

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
```
