---
title: Handbook Security and Vault
tags:
  - docs
  - security
  - v3
  - plan
date: 2026-03-13
status: complete
effort: 4h
priority: high
---

# Handbook Security and Vault

> Write the Security Model and Secrets & Vault chapters covering the 7-layer defense model, capability tokens, permissions, trust tiers, injection scanning, risk classification, escalation, identity, secrets vault, and lockdown.

---

## Why This Subtask
Security is described as "non-negotiable" in the AgentOS philosophy. Users need to understand the full security model to operate the system safely. The existing security doc (`docs/guide/06-security.md`) covers basics but is missing the V3 additions: injection scanner (23 patterns), risk classifier (5 levels), escalation system, agent identity (Ed25519), trust tiers, and the vault lockdown command.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Defense layers | 7 layers listed briefly | Each layer explained in detail with examples |
| Injection scanner | Not documented for users | Full section: what it detects, 23 pattern categories, taint wrapping, how it integrates with task execution |
| Risk classifier | Not documented for users | 5-level taxonomy (Level 0-4) with examples and escalation thresholds |
| Escalation system | Not documented for users | Full workflow: agent triggers escalation, task pauses, human reviews via CLI, resolution resumes task |
| Agent identity | Not documented for users | Ed25519 keypairs, message signing, identity show/revoke |
| Vault lockdown | Not documented | Emergency lockdown command, what it does |
| Deny entries | Not documented | `PermissionSet.deny_entries` and SSRF blocking |

---

## What to Do

### 1. Write `08-Security Model.md`

Read these source files for ground truth:
- `docs/guide/06-security.md` -- existing security content
- `crates/agentos-capability/src/engine.rs` -- `CapabilityEngine`, token issuance and verification
- `crates/agentos-types/src/capability.rs` -- `CapabilityToken`, `PermissionSet`, `PermissionEntry` types
- `crates/agentos-kernel/src/injection_scanner.rs` -- injection patterns, `InjectionScanResult`, `taint_wrap()`
- `crates/agentos-kernel/src/risk_classifier.rs` -- `ActionRiskLevel` (Level0-Level4), classification logic
- `crates/agentos-kernel/src/escalation.rs` -- `EscalationManager`, `PendingEscalation`, `sweep_expired()`
- `crates/agentos-kernel/src/identity.rs` -- Ed25519 key generation, signing, verification
- `crates/agentos-kernel/src/intent_validator.rs` -- two-layer validation (structural + semantic)
- `crates/agentos-cli/src/commands/perm.rs` -- permission CLI commands
- `crates/agentos-cli/src/commands/role.rs` -- role CLI commands
- `crates/agentos-cli/src/commands/escalation.rs` -- escalation CLI commands
- `crates/agentos-cli/src/commands/identity.rs` -- identity CLI commands
- `crates/agentos-sandbox/src/lib.rs` -- seccomp-BPF sandboxing

The chapter must include:

**Section: Security Philosophy**
- Non-negotiable principle, threat model assumptions (prompt injection, privilege escalation, rogue tasks, supply chain attacks)

**Section: Defense in Depth (7 Layers)**
For each layer, provide:
1. **Capability-Based Access Control** -- unforgeable HMAC-SHA256 tokens, scoped per task/agent, time-limited
2. **Tool Sandboxing** -- seccomp-BPF, bwrap namespace isolation, Wasmtime capability isolation
3. **Intent Verification** -- structural validation (capability check) + semantic validation (loop detection, write-without-read, scope escalation)
4. **Output Sanitization** -- tool outputs wrapped in typed delimiters, taint_wrap() for injection-detected content
5. **Immutable Audit Log** -- Merkle hash chain, append-only SQLite
6. **Secrets Isolation** -- AES-256-GCM encryption, Argon2id key derivation, zeroize on drop, proxy tokens
7. **Agent Identity** -- Ed25519 signed messages, kernel-issued identity tokens

**Section: Capability Tokens**
- Token structure: `task_id`, `agent_id`, `allowed_tools`, `allowed_intents`, `issued_at`, `expires_at`, `signature`
- How tokens are issued (kernel signs with HMAC-SHA256)
- How tokens are verified (every tool call, every intent)
- Token delegation (child tasks get downscoped tokens)

**Section: Permission System**
- Format: `<resource>:<ops>` where ops are r/w/x
- Zero-permissions by default
- `agentctl perm grant/revoke/show` commands with examples
- Time-limited permissions (`--expires`)
- Permission profiles (create, assign)
- Deny entries for explicit blocking
- SSRF blocking via deny entries on network resources
- Path-prefix matching for filesystem permissions

**Section: Roles (RBAC)**
- `agentctl role create/delete/list/grant/revoke/assign/remove` with examples
- Persistent across kernel restarts
- Roles compose with direct permission grants

**Section: Injection Scanner**
- 23 pattern categories detected (system prompt extraction, role override, encoding attacks, etc.)
- How it integrates: runs on every LLM output before tool execution
- Taint wrapping: suspicious content is wrapped in `<user_data>` tags
- Standing system prompt instruction about tainted content

**Section: Risk Classification**
- 5 levels: Level 0 (safe) to Level 4 (forbidden)
- Examples for each level
- Level 3-4 actions trigger escalation and task pause

**Section: Escalation System**
- `PendingEscalation` structure: reason, urgency, blocking, context_summary, decision_point, options
- Auto-expiry after 5 minutes (`sweep_expired()`)
- CLI workflow: `agentctl escalation list`, `escalation get <id>`, `escalation resolve <id> --decision "Approved"`
- Resolution resumes the paused task

**Section: Agent Identity**
- Ed25519 keypair generated on agent connect
- Messages signed with private key
- `agentctl identity show --agent <name>` -- view public key
- `agentctl identity revoke --agent <name>` -- revoke identity and permissions

### 2. Write `09-Secrets and Vault.md`

Read these source files:
- `crates/agentos-vault/src/lib.rs` -- `SecretsVault`, encryption/decryption
- `crates/agentos-types/src/secret.rs` -- `SecretEntry`, `SecretScope`
- `crates/agentos-cli/src/commands/secret.rs` -- secret CLI commands (set, list, revoke, rotate, lockdown)
- `docs/guide/06-security.md` -- existing vault section

The chapter must include:
- **Vault architecture** -- AES-256-GCM encryption, Argon2id key derivation from passphrase, SQLite storage
- **Security guarantees** -- never in CLI args, never in env vars, never in config files, zeroed from memory
- **Setting secrets** -- `agentctl secret set NAME [--scope SCOPE]` with hidden input
- **Secret scopes** -- `global`, `agent:<name>`, `tool:<name>` with use cases
- **Listing secrets** -- metadata only, values never shown
- **Rotating secrets** -- `agentctl secret rotate NAME`
- **Revoking secrets** -- `agentctl secret revoke NAME`
- **Emergency lockdown** -- `agentctl secret lockdown` -- revokes all proxy tokens, blocks new issuance
- **How agents access secrets** -- kernel decrypts at initialization, passes to LLM adapters internally; agents never see raw values

---

## Files Changed

| File | Change |
|------|--------|
| `obsidian-vault/reference/handbook/08-Security Model.md` | Create new |
| `obsidian-vault/reference/handbook/09-Secrets and Vault.md` | Create new |

---

## Prerequisites
[[01-foundation-chapters]] must be complete (architecture context needed).

---

## Test Plan
- Both files exist
- Security chapter covers all 7 defense layers with detail
- All 5 risk levels documented
- Escalation workflow documented with CLI commands
- Vault chapter covers set, list, revoke, rotate, lockdown commands
- All 3 secret scopes documented

---

## Verification
```bash
test -f obsidian-vault/reference/handbook/08-Security\ Model.md
test -f obsidian-vault/reference/handbook/09-Secrets\ and\ Vault.md

# Security chapter covers all layers
grep -c "Layer\|Defense" obsidian-vault/reference/handbook/08-Security\ Model.md
# Should be >= 7

# Vault chapter covers all commands
for cmd in set list revoke rotate lockdown; do
  grep -q "$cmd" obsidian-vault/reference/handbook/09-Secrets\ and\ Vault.md || echo "MISSING: $cmd"
done
```
