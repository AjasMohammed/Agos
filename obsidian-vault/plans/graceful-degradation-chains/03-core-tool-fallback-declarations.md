---
title: "Phase 3: Core Tool Fallback Declarations"
tags:
  - tools
  - resilience
  - v4
  - plan
date: 2026-04-07
status: planned
effort: 0.5d
priority: medium
---

# Phase 3: Core Tool Fallback Declarations

> Add fallback chains to the most failure-prone core tool manifests — file I/O, HTTP, and shell tools.

---

## Why This Phase

The resolver (Phase 2) is generic infrastructure. This phase makes it useful by declaring fallback chains for the tools that fail most often in practice: file writes (disk full, permission denied), HTTP requests (timeout, connection refused), and shell execution (timeout).

---

## Current → Target State

**Current:** Core tool manifests in `tools/core/` have no `[[fallback]]` sections.

**Target:** High-failure-rate tools declare practical fallback chains.

---

## Detailed Subtasks

### 1. File write fallbacks

**File:** `tools/core/file-write.toml`

```toml
[[fallback]]
on_error = "StorageError"
try_tool = "file-write"
transform = { path = "prepend:/tmp/agentos-overflow/" }
max_retries = 1

[[fallback]]
on_error = "PermissionDenied"
try_tool = "notify-user"
transform = { message = "replace:File write failed due to permissions. Path: ${path}" }
max_retries = 1
```

### 2. HTTP client fallbacks

**File:** `tools/core/http-client.toml`

```toml
[[fallback]]
on_error = "Timeout"
try_tool = "http-client"
transform = { timeout_ms = "replace:30000" }
max_retries = 2
```

### 3. Shell exec fallbacks

**File:** `tools/core/shell-exec.toml`

```toml
[[fallback]]
on_error = "Timeout"
try_tool = "shell-exec"
transform = { timeout_secs = "replace:120" }
max_retries = 1
```

### 4. Web fetch fallbacks

**File:** `tools/core/web-fetch.toml`

```toml
[[fallback]]
on_error = "Timeout"
try_tool = "web-fetch"
transform = { timeout_ms = "replace:30000" }
max_retries = 1

[[fallback]]
on_error = "NetworkError"
try_tool = "notify-user"
transform = { message = "replace:Web fetch failed due to network error. URL could not be reached." }
max_retries = 1
```

---

## Files Changed

| File | Change |
|------|--------|
| `tools/core/file-write.toml` | Add 2 fallback rules |
| `tools/core/http-client.toml` | Add 1 fallback rule |
| `tools/core/shell-exec.toml` | Add 1 fallback rule |
| `tools/core/web-fetch.toml` | Add 2 fallback rules |

---

## Dependencies

- **Requires:** Phase 1 (schema), Phase 2 (resolver must be functional)
- **Blocks:** Nothing

---

## Test Plan

1. **Integration test: file-write disk full** — simulate `StorageError` from file-write; verify fallback writes to `/tmp/agentos-overflow/`
2. **Integration test: http timeout retry** — simulate timeout from http-client; verify retry with longer timeout
3. **Manifest parse test** — load each modified manifest; verify `fallbacks` field is populated with correct rules

---

## Verification

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```
