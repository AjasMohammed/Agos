---
title: Security Model
tags: [reference, security]
---

# Security Model

AgentOS implements a 7-layer security architecture. Security is a non-negotiable design principle - agents start with zero permissions and must be explicitly granted access.

## The 7 Layers

### 1. Capability-Based Access Control
Every operation requires an HMAC-SHA256 signed [[Capability and Permissions|capability token]] that is:
- **Kernel-issued** - Only the kernel can mint tokens
- **Task-scoped** - Bound to a specific task
- **Time-limited** - Automatic expiry via TTL
- **Tool-restricted** - Lists exactly which tools are allowed
- **Permission-carrying** - Embeds the agent's permission set

### 2. Tool Sandboxing
Tools execute in isolated environments:
- **seccomp-BPF** - Syscall filtering (Linux)
- **bwrap** (bubblewrap) - Filesystem isolation for shell commands
- **WASM sandbox** - Wasmtime isolation for WASM tools
- **Resource limits** - Memory, CPU time, network access per tool manifest

### 3. Intent Verification
Before any tool executes:
1. Token signature verified (HMAC-SHA256)
2. Token expiry checked
3. Tool ID in allowed set
4. Intent type in allowed set
5. Permission bits sufficient

### 4. Output Sanitization
- Tool results wrapped in typed delimiters
- Prevents prompt injection attacks
- Structured JSON responses

### 5. Immutable Audit Log
Every significant operation is recorded in an append-only SQLite database:
- 35+ event types covering all operations
- Severity levels: Info, Warn, Error, Security
- Full traceability via TraceID correlation

See [[Audit System]] for details.

### 6. Secrets Isolation
All credentials stored in an encrypted vault:
- **AES-256-GCM** authenticated encryption
- **Argon2id** key derivation (memory-hard, resistant to GPU attacks)
- Values zeroed from memory after use (`zeroize` crate)
- Never exposed in environment variables or logs

See [[Vault and Secrets]] for details.

### 7. Agent Identity Signing
- Kernel issues identity tokens to connected agents
- All operations traceable to a specific agent

## Security Properties

| Property | Implementation |
|---|---|
| Zero trust | Agents start with no permissions |
| Least privilege | Tokens scoped to specific task + tools |
| Defense in depth | 7 independent security layers |
| Unforgeable tokens | HMAC-SHA256 with kernel-only key |
| Memory safety | Rust (no buffer overflows, use-after-free) |
| Key zeroing | `zeroize` crate clears secrets from RAM |
| Path traversal protection | File tools canonicalize + validate paths |
| Command injection prevention | Null byte checks in shell-exec |
| No hardcoded secrets | Vault-only secret storage |
| Audit trail | Every operation logged immutably |

## Sandbox Details

### Tool Manifest Constraints
```toml
[sandbox]
network = false       # Block network access
fs_write = false      # Block filesystem writes
gpu = false           # Block GPU access
max_memory_mb = 128   # Memory limit
max_cpu_ms = 10000    # CPU time limit
syscalls = []         # Explicit syscall allowlist
```

### Shell Execution Isolation (bwrap)
- Read-only root filesystem mount
- Data directory mounted read-write
- Sensitive directories hidden via tmpfs (`/etc/shadow`, `/root`, etc.)
- Network optionally blocked
- seccomp-BPF filter applied

### WASM Isolation
- Wasmtime 38 runtime with cranelift JIT
- Epoch-based CPU interruption (no busy-polling)
- No host filesystem access (only via explicit capabilities)
- Pre-compiled at boot for performance
