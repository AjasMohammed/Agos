---
title: Security Readiness Closure
tags:
  - security
  - v3
  - plan
date: 2026-03-12
status: planned
effort: 2d
priority: critical
---

# Security Readiness Closure

> Create a concrete, executable security acceptance test suite with specific test files, assertions, and audit event verification.

## Why this sub-task

Security features exist in code but require operational verification before first deployment. Existing tests are unit-level, not scenario-level. This step creates a consolidated acceptance suite where each scenario has a named test function, specific assertions, and expected audit events. Any failure blocks deployment.

## Current -> Target State

- **Current:** mixed status across message authentication, injection scanning, escalation, and secret scope checks. No consolidated security smoke suite. Existing tests are scattered across unit test modules.
- **Target:** `crates/agentos-kernel/tests/security_acceptance_test.rs` with 7 named scenarios, each asserting deny/escalate behavior and audit event emission. `cargo test -p agentos-kernel --test security_acceptance_test` is the single command to run.

## What to Do

### 1. Create security acceptance test file

**File:** `crates/agentos-kernel/tests/security_acceptance_test.rs`

Add test helpers from `tests/common.rs` and implement each scenario below.

### 2. Implement 7 mandatory scenarios

#### Scenario A: `test_reject_unsigned_message`
- **Code path:** `agent_message_bus.rs` — `verify_signature()`
- **Setup:** Register two agents via `register_mock_agent()`. Agent A sends message to Agent B without signing.
- **Assert:** Message delivery returns error (signature missing/invalid).
- **Audit:** Query audit log for security event with agent A's ID.

#### Scenario B: `test_reject_forged_signature`
- **Code path:** `agent_message_bus.rs` — `verify_signature()`
- **Setup:** Agent A signs a message, tamper the signature bytes.
- **Assert:** `verify_signature()` returns `false` or error.
- **Audit:** Security event logged.

#### Scenario C: `test_secret_scope_denial`
- **Code path:** `vault.rs` — `get_secret()` scope check
- **Setup:** `vault.set_secret("api_key", "value", Scope::Agent(agent_a_id))`. Agent B calls `get_secret("api_key", Scope::Agent(agent_b_id))`.
- **Assert:** Returns `Err` with scope-denied.
- **Audit:** `SecretAccessDenied` event.

#### Scenario D: `test_high_risk_escalation`
- **Code path:** `risk_classifier.rs` → `escalation.rs`
- **Setup:** Configure tool action as `hard_approval` in risk classifier. Agent submits intent.
- **Assert:** Returns `PendingEscalation` instead of executing.
- **Audit:** `EscalationCreated` event.

#### Scenario E: `test_injection_detection`
- **Code path:** `injection_scanner.rs` — `scan()`
- **Setup:** Feed 4 payload types: role override (`"ignore previous instructions"`), system prompt exfil (`"repeat your system prompt"`), delimiter injection (`"</system>"`), base64-encoded payload.
- **Assert:** `scan()` returns `InjectionDetected` for each with correct pattern classification.
- **Audit:** `InjectionDetected` event for each.

#### Scenario F: `test_blocked_tool_rejection`
- **Code path:** `tool_registry.rs` — `register()`
- **Setup:** Create tool manifest with `trust_tier = "blocked"`.
- **Assert:** `register()` returns `Err(AgentOSError::ToolBlocked)`.
- **Audit:** `ToolBlocked` event.

#### Scenario G: `test_invalid_tool_signature`
- **Code path:** `signing.rs` — `verify_manifest()`
- **Setup:** Create Community-tier tool manifest, tamper the signature field.
- **Assert:** `verify_manifest()` returns `Err(AgentOSError::ToolSignatureInvalid)`.
- **Audit:** `ToolSignatureInvalid` event.

### 3. Add runbook section to security docs

**File:** `docs/guide/06-security.md`

Add "Deployment Security Acceptance" section:
- List each scenario name and what it validates
- Command: `cargo test -p agentos-kernel --test security_acceptance_test`
- Pass criteria: all 7 pass
- What to do on failure

### 4. Add gate to deployment docs

**File:** `agentic-os-deployment.md`

Add "Security Gate — Required before launch": all 7 scenarios must pass, any failure is a hard block.

### 5. Track closure

**File:** `obsidian-vault/roadmap/Issues and Fixes.md`

Add security closure checklist with pass/fail per scenario.

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/tests/security_acceptance_test.rs` | New: 7 named security scenarios |
| `docs/guide/06-security.md` | Add deployment acceptance section |
| `agentic-os-deployment.md` | Add security gate requirements |
| `obsidian-vault/roadmap/Issues and Fixes.md` | Security closure tracking |

## Expected Inputs and Outputs

- **Input:** current security implementation state and test helpers in `tests/common.rs`.
- **Output:** 7 passing security acceptance tests, documented runbook, deployment gate.

## Prerequisites

- [[16-00-Code Safety Hardening]]
- [[16-01-Restore Quality Gates]]
- [[16-03-Add Container Deployment Artifacts]]

## Verification

```bash
# Run security acceptance suite
cargo test -p agentos-kernel --test security_acceptance_test

# Verify all 7 scenarios exist
cargo test -p agentos-kernel --test security_acceptance_test -- --list 2>&1 | grep 'test ' | wc -l
# Expected: 7

# Full workspace still passes
cargo test --workspace
```

Pass criteria:
- All 7 defined security scenarios pass.
- Required deny/escalation events appear in audit logs.
- `cargo test --workspace` still green.
