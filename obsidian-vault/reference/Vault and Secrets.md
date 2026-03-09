---
title: Vault and Secrets
tags: [reference, security, vault]
---

# Vault and Secrets

The Secrets Vault provides encrypted storage for API keys, credentials, and other sensitive data.

**Source:** `crates/agentos-vault/src/vault.rs`

## Encryption

| Component | Algorithm |
|---|---|
| Key Derivation | Argon2id (memory-hard KDF) |
| Data Encryption | AES-256-GCM (authenticated) |
| Key Zeroing | `zeroize` crate |

### How It Works

1. User provides a **passphrase** at kernel boot
2. Passphrase + random salt → **Argon2id** → 256-bit master key
3. Each secret encrypted with **AES-256-GCM** using the master key
4. A **sentinel value** (encrypted test string) verifies correct passphrase
5. After use, keys are **zeroed from memory**

## Storage Schema (SQLite)

```sql
CREATE TABLE secrets (
    id TEXT PRIMARY KEY,
    name TEXT UNIQUE,
    owner TEXT,              -- "Kernel" | "Agent(<id>)" | "Tool(<id>)"
    scope TEXT,              -- "Global" | "Agent(<id>)" | "Tool(<id>)"
    encrypted_value BLOB,
    created_at TEXT,
    last_used_at TEXT
);

CREATE TABLE vault_meta (
    key TEXT PRIMARY KEY,    -- "argon2_salt", "sentinel"
    value BLOB
);
```

## Secret Scoping

| Scope | Access |
|---|---|
| `global` | All agents and tools |
| `agent:<name>` | Only the named agent |
| `tool:<name>` | Only the named tool |

## Secret Ownership

| Owner | Meaning |
|---|---|
| `Kernel` | System-level secret |
| `Agent(<id>)` | Created by/for an agent |
| `Tool(<id>)` | Created by/for a tool |

## CLI Usage

```bash
# Store a global API key
agentctl secret set --scope global openai_key sk-abc123

# Store agent-scoped secret
agentctl secret set --scope agent:analyst api_token tok-xyz

# List secrets (metadata only, never values)
agentctl secret list

# Rotate a secret
agentctl secret rotate openai_key sk-new456

# Revoke (delete) a secret
agentctl secret revoke old_key
```

## Security Properties

- Values **never** appear in logs or environment variables
- Listing secrets returns **metadata only** (name, owner, scope, dates)
- Values are **decrypted on-demand** and zeroed after use
- The vault passphrase is prompted at boot (or passed via `--vault_passphrase`)
- Salt is unique per vault instance
