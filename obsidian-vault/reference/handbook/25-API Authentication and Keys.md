---
title: API Authentication and Keys
tags:
  - api
  - security
  - reference
  - handbook
date: 2026-04-02
status: complete
effort: 1h
priority: high
---

# API Authentication and Keys

> API keys are the sole authentication mechanism for the AgentOS REST API and WebSocket endpoint. This chapter covers key format, lifecycle, storage, scoping, and security practices.

---

## Key Format

All AgentOS API keys have the format:

```
agos_<64-hex-lowercase-chars>
```

For example: `agos_a3f2b8e1c7d4...` (69 characters total: 5-char prefix + 64 hex).

The key is generated from 32 random bytes (via a CSPRNG) encoded as hexadecimal. This gives 256 bits of entropy — brute-forcing a key is computationally infeasible.

---

## Key Store

API keys are managed by the `ApiKeyStore` in `agentos-api`. The store:

- Holds an **in-memory HashMap** for O(1) validation on every request
- **Persists to a SQLite database** at `data/api_keys.db` so keys survive kernel restarts
- Uses an **HMAC-SHA256 signing secret** (32 bytes, random, server-side) stored in the same database
- Validates keys by verifying the HMAC — the raw key material is never stored, only its HMAC digest

The HMAC signing secret is generated on first startup and reloaded from the database on subsequent starts. This means key validation is consistent across restarts.

---

## Key Lifecycle

### Creation

Keys are created via the `ApiKeyStore::create_key()` API (called from the kernel or setup tooling). Each key has:

| Field | Description |
|-------|-------------|
| `name` | Display name (e.g. `"CI pipeline"`, `"dev laptop"`) |
| `permissions` | List of scopes (e.g. `["agents:r", "tasks:w"]`). Empty = full access. |
| `expires_at` | Optional expiry timestamp. `None` = never expires. |
| `created_at` | Timestamp of creation |
| `last_used_at` | Updated on each successful validation (async, non-blocking) |

The full key string (`agos_<hex>`) is returned **once** at creation and never stored in plaintext. There is no way to retrieve the key material after this point — if lost, the key must be revoked and a new one created.

### Validation

On each protected API request, the middleware:

1. Extracts the `Authorization: Bearer <key>` header
2. Checks that the key starts with `agos_` and is 69 characters
3. Uses the first 16 hex chars after the prefix as a lookup key in the HashMap
4. Verifies the HMAC using **constant-time comparison** (`subtle::ConstantTimeEq`) to prevent timing attacks
5. Checks that the record is not revoked
6. Checks that `expires_at` has not passed
7. Updates `last_used_at` in the background (non-blocking)

### Revocation

A key can be revoked at any time. Revocation:

- Sets `revoked = true` in the in-memory HashMap immediately
- Persists the revocation to SQLite in the background
- Takes effect on the **next request** — there is no grace period

Revoked keys are not removed from the database, which preserves the audit trail of which keys existed. They are excluded from the `list()` API.

### Expiry

Keys with an `expires_at` timestamp are automatically rejected after that time. The check occurs on every validation, so expiry takes effect within one request of the deadline.

---

## Permission Scopes

Each key carries a list of permission scopes. An empty permissions list is treated as **full access** (used for bootstrap/admin keys). Explicit scopes follow the format `<resource>:<op>`:

| Scope | Access |
|-------|--------|
| `agents:r` | Read agents (list, get detail) |
| `agents:w` | Modify agents (connect, disconnect, permissions) |
| `tasks:r` | Read tasks (list, get, trace) |
| `tasks:w` | Run and cancel tasks |
| `tools:r` | Read tools (list, get) |
| `tools:w` | Manage tools (install, remove) |
| `secrets:r` | Read secret metadata (not values) |
| `secrets:w` | Set and revoke secrets |
| `pipelines:r` | Read pipelines |
| `pipelines:w` | Save, run, delete pipelines |
| `audit:r` | Query audit log and verify chain |
| `costs:r` | Read cost summaries |
| `notifications:r` | Read notifications |
| `notifications:w` | Respond to notifications |
| `system:r` | Read system status |
| `*:r` | Read access across all resources |
| `*:w` | Write access across all resources |

### Scope Enforcement

The permission check in each handler:

```rust
// "Empty permissions = full access" (bootstrap key)
if key.0.permissions.is_empty() {
    return Ok(());
}
// Check resource + op match
```

This means a key with `permissions = []` (not `permissions = ["*:r", "*:w"]`) is the super-admin key. Explicit wildcard scopes (`*:r`) also work but are more explicit.

---

## WebSocket Authentication

The WebSocket endpoint does not support `Authorization` headers during the HTTP upgrade (browser `WebSocket` API limitation). Instead, pass the key as a query parameter:

```
ws://localhost:8080/api/v1/ws?token=agos_<key>
```

The same validation rules apply (HMAC verify, revocation check, expiry check).

> [!warning] Query Parameter Security
> Query parameters may appear in server access logs. Use HTTPS/WSS in production to prevent key exposure in transit. For CLI tooling connecting to localhost, HTTP is acceptable.

---

## Security Best Practices

### Use the minimum required scope

Instead of a full-access key, issue scoped keys:

```
CI pipeline:    ["tasks:w", "tasks:r", "agents:r"]
Monitoring:     ["system:r", "costs:r", "audit:r"]
Orchestrator:   ["agents:rw", "tasks:rw", "pipelines:rw"]
```

### Set expiry for short-lived access

For one-off scripts or temporary integrations, set an expiry:

- Short-lived (1 hour): `expires_at = now() + 3600s`
- Session-scoped (8 hours): `expires_at = end_of_workday`

### Rotate keys periodically

Issue a new key, update consumers, then revoke the old key. Because `last_used_at` is tracked, you can verify the old key is no longer in use before revoking.

### Never commit keys to version control

Store keys in environment variables or a secrets manager. When using the AgentOS vault, store the API key as a vault secret and inject it at runtime:

```bash
export AGENTOS_API_KEY=$(agentos secret get MY_API_KEY)
```

### The bootstrap key

On **each kernel startup** with the API enabled, a fresh bootstrap key with full access (`permissions = []`) is printed directly to the console. Any bootstrap key from the previous run is automatically revoked before the new one is issued — at most one active bootstrap key exists in the database at any time.

```
╔══════════════════════════════════════════════════════════════════════╗
║  Bootstrap API key (full admin access — store securely):             ║
║  agos_a3f2b8e1c7d4...                                                ║
╚══════════════════════════════════════════════════════════════════════╝

  Example: curl -H "Authorization: Bearer agos_..." http://127.0.0.1:8080/v1/status
```

The bootstrap key is intended for initial provisioning only. Immediately issue scoped keys for your actual consumers and store the bootstrap key in a secure location (or revoke it once scoped keys are in place).

> [!warning] Bootstrap key rotates on every restart
> A new bootstrap key is issued on every `agentos start`. Any client holding the previous bootstrap key will receive `401 Unauthorized` after a restart. Use scoped per-integration keys in production — the bootstrap key is for setup only.

---

## HMAC Validation Security Properties

The key validation implementation has these security properties:

| Property | Mechanism |
|----------|-----------|
| Constant-time comparison | `subtle::ConstantTimeEq` prevents timing side-channels |
| No raw key storage | Only HMAC digest stored; plaintext key is unrecoverable |
| Fast lookup | First 16 hex chars used as lookup prefix; HMAC only computed after lookup hit |
| Restart persistence | HMAC secret stored in SQLite `meta` table; consistent across restarts |
| Thread-safe | `Arc<RwLock<StoreInner>>` for concurrent reads; write lock only for mutations |

---

## Related

- [[23-REST API Reference]] — Full REST endpoint reference with permission requirements
- [[24-WebSocket Guide]] — WebSocket connection and auth via query parameter
- [[08-Security Model]] — API authentication as Layer 8 of the security model
- [[09-Secrets and Vault]] — Storing API keys in the encrypted vault
