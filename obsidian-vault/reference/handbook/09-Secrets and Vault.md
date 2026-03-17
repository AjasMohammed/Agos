---
title: Secrets and Vault
tags:
  - security
  - vault
  - reference
  - handbook
  - v3
date: 2026-03-17
status: complete
effort: 2h
priority: high
---

# Secrets and Vault

> The AgentOS vault stores sensitive credentials — API keys, passwords, tokens — with AES-256-GCM encryption and Argon2id key derivation. Agents never see raw secret values.

---

## Overview

The secrets vault is a security boundary that sits between agents and credentials. Its two core guarantees:

1. **Secrets never leave the kernel in plaintext** — agents receive opaque proxy tokens, not raw values
2. **Secrets never enter the system insecurely** — not via CLI arguments, not via environment variables, not via config files

The vault lives in `crates/agentos-vault/` and is backed by a single SQLite database (`vault.db` by default).

---

## Vault Architecture

### Encryption

Every secret stored in the vault is encrypted with **AES-256-GCM**:

- 256-bit symmetric key derived from a passphrase
- GCM mode provides both confidentiality and integrity (authenticated encryption)
- Each secret has its own random nonce — the same value stored twice produces different ciphertexts

### Key Derivation

The vault key is derived from a passphrase using **Argon2id**:

| Parameter | Value | Rationale |
|-----------|-------|-----------|
| Memory | 64 MiB | High memory cost increases attacker cost |
| Iterations | 3 | OWASP-recommended minimum |
| Parallelism | 4 lanes | Utilizes multi-core hardware |
| Output | 32 bytes | AES-256 key |
| Algorithm | Argon2id | Resistance to both side-channel and GPU attacks |

A random 32-byte salt is generated once at vault initialization and stored in the database alongside the ciphertext. This ensures that identical passphrases on different installations produce different keys.

### Memory Safety

All sensitive material uses **zero-on-drop** semantics:

- The master key is a `MasterKey` struct implementing `ZeroizeOnDrop` — its 32 key bytes are overwritten with zeros when the struct goes out of scope
- Passphrase input is stored in `ZeroizingString`, which zeroes its heap allocation on drop
- Secret values retrieved for kernel-internal use follow the same pattern

### Storage

The vault database (`vault.db`) stores per-secret:
- Name
- Encrypted value (AES-256-GCM ciphertext + nonce)
- Scope
- Owner reference
- `created_at` timestamp
- `last_used_at` timestamp

The database file must be protected by OS-level filesystem permissions — the vault encryption provides a second line of defense if the file is copied off-system.

---

## Security Guarantees

| Guarantee | How it is enforced |
|-----------|--------------------|
| Never in CLI arguments | `agentctl secret set` uses `rpassword::read_password()` — terminal echo is suppressed, value is never in the shell command string |
| Never in environment variables | Secrets are retrieved by the kernel on startup, not via `$ENV` injection |
| Never in config files | `config/default.toml` contains no secret values; vault path and params only |
| Zeroed from memory | `ZeroizeOnDrop` on `MasterKey` and `ZeroizingString` |
| Audit trail | Every secret access and vault lockdown event is written to the audit log |

---

## Setting Secrets

```bash
agentctl secret set NAME [--scope SCOPE]
```

The command prompts for the value with hidden input — the value is never visible in the shell history or process table:

```bash
agentctl secret set OPENAI_API_KEY
# Enter value for 'OPENAI_API_KEY' (input hidden): ▌
# ✅ Secret 'OPENAI_API_KEY' stored securely

agentctl secret set DB_PASSWORD --scope agent:worker
# Enter value for 'DB_PASSWORD' (input hidden): ▌
# ✅ Secret 'DB_PASSWORD' stored securely
```

**Parameters:**
- `NAME` — the secret name (e.g. `OPENAI_API_KEY`, `STRIPE_SECRET_KEY`)
- `--scope` — access scope (see [[#Secret Scopes]]); defaults to `global`

---

## Secret Scopes

Scopes control which agents and tools can access a secret. The kernel enforces scope at retrieval time.

| Scope | Format | When to use |
|-------|--------|-------------|
| `global` | `global` | Secrets accessible to all agents (e.g., shared API credentials) |
| Agent-scoped | `agent:<name>` | Secrets tied to a specific agent (e.g., the agent's own service account key) |
| Tool-scoped | `tool:<name>` | Secrets used by a specific tool (e.g., an API key only the `stripe-tool` should touch) |

### Examples

```bash
# Global — any agent can use this
agentctl secret set ANTHROPIC_API_KEY --scope global

# Scoped to the 'finance-agent' only
agentctl secret set PAYROLL_DB_PASSWORD --scope agent:finance-agent

# Scoped to the 'stripe-payment' tool only
agentctl secret set STRIPE_SECRET_KEY --scope tool:stripe-payment
```

> [!tip] Prefer Narrow Scopes
> Use the narrowest scope that works. A compromised agent with `global` scope access can reach all secrets; one with `agent:worker` scope can only reach its own.

---

## Listing Secrets

```bash
agentctl secret list
```

Lists all stored secrets — **metadata only**. Secret values are never shown:

```
NAME                      SCOPE                LAST USED
--------------------------------------------------------------
OPENAI_API_KEY            Global               2026-03-17 09:22:00 UTC
DB_PASSWORD               Agent(worker)        never
STRIPE_SECRET_KEY         Tool(stripe-payment) 2026-03-16 14:05:00 UTC
```

The `last_used_at` timestamp updates each time the kernel retrieves a secret value for an agent or tool.

---

## Rotating Secrets

Use `rotate` when a credential has been compromised or needs periodic renewal. The old value is replaced atomically — the secret is unavailable for the minimum possible time:

```bash
agentctl secret rotate NAME
```

```bash
agentctl secret rotate OPENAI_API_KEY
# Enter new value for 'OPENAI_API_KEY' (input hidden): ▌
# ✅ Secret 'OPENAI_API_KEY' rotated
```

The rotation is logged to the audit trail with a `SecretRotated` event. The scope and ownership are preserved — only the encrypted value changes.

---

## Revoking Secrets

`revoke` permanently deletes a secret from the vault:

```bash
agentctl secret revoke NAME
```

```bash
agentctl secret revoke OLD_API_KEY
# ✅ Secret 'OLD_API_KEY' revoked
```

After revocation, any agent or tool that attempts to access the secret will receive an error. The revocation event is written to the audit log.

---

## Emergency Lockdown

`lockdown` is a break-glass command for security incidents. It revokes **all active proxy tokens** and **blocks new issuance** until the kernel is restarted:

```bash
agentctl secret lockdown
```

**What it does:**
1. Invalidates all currently-issued proxy tokens (agents can no longer access any secrets)
2. Blocks the kernel from issuing new proxy tokens for any secret
3. Writes a `VaultLockdown` event to the audit log
4. Does **not** delete the encrypted values — secrets can be recovered after the kernel restarts with the correct passphrase

**When to use it:**
- A credential leak is suspected
- An agent is exhibiting unexpected behavior that may involve secret access
- An operator needs to stop all secret access immediately while investigating

> [!danger] Lockdown Stops All Agent Activity
> Any running task that requires a secret (e.g., LLM API calls) will fail immediately after lockdown. Use this command only when necessary. To restore operation, restart the kernel — the vault will reinitialize with valid proxy tokens.

---

## How Agents Access Secrets

Agents **never see raw secret values**. The flow is:

```
Agent task starts
  → Kernel issues CapabilityToken with allowed tools
  → Agent issues intent: Execute("llm-call", {model: "gpt-4"})
  → Kernel retrieves OPENAI_API_KEY from vault (decrypt with master key)
  → Kernel injects raw value into the LLM adapter HTTP request
  → Raw value is zeroed from memory after the request
  → Agent receives the LLM response — never the API key
```

The kernel decrypts secrets on demand, uses them for the minimum necessary duration, then zeroes the plaintext. Secret values do not flow through the intent system, the audit log content, or the agent's context window.

---

## Configuration

Vault settings in `config/default.toml`:

```toml
[secrets]
# Path to the SQLite vault database
vault_path = "/tmp/agentos/vault/secrets.db"
```

Argon2id key derivation parameters (64 MiB memory, 3 iterations, 4 lanes) are hardcoded for security and cannot be weakened via config.

The vault passphrase is **never** stored in config. It is provided at kernel startup:
- Via the `AGENTOS_VAULT_PASSPHRASE` environment variable (set by the systemd service or container entrypoint, not by humans)
- Or interactively when running in development mode

---

## Audit Events

All vault operations are logged to the audit trail:

| Event | Trigger |
|-------|---------|
| `SecretSet` | `agentctl secret set` |
| `SecretAccessed` | Kernel decrypts a value for agent use |
| `SecretRotated` | `agentctl secret rotate` |
| `SecretRevoked` | `agentctl secret revoke` |
| `VaultLockdown` | `agentctl secret lockdown` |

Audit entries include the secret name, scope, requesting agent/tool, and timestamp. Secret values are never written to the audit log.

---

## Related

- [[08-Security Model]] — Full security model including vault isolation (Layer 6)
- [[04-CLI Reference Complete]] — Complete CLI reference
- [[03-Architecture Overview]] — System architecture
