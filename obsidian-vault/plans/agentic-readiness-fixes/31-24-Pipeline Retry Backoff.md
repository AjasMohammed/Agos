---
title: "Pipeline Retry with Exponential Backoff"
tags:
  - next-steps
  - pipeline
  - reliability
  - agentic-readiness
date: 2026-03-19
status: planned
effort: 2h
priority: medium
---

# Pipeline Retry with Exponential Backoff

> Replace immediate retry on pipeline step failure with exponential backoff to avoid hammering failing tools/services.

## What to Do

Pipeline step retries happen immediately on failure. If a step fails due to a transient issue (rate limit, network timeout), the immediate retry will likely fail too, wasting budget and potentially triggering rate limits.

### Steps

1. **Add backoff logic** to the retry loop in `crates/agentos-pipeline/src/engine.rs`:
   ```rust
   let delay = Duration::from_millis(500 * 2u64.pow(attempt as u32));
   let max_delay = Duration::from_secs(30);
   tokio::time::sleep(delay.min(max_delay)).await;
   ```

2. **Add retry config to pipeline step definition:**
   ```yaml
   steps:
     - id: fetch-data
       tool: http-client
       max_attempts: 3
       retry_backoff_ms: 500  # Initial backoff, doubles each attempt
       retry_max_delay_ms: 30000
   ```

3. **Add jitter** to prevent thundering herd:
   - Add random jitter of ±25% to the computed delay
   - Use `rand::thread_rng().gen_range(0.75..=1.25)`

4. **Log retry attempts** with attempt number, delay, and error

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-pipeline/src/engine.rs` | Add exponential backoff with jitter to retry loop |

## Prerequisites

None.

## Verification

```bash
cargo test -p agentos-pipeline
cargo clippy --workspace -- -D warnings
```

Test: step fails 2 times then succeeds → verify delays between retries increase. Step fails all attempts → verify final failure includes all attempt errors.
