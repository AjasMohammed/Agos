---
title: Security Gate Closure
tags:
  - security
  - v3
  - plan
date: 2026-03-12
status: complete
effort: 2d
priority: critical
---

# Security Gate Closure

> Translate security implementation into mandatory, executable deployment acceptance checks with specific test files and assertions.

## Why this phase

Security controls are only deployment-ready when they are verifiably enforced in runtime behavior and operational workflows. The current security tests exist in scattered unit tests, but there is no consolidated acceptance suite that an operator can run before launch. Each scenario must have a specific test file, assertion pattern, and expected audit event.

## Current -> Target state

- **Current:** critical controls exist in code paths, but deployment-time validation is fragmented. No consolidated security smoke suite. Existing tests are unit-level, not scenario-level.
- **Target:** explicit security smoke test module with named scenarios, specific assertions, and audit event verification. Any failure blocks deployment.

## Detailed subtasks

### 1. Create security acceptance test module

**File:** `crates/agentos-kernel/tests/security_acceptance_test.rs`

This file consolidates all mandatory security scenarios as integration tests.

### 2. Implement mandatory scenarios

Each scenario must:
- Set up the required state (agent, tool, message)
- Trigger the security-relevant action
- Assert the expected deny/escalate behavior
- Verify the corresponding audit event was emitted

#### Scenario A: Reject unsigned A2A message
- **Code path:** `crates/agentos-kernel/src/agent_message_bus.rs` — `verify_signature()`
- **Setup:** Register two agents. Agent A sends a message to Agent B without signing it.
- **Assert:** Message delivery fails with `AgentOSError::InvalidInput` or signature verification error.
- **Audit event:** `MessageAuthFailed` or equivalent event logged.

#### Scenario B: Reject forged signature
- **Code path:** `crates/agentos-kernel/src/agent_message_bus.rs` — `verify_signature()`
- **Setup:** Agent A signs a message, then tamper with the signature bytes before delivery.
- **Assert:** Signature verification fails.
- **Audit event:** Security-level event logged with agent ID.

#### Scenario C: Enforce secret scope denial
- **Code path:** `crates/agentos-vault/src/vault.rs` — `get_secret()` scope check
- **Setup:** Store a secret scoped to Agent A. Agent B attempts to read it.
- **Assert:** `get_secret()` returns scope-denied error.
- **Audit event:** `SecretAccessDenied` event logged.

#### Scenario D: Escalate high-risk tool execution
- **Code path:** `crates/agentos-kernel/src/risk_classifier.rs` → `crates/agentos-kernel/src/escalation.rs`
- **Setup:** Configure a tool action as `hard_approval`. Agent submits intent.
- **Assert:** Task enters `PendingEscalation` state instead of executing.
- **Audit event:** `EscalationCreated` event logged.

#### Scenario E: Detect prompt injection in LLM output
- **Code path:** `crates/agentos-kernel/src/injection_scanner.rs` — `scan()`
- **Setup:** Feed known injection payloads (role override, system prompt exfil, delimiter injection, base64-encoded payloads) to the scanner.
- **Assert:** Scanner returns `InjectionDetected` with appropriate pattern classification for each payload.
- **Audit event:** `InjectionDetected` event logged.

#### Scenario F: Block tool with `Blocked` trust tier
- **Code path:** `crates/agentos-kernel/src/tool_registry.rs` — `register()`
- **Setup:** Attempt to register a tool manifest with `trust_tier = "blocked"`.
- **Assert:** Registration fails with `AgentOSError::ToolBlocked`.
- **Audit event:** `ToolBlocked` event logged.

#### Scenario G: Reject tool with invalid signature
- **Code path:** `crates/agentos-tools/src/signing.rs` — `verify_manifest()`
- **Setup:** Create a Community-tier tool manifest with a tampered signature.
- **Assert:** Verification fails with `AgentOSError::ToolSignatureInvalid`.
- **Audit event:** `ToolSignatureInvalid` event logged.

### 3. Add operational runbook section

**File:** `docs/guide/06-security.md`

Add a "Deployment Security Acceptance" section listing:
- Each scenario name and what it validates
- How to run: `cargo test -p agentos-kernel --test security_acceptance_test`
- Expected pass criteria
- What to do if a scenario fails

### 4. Define launch-blocking policy

**File:** `agentic-os-deployment.md`

Add section: "Security Gate — Required before launch"
- All 7 scenarios must pass
- Audit log must contain corresponding events
- Any failure is a hard block — no deployment without resolution

### 5. Track closure status

**File:** `obsidian-vault/roadmap/Issues and Fixes.md`

Add security closure checklist with pass/fail per scenario.

## Files changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/tests/security_acceptance_test.rs` | New: 7 security acceptance scenarios |
| `docs/guide/06-security.md` | Add deployment acceptance section |
| `agentic-os-deployment.md` | Add security gate requirements |
| `obsidian-vault/roadmap/Issues and Fixes.md` | Security closure tracking |

## Dependencies

- **Requires:** [[01-quality-gates-stabilization]], [[03-containerization-and-runtime]].
- **Blocks:** [[05-release-process-and-cutover]].

## Test plan

- Execute each mandatory scenario and assert expected deny/escalate behavior.
- Verify corresponding audit records are emitted.
- Run the full suite: `cargo test -p agentos-kernel --test security_acceptance_test`
- All 7 scenarios must pass.

## Verification

```bash
# Run security acceptance suite
cargo test -p agentos-kernel --test security_acceptance_test

# Verify all scenarios are present
cargo test -p agentos-kernel --test security_acceptance_test -- --list 2>&1 | grep 'test ' | wc -l
# Expected: 7

# Full workspace test still passes
cargo test --workspace
```
