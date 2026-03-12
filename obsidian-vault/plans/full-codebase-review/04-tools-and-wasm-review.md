---
title: "Phase 4: Tools & WASM Review"
tags:
  - review
  - security
  - tools
  - wasm
  - phase-4
date: 2026-03-13
status: planned
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
- [ ] `AgentTool` trait is object-safe
- [ ] Path sanitization: `..` blocked in all forms (`../`, `..\\`, URL-encoded `%2e%2e`)
- [ ] Ed25519 canonical JSON construction is deterministic (sorted keys, no optional whitespace)
- [ ] Signature verification rejects: wrong key, tampered payload, truncated signature
- [ ] Key generation uses cryptographically secure RNG
- [ ] Sanitize handles null bytes, control characters, excessively long inputs

---

## Step 4.2 — Loader, Runner, Lib (~623 lines)

**Files:**
- `crates/agentos-tools/src/loader.rs` (60) — Manifest loading
- `crates/agentos-tools/src/runner.rs` (139) — Tool execution runner
- `crates/agentos-tools/src/lib.rs` (424) — Module re-exports and tool dispatch

**Checklist:**
- [ ] Tool manifest loading validates required fields
- [ ] Trust tier enforced at load time (Blocked tools rejected)
- [ ] Tool runner validates capability token before execution
- [ ] Runner handles tool timeout (does not hang forever)
- [ ] `lib.rs` tool dispatch covers all registered tools

---

## Step 4.3 — File I/O Tools (~227 lines) `SECURITY-CRITICAL`

**Files:**
- `crates/agentos-tools/src/file_reader.rs` (85)
- `crates/agentos-tools/src/file_writer.rs` (142)

**Checklist:**
- [ ] Path traversal blocked: both tools reject paths containing `..`
- [ ] File size limits on read (no reading multi-GB files into memory)
- [ ] Write tool respects permission set (cannot write outside allowed directories)
- [ ] Symlink following: does it follow symlinks to escape allowed directories?
- [ ] TOCTOU race conditions between path check and file operation

---

## Step 4.4 — Shell, HTTP Client, Process Manager (~567 lines) `SECURITY-CRITICAL`

**Files:**
- `crates/agentos-tools/src/shell_exec.rs` (162) — Shell command execution
- `crates/agentos-tools/src/http_client.rs` (342) — HTTP requests
- `crates/agentos-tools/src/process_manager.rs` (63) — Process management

**Checklist:**
- [ ] Shell exec: command injection prevention (no raw string interpolation)
- [ ] Shell exec: timeout enforcement, output size limits
- [ ] HTTP client: SSRF prevention (blocks 10.x, 172.16.x, 192.168.x, 169.254.x, localhost)
- [ ] HTTP client: follows redirects safely (redirect to internal IP blocked)
- [ ] HTTP client: response size limits
- [ ] Process manager: cannot kill arbitrary system processes

---

## Step 4.5 — Memory, Data Tools (~480 lines)

**Files:**
- `crates/agentos-tools/src/memory_search.rs` (230)
- `crates/agentos-tools/src/memory_write.rs` (131)
- `crates/agentos-tools/src/data_parser.rs` (119)

**Checklist:**
- [ ] Memory search bounds result set size
- [ ] Memory write validates input size
- [ ] Data parser handles malformed input gracefully (no panics on bad JSON/YAML/TOML)
- [ ] No unbounded allocations from user-controlled input

---

## Step 4.6 — Remaining Tools & Tests (~721 lines)

**Files:**
- `crates/agentos-tools/src/sys_monitor.rs` (52), `hardware_info.rs` (42), `network_monitor.rs` (42), `log_reader.rs` (46), `agent_message.rs` (50), `task_delegate.rs` (60)
- `crates/agentos-tools/tests/http_client_test.rs` (212), `shell_exec_test.rs` (80)

**Checklist:**
- [ ] Log reader does not expose arbitrary file reading
- [ ] Network monitor does not leak internal network topology
- [ ] Tests exercise error paths and security invariants
- [ ] Test isolation: tests use tempdir, not real filesystem

---

## Step 4.7 — WASM Runtime (~284 lines)

**Files:**
- `crates/agentos-wasm/src/lib.rs` (~46), `wasm_tool.rs` (~238)

**Checklist:**
- [ ] WASM fuel/memory limits configured
- [ ] WASI permissions minimal (no unnecessary filesystem or network access)
- [ ] Module loading validates WASM binary
- [ ] Error handling for WASM traps (out-of-bounds, stack overflow)

---

## Files Changed

No files changed — read-only review phase.

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
