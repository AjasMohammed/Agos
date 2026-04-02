---
title: "Phase 3.2: Benchmarks & Performance"
tags:
  - kernel
  - benchmarks
  - v3
  - plan
  - phase-3
date: 2026-03-30
status: planned
effort: 2d
priority: medium
---

# Phase 3.2: Benchmarks & Performance

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Establish published performance baselines using Criterion benchmarks with CI regression gating.

**Architecture:** Criterion micro-benchmarks in `crates/agentos-kernel/benches/` measuring cold start, routing throughput, memory scaling, tool execution latency, and audit write throughput. CI runs benchmarks on PRs and blocks merge on >5% regression.

**Tech Stack:** criterion (benchmarking), sysinfo (RSS measurement), GitHub Actions

---

## Why This Phase

OpenFang publishes 180ms cold start, 2,400 tasks/sec, 40MB idle. AgentOS has no published numbers. Without benchmarks, claims of performance are unverifiable and the project loses credibility against competitors with real data.

## Current → Target State

**Current:** No benchmarks. No performance data.

**Target:** 8 benchmarks, CI integration, published comparison table.

## Files Changed

| File | Action | Purpose |
|------|--------|---------|
| `crates/agentos-kernel/Cargo.toml` | Modify | Add criterion dev-dependency |
| `crates/agentos-kernel/benches/bench_cold_start.rs` | Create | Kernel boot latency |
| `crates/agentos-kernel/benches/bench_routing.rs` | Create | Task dispatch throughput |
| `crates/agentos-kernel/benches/bench_memory.rs` | Create | RSS per agent scaling |
| `crates/agentos-kernel/benches/bench_tool_exec.rs` | Create | Sandbox spawn latency |
| `crates/agentos-kernel/benches/bench_audit.rs` | Create | Audit log write throughput |
| `.github/workflows/bench.yml` | Create | CI benchmark workflow |
| `docs/benchmarks.md` | Create | Published results |

## Dependencies

- **Requires:** Phase 1.1 (REST API — for HTTP benchmarks), Phase 3.1 (Single binary — for binary size measurement)
- **Blocks:** Nothing

---

## Detailed Tasks

### Task 1: Add Criterion and Write Cold Start Benchmark

- [ ] Add to `crates/agentos-kernel/Cargo.toml`:
```toml
[dev-dependencies]
criterion = { version = "0.5", features = ["html_reports"] }

[[bench]]
name = "bench_cold_start"
harness = false

[[bench]]
name = "bench_routing"
harness = false

[[bench]]
name = "bench_audit"
harness = false
```

- [ ] Write `bench_cold_start.rs`:
```rust
use criterion::{criterion_group, criterion_main, Criterion};
use std::time::Instant;

fn bench_kernel_boot(c: &mut Criterion) {
    c.bench_function("kernel_cold_start", |b| {
        b.iter_custom(|iters| {
            let mut total = std::time::Duration::ZERO;
            for _ in 0..iters {
                let start = Instant::now();
                // Boot kernel with mock LLM, temp dirs
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async {
                    let tmp = tempfile::TempDir::new().unwrap();
                    // Kernel::boot() with minimal config
                    // Measure time to first health check response
                });
                total += start.elapsed();
            }
            total
        });
    });
}

criterion_group!(benches, bench_kernel_boot);
criterion_main!(benches);
```

- [ ] Run: `cargo bench -p agentos-kernel --bench bench_cold_start`
- [ ] Commit

### Task 2: Routing Throughput Benchmark

- [ ] Write `bench_routing.rs` measuring tasks dispatched per second with MockLLM
- [ ] Use `criterion::Throughput::Elements` for tasks/sec reporting
- [ ] Target: ≥2,500 tasks/sec
- [ ] Commit

### Task 3: Audit Write Benchmark

- [ ] Write `bench_audit.rs` measuring audit log appends per second
- [ ] Use tempfile for isolated SQLite DB
- [ ] Target: ≥10,000 appends/sec
- [ ] Commit

### Task 4: CI Benchmark Workflow

- [ ] Write `.github/workflows/bench.yml` that runs on PRs to main
- [ ] Uses `critcmp` to compare against baseline
- [ ] Posts results as PR comment
- [ ] Fails if any metric regresses >5%
- [ ] Commit

### Task 5: Published Results Page

- [ ] Write `docs/benchmarks.md` with methodology, hardware specs, and comparison table
- [ ] Include reproducible commands
- [ ] Commit

## Verification

```bash
cargo bench -p agentos-kernel
# Check HTML reports in target/criterion/
```
