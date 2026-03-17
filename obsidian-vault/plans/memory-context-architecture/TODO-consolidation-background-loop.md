---
title: "TODO: Wire Consolidation Engine Background Loop"
tags:
  - memory
  - kernel
  - next-steps
date: 2026-03-17
status: complete
effort: 2h
priority: high
---

# Wire Consolidation Engine Background Loop

> Start the ConsolidationEngine as a periodic background task so episodic memories are distilled into procedural procedures during kernel operation.

## Why This Phase

`ConsolidationEngine` is fully implemented in `crates/agentos-kernel/src/consolidation.rs` (237 lines) and assigned as a field on `Kernel` (`consolidation_engine: Arc<ConsolidationEngine>`). However, no background task or periodic loop ever calls `consolidation_engine.run(...)`. The consolidation pathway — which distills groups of related episodic entries into `Procedure` records in the `ProceduralStore` — is dead code at runtime. Without this loop, procedural memory never accumulates patterns no matter how many tasks complete.

## Current → Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `ConsolidationEngine::run()` | Never called | Runs periodically in background, triggered 30 min after kernel boot and then every 30 min |
| `ProceduralStore` | Created, empty at startup | Accumulates `Procedure` records as episodic groups are distilled |
| Background task lifecycle | Not spawned | Supervised by `run_loop.rs` background task group with `CancellationToken` |

## Detailed Subtasks

1. Open `crates/agentos-kernel/src/run_loop.rs`
2. Find the section where background tasks are spawned (search for `tokio::spawn` near health monitor, background pool, or event dispatch)
3. Add a consolidation background task after the existing background task spawns:
   ```rust
   // Spawn consolidation background loop (runs every 30 minutes)
   let consolidation_engine = self.consolidation_engine.clone();
   let consolidation_token = shutdown.child_token();
   tokio::spawn(async move {
       let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(1800));
       interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
       loop {
           tokio::select! {
               _ = interval.tick() => {
                   if let Err(e) = consolidation_engine.run_cycle().await {
                       tracing::warn!(error = %e, "Consolidation run failed");
                   }
               }
               _ = consolidation_token.cancelled() => break,
           }
       }
   });
   ```
4. Verify `ConsolidationEngine::run_cycle()` signature in `crates/agentos-kernel/src/consolidation.rs` — confirm it takes no args and returns `Result<ConsolidationReport, AgentOSError>`
5. Confirm `shutdown` token (or equivalent `CancellationToken`) is accessible in the scope where background tasks are spawned

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/run_loop.rs` | Add `tokio::spawn` for consolidation background loop with 30-min interval |

## Dependencies

None — `ConsolidationEngine` and `ProceduralStore` are already wired into `Kernel`. Only the background task launch is missing.

## Test Plan

- `cargo test -p agentos-kernel` — all existing tests must pass
- Confirm `consolidation_engine` field exists on `Kernel`: `grep "consolidation_engine" crates/agentos-kernel/src/kernel.rs`
- Confirm `ConsolidationEngine::run()` is callable with no args by checking its signature in `consolidation.rs`

## Verification

```bash
cargo build --workspace
cargo test --workspace
# Confirm consolidation is now invoked from run_loop:
grep -n "consolidation_engine.run\|consolidation.*spawn" crates/agentos-kernel/src/run_loop.rs
# Expected: at least 1 match
```

## Related

- [[Memory Context Architecture Plan]] — master plan
- [[07-consolidation-pathways]] — original phase spec
- [[audit_report]] — GAP-M02
