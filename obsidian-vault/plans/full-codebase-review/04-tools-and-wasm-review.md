---
title: "Phase 4: Tools & WASM Review"
tags:
  - review
  - security
  - tools
  - wasm
  - phase-4
date: 2026-03-13
status: complete
effort: 3h
priority: critical
---

# Phase 4: Tools & WASM Review

> Review the 17+ built-in tools, Ed25519 signing, manifest loading, input sanitization, and WASM execution runtime.

---

## Why This Phase

Tools are the **execution boundary** — they perform file I/O, shell commands, HTTP requests, and process management on behalf of AI agents. This is where path traversal, command injection, and SSRF attacks would occur. The signing system protects tool integrity. The WASM runtime executes untrusted code.

---

## Current → Target State

- **Current:** `agentos-tools` (20 files, 2,676 lines + 2 test files), `agentos-wasm` (2 files, 284 lines), `agentos-sdk` (1 file + 1 test)
- **Target:** All tool implementations validated for injection, traversal, and SSRF resistance; signing verified for forgery resistance

---

## Step 4.1 — Trait, Sanitize, Signing (~487 lines) `SECURITY-CRITICAL`

**Files:**
- `crates/agentos-tools/src/traits.rs` (35) — `AgentTool` trait
- `crates/agentos-tools/src/sanitize.rs` (94) — Input/output sanitization
- `crates/agentos-tools/src/signing.rs` (358) — Ed25519 manifest signing/verification

**Checklist:**
- [x] `AgentTool` trait is object-safe — `async_trait` + `Send + Sync` bounds, object-safe methods
- [x] Path sanitization: `..` blocked in all forms — canonicalize + starts_with enforces boundary
- [x] Ed25519 canonical JSON construction is deterministic — BTreeMap-ordered `serde_json::Value::Object`
- [x] Signature verification rejects: wrong key, tampered payload, truncated signature — tested in signing tests
- [x] Key generation uses cryptographically secure RNG — CLI keygen uses `OsRng`; primitive takes a seed
- [x] Sanitize handles null bytes, control characters — output sanitizer escapes injection patterns; file tools use Path (null-safe at OS level)

---

## Step 4.2 — Loader, Runner, Lib (~623 lines)

**Files:**
- `crates/agentos-tools/src/loader.rs` (60) — Manifest loading
- `crates/agentos-tools/src/runner.rs` (139) — Tool execution runner
- `crates/agentos-tools/src/lib.rs` (424) — Module re-exports and tool dispatch

**Checklist:**
- [x] Tool manifest loading validates required fields — TOML deserialization errors on missing required fields
- [x] Trust tier enforced at load time (Blocked tools rejected) — `verify_manifest` called in `load_manifest`
- [x] Tool runner validates capability token before execution — `runner.rs` does defense-in-depth permission check
- [x] Runner handles tool timeout — each tool (shell-exec, http-client) enforces its own timeout; ToolRunner has no global timeout (noted below)
- [x] `lib.rs` tool dispatch covers all registered tools — all 17 built-in tools registered in `register_memory_tools`
- **Note:** ToolRunner has no global per-call timeout wrapper. Tools without internal timeouts (memory ops, data parser) could block indefinitely if the backing store hangs. Low risk in practice but worth tracking.

---

## Step 4.3 — File I/O Tools (~227 lines) `SECURITY-CRITICAL`

**Files:**
- `crates/agentos-tools/src/file_reader.rs` (85)
- `crates/agentos-tools/src/file_writer.rs` (142)

**Checklist:**
- [x] Path traversal blocked: both tools reject paths — reader uses `canonicalize` + `starts_with`; writer uses `normalize_path` + `starts_with(canonical_data_dir)`
- [x] File size limits on read — **FIXED**: `file_reader.rs` now rejects files > 10 MiB before calling `read_to_string`
- [x] `data_dir` canonicalized in containment check — **FIXED**: reader now canonicalizes `data_dir` (matching writer behavior)
- [x] Write tool respects permission set — `fs.user_data:w` required; path confined to `canonical_data_dir`
- [x] Symlink following — `canonicalize` resolves all symlinks then checks containment; escape via symlink is blocked
- [x] TOCTOU — file-writer uses atomic `.tmp` + rename for overwrite/create_only; append uses O_APPEND flag

---

## Step 4.4 — Shell, HTTP Client, Process Manager (~567 lines) `SECURITY-CRITICAL`

**Files:**
- `crates/agentos-tools/src/shell_exec.rs` (162) — Shell command execution
- `crates/agentos-tools/src/http_client.rs` (342) — HTTP requests
- `crates/agentos-tools/src/process_manager.rs` (63) — Process management

**Checklist:**
- [x] Shell exec: command injection prevention — command passed as separate arg to `sh -c`, not interpolated into shell string; null byte check added
- [x] Shell exec: timeout enforcement, output size limits — `tokio::time::timeout(timeout_secs)` + 50K char truncation
- [x] Shell exec: requires bwrap — hard-errors if bubblewrap not installed; no fallback to unsandboxed exec
- [x] HTTP client: SSRF prevention — blocks loopback, RFC1918 (10.x, 172.16.x, 192.168.x), link-local 169.254.x, multicast, ::1, fc00::/7, fe80::/10, `localhost` hostname
- [x] HTTP client: redirect blocking — `Policy::none()` on reqwest client; no auto-follows
- [x] HTTP client: response size limits — 10 MiB streaming cap with truncation flag
- [x] Process manager: cannot kill arbitrary system processes — delegates to HAL; requires `process.kill` permission; HAL enforces process ownership

---

## Step 4.5 — Memory, Data Tools (~480 lines)

**Files:**
- `crates/agentos-tools/src/memory_search.rs` (230)
- `crates/agentos-tools/src/memory_write.rs` (131)
- `crates/agentos-tools/src/data_parser.rs` (119)

**Checklist:**
- [x] Memory search bounds result set size — **FIXED**: `top_k` now capped at 100 via `top_k.min(MAX_TOP_K)`
- [x] Memory write validates input size — **FIXED**: `content` now rejected if > 512 KiB
- [x] Data parser handles malformed input gracefully — errors returned (not panics) for bad JSON/CSV/TOML
- [x] No unbounded allocations from user-controlled input — **FIXED**: `data` field in data-parser capped at 4 MiB; CSV row count capped at 50,000

---

## Step 4.6 — Remaining Tools & Tests (~721 lines)

**Files:**
- `crates/agentos-tools/src/sys_monitor.rs` (52), `hardware_info.rs` (42), `network_monitor.rs` (42), `log_reader.rs` (46), `agent_message.rs` (50), `task_delegate.rs` (60)
- `crates/agentos-tools/tests/http_client_test.rs` (212), `shell_exec_test.rs` (80)

**Checklist:**
- [x] Log reader does not expose arbitrary file reading — delegates to HAL `log` subsystem; requires `fs.app_logs`/`fs.system_logs` permissions; no direct file path access
- [x] Network monitor does not leak internal network topology — delegates to HAL `network` subsystem; returns only what HAL exposes
- [x] Tests exercise error paths and security invariants — path traversal, permission denial, SSRF, timeout, size limit tests all present
- [x] Test isolation: tests use tempdir — `TempDir` used throughout; no global filesystem mutations

---

## Step 4.7 — WASM Runtime (~284 lines)

**Files:**
- `crates/agentos-wasm/src/lib.rs` (~46), `wasm_tool.rs` (~238)

**Checklist:**
- [x] WASM memory limit — **FIXED**: `WasiState` implements `ResourceLimiter` with 256 MiB cap; `store.limiter()` wired in
- [x] WASM CPU limit — epoch interruption configured; per-invocation tokio task fires `engine.increment_epoch()` after `max_cpu_ms`
- [x] WASI permissions minimal — `WasiCtxBuilder` grants only: stdin pipe, stderr pipe, one env var (`AGENTOS_OUTPUT_FILE`); no filesystem mounts, no network access
- [x] Module loading validates WASM binary — `Module::from_file` validates WASM binary format at load time; invalid binaries return error
- [x] Error handling for WASM traps — `match start.call_async` handles epoch timeout (CPU exceeded), generic traps (with stderr capture), and normal completion separately
- [x] Output file RAII cleanup — `TempOutputFile` guard ensures output file is deleted on all code paths including panics

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-tools/src/file_reader.rs` | Add 10 MiB size guard before `read_to_string`; canonicalize `data_dir` in containment check |
| `crates/agentos-tools/src/memory_search.rs` | Cap `top_k` at 100 |
| `crates/agentos-tools/src/memory_write.rs` | Reject `content` > 512 KiB |
| `crates/agentos-tools/src/data_parser.rs` | Reject `data` > 4 MiB; cap CSV rows at 50,000 |
| `crates/agentos-wasm/src/wasm_tool.rs` | Add `WasiState` with `ResourceLimiter` (256 MiB cap); wire `store.limiter()` |

## Dependencies

Phases 1-3 (types, capability, bus understood).

## Verification

```bash
cargo build -p agentos-tools -p agentos-wasm -p agentos-sdk
cargo test -p agentos-tools -p agentos-sdk
```

---

## Related

- [[Full Codebase Review Plan]]
- [[08-security-deep-dives]] — Step 8.2 does adversarial deep dive on tool execution boundary
