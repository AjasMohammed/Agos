---
title: "Phase 1: Sandbox Showcase & Enterprise Trust"
tags:
  - strategy
  - security
  - enterprise
  - phase-1
date: 2026-04-08
status: planned
effort: 2w
priority: critical
---

# Phase 1: Sandbox Showcase & Enterprise Trust

> Build hyper-visible proof that AgentOS prevents autonomous agents from exceeding their boundaries. Produce a demo video, a technical whitepaper, and hardening fixes that close remaining security gaps.

---

## Why This Phase

Research finding: Enterprise adoption of autonomous agents is blocked by fear of **arbitrary tool execution** — agents running `rm -rf`, dropping database tables, or exfiltrating data. AgentOS already has the security primitives (CapabilityTokens, Seccomp-BPF, WASM sandbox, Trust Tiers, audit log). What's missing is **visible proof** — a demonstration that converts internal capability into external trust.

Competitor context: LangGraph wins enterprise trust through deterministic DAGs and visual debugging. PydanticAI wins through type-safe validation. Neither has kernel-level capability enforcement. AgentOS's demo must show something no framework can: a malicious prompt intercepted at the syscall level.

---

## Current → Target State

**Current:** Security primitives are implemented across 5+ crates but have no external-facing demonstration, whitepaper, or hardening verification suite.

**Target:** A polished demo scenario, a published whitepaper, and a hardened security surface that can withstand adversarial review.

---

## Detailed Subtasks

### 1. Build the "Malicious Agent" Demo Scenario

Create a reproducible demo where an LLM agent receives a prompt injection telling it to:
- (a) Drop a database table via the shell tool
- (b) Read `/etc/passwd` via the file tool
- (c) Exfiltrate secrets from the vault
- (d) Escape the WASM sandbox

**Files to create/modify:**
- `examples/security-demo/malicious_prompts.toml` — curated attack prompts
- `examples/security-demo/demo_agent.toml` — agent manifest with restricted CapabilityToken
- `examples/security-demo/run_demo.sh` — orchestration script that runs each attack and captures output

**Implementation:**
```rust
// The demo agent has a CapabilityToken with only:
// - file:read (restricted to /tmp/demo/)
// - shell:execute (restricted to echo, ls)
// When the malicious prompt tries to `rm -rf /`, the kernel:
// 1. Validates CapabilityToken against required PermissionSet
// 2. Rejects: PermissionDenied("shell:execute does not cover rm")
// 3. Logs to AuditLog: ToolRejected { tool: "shell", reason: "..." }
// 4. Escalation created if configured
```

### 2. Record Demo Video / Asciinema

Capture a terminal recording showing:
1. Agent boots with restricted capabilities
2. Malicious prompt injected
3. Kernel intercepts each attack with clear error messages
4. Audit log shows every rejection with timestamps
5. Comparison: same prompt on unprotected Python script succeeds (destructively)

**Tool:** `asciinema` for terminal recording, convert to GIF/MP4 for sharing.

### 3. Write Security Whitepaper

**File:** `docs/whitepapers/agentos-security-model.md` (or PDF export)

**Sections:**
1. **Threat Model** — what autonomous agents can do wrong (arbitrary execution, data exfil, prompt injection, sandbox escape)
2. **Defense-in-Depth Architecture** — layered security: CapabilityTokens → Trust Tiers → Seccomp-BPF → WASM → Audit Chain
3. **Capability Token System** — HMAC-SHA256 signing, per-tool validation, time-limited tokens, permission deny entries
4. **Trust Tier System** — Ed25519 manifest signing, Core/Verified/Community/Blocked enforcement
5. **Sandbox Architecture** — Seccomp-BPF syscall filtering, Wasmtime isolation, path traversal blocking
6. **Audit Trail** — append-only SQLite, HMAC chain for tamper detection, 83+ event types
7. **Vault & Secret Management** — AES-256-GCM, Argon2id, ZeroizingString, proxy vault at tool boundary
8. **Comparison Matrix** — AgentOS vs LangGraph vs CrewAI vs mcp-agent on security dimensions

### 4. Security Hardening Sweep

Close remaining gaps identified in research:

| Gap | Fix | File(s) |
|-----|-----|---------|
| Secret proxy partial wiring | Verify `ProxyVault` is used at all tool execution boundaries | `crates/agentos-kernel/src/task_executor.rs` |
| Path traversal edge cases | Fuzz `..` blocking with URL-encoded variants (`%2e%2e`) | `crates/agentos-tools/src/file_tools.rs` |
| Prompt injection patterns | Add Unicode homoglyph detection to `injection_scanner.rs` | `crates/agentos-kernel/src/injection_scanner.rs` |
| Audit chain integrity | Add verification CLI command: `agentos audit verify` | `crates/agentos-cli/src/commands/audit.rs` |

### 5. Security Test Suite

Create dedicated security integration tests:

**File:** `crates/agentos-kernel/tests/security_hardening.rs`

```rust
#[tokio::test]
async fn test_capability_token_rejects_unauthorized_tool() { ... }

#[tokio::test]
async fn test_path_traversal_blocked_with_url_encoding() { ... }

#[tokio::test]
async fn test_wasm_sandbox_prevents_filesystem_escape() { ... }

#[tokio::test]
async fn test_audit_chain_detects_tampering() { ... }

#[tokio::test]
async fn test_vault_secrets_never_in_tool_output() { ... }
```

---

## Files Changed

| File | Change |
|------|--------|
| `examples/security-demo/` (new dir) | Demo scenario assets |
| `docs/whitepapers/agentos-security-model.md` (new) | Security whitepaper |
| `crates/agentos-kernel/src/injection_scanner.rs` | Unicode homoglyph detection |
| `crates/agentos-tools/src/file_tools.rs` | URL-encoded traversal blocking |
| `crates/agentos-cli/src/commands/audit.rs` | `audit verify` subcommand |
| `crates/agentos-kernel/tests/security_hardening.rs` (new) | Security integration tests |

---

## Dependencies

- **Requires:** Nothing — can start immediately
- **Blocks:** Phase 4 (demo assets feed developer marketing), Phase 5 (hardening fixes inform enterprise readiness)

---

## Test Plan

1. Run demo script end-to-end: all 4 attack vectors rejected with correct error codes
2. `cargo test -p agentos-kernel -- security_hardening` — all pass
3. `agentos audit verify` on a populated audit DB — reports chain intact
4. Manual review: whitepaper accurately describes implemented (not aspirational) features
5. `cargo clippy --workspace -- -D warnings` — no new warnings

---

## Verification

```bash
# Run security demo
cd examples/security-demo && bash run_demo.sh

# Run security tests
cargo test -p agentos-kernel -- security_hardening

# Verify audit chain
cargo run -- audit verify --db-path /tmp/test_audit.db

# Full workspace check
cargo build --workspace && cargo test --workspace && cargo clippy --workspace -- -D warnings
```
