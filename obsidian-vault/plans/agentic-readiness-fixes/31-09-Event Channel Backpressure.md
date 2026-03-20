---
title: "Event Channel Backpressure"
tags:
  - next-steps
  - kernel
  - events
  - agentic-readiness
date: 2026-03-19
status: planned
effort: 3h
priority: high
---

# Event Channel Backpressure

> Increase event broadcast channel capacity and add backpressure signaling to prevent silent event loss under load.

## What to Do

The event dispatch uses a `tokio::sync::broadcast` channel with capacity 64. Under heavy load (10+ agents, frequent tool calls), events are silently dropped. Agents may miss events they subscribed to.

### Steps

1. **Increase default channel capacity** in `crates/agentos-kernel/src/event_dispatch.rs`:
   - Change from 64 to 1024 (configurable)
   - Add to config:
     ```toml
     [kernel.events]
     channel_capacity = 1024
     ```

2. **Add lag detection:**
   - `broadcast::Receiver::recv()` returns `RecvError::Lagged(n)` when messages are dropped
   - Log a warning with the number of missed events
   - Emit a `SystemEvent::EventsDropped { count: u64, subscriber_id }` event (don't recurse — emit directly to audit log, not through the event system)

3. **Add event processing metrics:**
   - Track `events_emitted`, `events_dropped`, `events_processed` as atomic counters
   - Expose via a health endpoint or introspection tool

4. **Consider bounded mpsc as alternative** for critical subscribers:
   - For subscribers that cannot tolerate event loss (e.g., audit log), use a dedicated `mpsc` channel instead of broadcast
   - Keep broadcast for best-effort subscribers

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/event_dispatch.rs` | Increase capacity, add lag detection, add metrics |
| `crates/agentos-kernel/src/run_loop.rs` | Handle lag errors in event receiver |
| `config/default.toml` | Add `[kernel.events]` section |

## Prerequisites

None.

## Verification

```bash
cargo test -p agentos-kernel
cargo clippy --workspace -- -D warnings
```

Test: emit 2000 events rapidly with a slow subscriber → verify lag warning logged, no silent loss.
