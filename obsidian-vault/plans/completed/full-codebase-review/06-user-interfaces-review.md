---
title: "Phase 6: User Interfaces Review"
tags:
  - review
  - cli
  - web
  - sdk
  - phase-6
date: 2026-03-13
status: complete
effort: 4h
priority: high
---

# Phase 6: User Interfaces Review

> Review the CLI (`agentctl`), web UI, SDK, and all integration tests.

---

## Why This Phase

The CLI is the primary user-facing surface — it serializes commands, handles user input, and displays output including potentially sensitive data (secrets, audit logs). The web UI adds XSS/CSRF risk. Integration tests are the project's main safety net — understanding their coverage reveals what is and isn't protected.

---

## Current → Target State

- **Current:** `agentos-cli` (19 files, 3,346 lines + 5 tests, 800 lines), `agentos-web` (13 files, 799 lines), `agentos-sdk` (1 file + 1 test)
- **Target:** All CLI commands validated for input handling and output safety; web UI checked for XSS/CSRF; test coverage gaps identified

---

## Step 6.1 — CLI: Main & Dispatch (~491 lines)

**Files:**
- `crates/agentos-cli/src/main.rs` (448) — CLI entry point, clap parsing
- `crates/agentos-cli/src/commands/mod.rs` (43) — Command dispatcher

**Checklist:**
- [ ] All clap subcommands match documented CLI reference
- [ ] Offline commands (keygen, sign, verify) work without kernel connection
- [ ] Error display is user-friendly (no raw Rust error dumps)
- [ ] Exit codes are meaningful

---

## Step 6.2 — CLI: Core Commands (~890 lines)

**Files:**
- `crates/agentos-cli/src/commands/agent.rs` (253), `task.rs` (140), `tool.rs` (220), `pipeline.rs` (277)

**Checklist:**
- [ ] CLI correctly serializes commands for bus transport
- [ ] Response parsing handles kernel errors gracefully
- [ ] User input validated client-side before sending to kernel
- [ ] No sensitive data printed to stdout/stderr by default

---

## Step 6.3 — CLI: Security Commands (~558 lines)

**Files:**
- `crates/agentos-cli/src/commands/secret.rs` (152), `perm.rs` (193), `escalation.rs` (213)

**Checklist:**
- [ ] Secret commands do not echo secrets to terminal
- [ ] Permission commands accurately represent grant/revoke semantics
- [ ] Escalation approval requires explicit confirmation

---

## Step 6.4 — CLI: Operational Commands (~961 lines)

**Files:**
- `crates/agentos-cli/src/commands/event.rs` (323), `audit.rs` (195), `schedule.rs` (159), `role.rs` (143), `bg.rs` (141)

**Checklist:**
- [ ] Audit query does not allow SQL injection via CLI arguments
- [ ] Event subscription handles large event streams
- [ ] Schedule commands validate cron expressions client-side

---

## Step 6.5 — CLI: Remaining Commands (~446 lines)

**Files:**
- `crates/agentos-cli/src/commands/resource.rs` (184), `snapshot.rs` (106), `identity.rs` (70), `cost.rs` (67), `status.rs` (19)

**Checklist:**
- [ ] Snapshot restore validates snapshot integrity
- [ ] Identity commands protect private key material
- [ ] Cost display handles currency formatting correctly

---

## Step 6.6 — CLI: Integration Tests (~800 lines)

**Files:**
- `crates/agentos-cli/tests/common.rs` (61), `integration_test.rs` (309), `pipeline_test.rs` (247), `secrets_test.rs` (118), `kernel_boot_test.rs` (65)

**Checklist:**
- [ ] Tests use `MockLLMCore`, not real APIs
- [ ] Tests use `tempfile` for filesystem isolation
- [ ] `serial_test` used where needed for shared state
- [ ] `setup_kernel()` correctly initializes all subsystems
- [ ] Security invariants tested: path traversal, token forgery, permission denial
- [ ] Identify gaps: which features have zero test coverage?

---

## Step 6.7 — Web UI (~799 lines)

**Files:**
- All 13 files in `crates/agentos-web/src/` (lib.rs, server.rs, router.rs, state.rs, templates.rs, handlers/*)

**Checklist:**
- [ ] **XSS prevention:** HTML templates escape user input (Minijinja autoescaping)
- [ ] **CSRF protection:** state-changing endpoints require token
- [ ] Authentication: is the web UI protected? (or is it local-only?)
- [ ] Secret handler does not expose plaintext secrets in HTML
- [ ] No sensitive data in server logs

---

## Step 6.8 — SDK (~100 lines)

**Files:**
- `crates/agentos-sdk/src/lib.rs` (31), `tests/tool_macro_test.rs` (69)

**Checklist:**
- [ ] Re-exports are complete and ergonomic
- [ ] Macro test covers basic tool registration
- [ ] Documentation examples compile

---

## Findings

| File | Line(s) | Severity | Category | Description | Fix Applied |
|------|---------|----------|----------|-------------|-------------|
| `crates/agentos-web/src/csrf.rs` | 13 | WARNING | Visibility | `TOKEN_TTL` was `pub` but is referenced from `server.rs` in same crate — should be `pub(crate)` | Yes — changed to `pub(crate)` |
| `crates/agentos-web/src/server.rs` | `start_with_shutdown` | WARNING | Memory leak | CSRF `DashMap<String,(String,Instant)>` grows unbounded as sessions are abandoned; no sweep task existed | Yes — added tokio sweep task every 30 min evicting entries older than 2×TOKEN_TTL |
| `crates/agentos-cli/src/commands/secret.rs` | `parse_scope()` | WARNING | Input validation | `agent:` and `tool:` scopes accepted an empty name (e.g. `agent:`) without error | Yes — added empty-name guard with `anyhow::bail!` |

## Remaining Issues

None — all findings remediated.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-web/src/csrf.rs` | `TOKEN_TTL` visibility `pub` → `pub(crate)` |
| `crates/agentos-web/src/server.rs` | Added CSRF sweep background task in `start_with_shutdown()` |
| `crates/agentos-cli/src/commands/secret.rs` | `parse_scope()` empty-name validation |

## Dependencies

Phases 1-5 (all lower layers understood).

## Verification

```bash
cargo test -p agentos-cli
cargo test -p agentos-sdk
cargo build -p agentos-web
```

---

## Related

- [[Full Codebase Review Plan]]
- [[05-kernel-core-review]]
- [[07-cross-cutting-passes]]
