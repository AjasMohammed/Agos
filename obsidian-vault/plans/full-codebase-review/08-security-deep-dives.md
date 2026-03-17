---
title: "Phase 8: Security Deep Dives"
tags:
  - review
  - security
  - adversarial
  - phase-8
date: 2026-03-13
status: complete
effort: 3h
priority: critical
---

# Phase 8: Security Deep Dives

> Adversarial-lens re-reads of the four most security-critical code paths. These re-examine files already covered in earlier phases, but with dedicated attacker-mindset questions.

---

## Why This Phase

Per-crate review catches implementation bugs. This phase asks: **"How would an attacker exploit this?"** — focusing on the authorization boundary, execution boundary, injection defense, and secrets management. These are the four paths where a single flaw could compromise the entire system.

---

## Step 8.1 — Capability Token Lifecycle

**Files:**
- `crates/agentos-capability/src/engine.rs` (453)
- `crates/agentos-capability/src/token.rs` (37)
- `crates/agentos-types/src/capability.rs` (317)

**Adversarial questions:**
- [ ] Can an agent **forge** a token with elevated permissions?
- [ ] Can an **expired** token be replayed?
- [ ] Is the HMAC comparison **constant-time**? (timing oracle)
- [ ] Can a token be used for a **different agent** than it was issued for?
- [ ] What happens if the HMAC key is **empty or default**?
- [ ] Can a token's permission set be **mutated after creation**?
- [ ] Is there a **token revocation** mechanism? What if an agent is compromised?
- [ ] Can a lower-privilege agent **craft a token** that grants higher privileges?

---

## Step 8.2 — Tool Execution Security Boundary

**Files:**
- `crates/agentos-kernel/src/task_executor.rs` (1,412) — specifically the tool call dispatch section
- `crates/agentos-tools/src/runner.rs` (139)
- `crates/agentos-tools/src/sanitize.rs` (94)

**Adversarial questions:**
- [ ] Can a tool call **bypass capability validation** entirely?
- [ ] Can a malicious tool name or input cause **path traversal** or **command injection**?
- [ ] Does the sanitizer catch **all encoding forms** of `..`? (`%2e%2e`, `..%2f`, `....//`, `..;/`)
- [ ] Can a tool execution **outlive its task** (zombie tool)?
- [ ] Can a tool **read the vault** or access another agent's context?
- [ ] What happens if a tool returns **adversarial output** (crafted to manipulate the LLM)?
- [ ] Can a tool call trigger **another tool call recursively** to bypass limits?

---

## Step 8.3 — Injection & Prompt Safety

**Files:**
- `crates/agentos-kernel/src/injection_scanner.rs` (381)
- `crates/agentos-kernel/src/trigger_prompt.rs` (348)
- `crates/agentos-kernel/src/context_compiler.rs` (554)

**Adversarial questions:**
- [ ] Can user data in context entries **inject system-level instructions**?
- [ ] Does the injection scanner run on **all** user-provided content?
- [ ] Can the trigger prompt be **manipulated via context entries**?
- [ ] Is `<user_data>` tagging applied **consistently** at every injection point?
- [ ] Can a **multi-step prompt injection** evade single-pass scanning?
- [ ] What if an injection attempt is **split across multiple context entries**?
- [ ] Can a tool output contain instructions that **override the system prompt**?
- [ ] Does the scanner handle **unicode homoglyphs** and **zero-width characters**?

---

## Step 8.4 — Secrets at Rest and in Transit

**Files:**
- `crates/agentos-vault/src/vault.rs` (784)
- `crates/agentos-vault/src/crypto.rs` (43)
- `crates/agentos-kernel/src/commands/secret.rs` (97)

**Adversarial questions:**
- [ ] Can an unauthenticated caller **read secrets from the vault database file** directly?
- [ ] Is the encryption key derivation **strong enough** against offline brute-force?
- [ ] Can a secret be **extracted from memory** after vault lock?
- [ ] Does the bus transport **encrypt secrets in transit**? (or are they plaintext on the Unix socket?)
- [ ] Can a malicious agent **access another agent's secrets**?
- [ ] What happens if the vault database is **corrupted or truncated**?
- [ ] Can an attacker **replay a previous vault state** to recover deleted secrets?
- [ ] Are **temp files or swap** used during vault operations that could leak plaintext?

---

## Findings

| File | Line(s) | Severity | Category | Description | Fix Applied |
|------|---------|----------|----------|-------------|-------------|
| `crates/agentos-kernel/src/injection_scanner.rs` | `scan()` | CRITICAL | Injection bypass | No Unicode NFKC normalization — attacker can use fullwidth chars (e.g., `ｉｇｎｏｒｅ`) to bypass all regex patterns | Yes — NFKC normalize before pattern matching |
| `crates/agentos-kernel/src/injection_scanner.rs` | pattern list | CRITICAL | Encoded payload | No standalone base64 block detection — large base64 blobs can encode instructions without keyword prefix | Yes — added `encoded_base64_standalone` pattern (60+ char base64 runs) |
| `crates/agentos-kernel/src/injection_scanner.rs` | `taint_wrap()` | WARNING | XML injection | `source` attribute interpolated without escaping — a tool name containing `"` could inject additional XML attributes | Yes — HTML-escape `source` (`&amp;`, `&quot;`, `&lt;`, `&gt;`) |
| `crates/agentos-kernel/src/injection_scanner.rs` | pattern list | WARNING | Incomplete detection | Missing closing XML tag pattern `</system>`, `</admin>` etc. — closing tags alone can confuse LLM context | Yes — added `delimiter_fake_xml_close_tag` pattern |
| `crates/agentos-vault/src/master_key.rs` | 20 | WARNING | Crypto | Argon2id `parallelism=1` — wastes multi-core hardware and reduces brute-force resistance | Yes — changed to `parallelism=4` (OWASP minimum) |
| `crates/agentos-capability/src/engine.rs` | `boot()` | WARNING | Error handling | If vault persistence of signing key fails, error is logged but engine continues with new ephemeral key — tokens won't survive restart | INFO/Design — logged at `error!` level; tokens are short-lived by design |
| `crates/agentos-kernel/src/commands/hal.rs` | all | WARNING | Authorization | HAL approve/deny/revoke commands have no caller authorization check | Deferred — protected by bus-level auth; full token-based HAL auth is spec item #9 |

## Remaining Issues

| Issue | Severity | Notes |
|-------|----------|-------|
| HAL command authorization | WARNING | Protected by bus auth today; proper token check deferred to spec #9 |
| Signing key persistence failure not fatal | INFO | By design — short-lived tokens; error is surfaced in logs |

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/injection_scanner.rs` | NFKC normalization, closing tag + standalone base64 patterns, `taint_wrap` escaping |
| `crates/agentos-vault/src/master_key.rs` | Argon2id parallelism 1→4 |
| `Cargo.toml` | Added `unicode-normalization = "0.1"` workspace dep |
| `crates/agentos-kernel/Cargo.toml` | Added `unicode-normalization` dep |

## Dependencies

Phases 1-6 complete (full codebase context available).

## Verification

Findings documented in structured table format. Critical findings should trigger immediate remediation tasks.

---

## Related

- [[Full Codebase Review Plan]]
- [[03-bus-and-capability-review]] — initial capability review
- [[04-tools-and-wasm-review]] — initial tools review
- [[10-synthesis-and-report]]
