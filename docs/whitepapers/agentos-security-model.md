# AgentOS Security Model

**Version:** 1.0  
**Date:** April 2026  
**Authors:** AgentOS Team

---

## Abstract

Autonomous AI agents pose unique security challenges: they execute code, access filesystems, call APIs, and make decisions with minimal human oversight. AgentOS is a Rust-based agent operating system that treats LLMs as untrusted, volatile CPUs and implements defense-in-depth security across seven layers. This whitepaper describes the threat model, architectural defenses, and verification mechanisms that make AgentOS suitable for enterprise deployment of autonomous agents.

---

## 1. Threat Model

AgentOS assumes the LLM is an **untrusted execution unit**. Unlike traditional software where the CPU faithfully executes instructions, an LLM may:

| Threat | Description | Real-World Impact |
|--------|-------------|-------------------|
| **Arbitrary tool execution** | Agent calls `rm -rf /`, drops database tables, or kills processes | Data loss, service outage |
| **Path traversal** | Agent reads `/etc/passwd`, `/etc/shadow`, or application secrets | Credential theft, privilege escalation |
| **Prompt injection** | Malicious input overrides the agent's system prompt | Agent becomes attacker-controlled |
| **Data exfiltration** | Agent sends sensitive data to external servers via curl/wget | Data breach, compliance violation |
| **Sandbox escape** | Agent breaks out of execution isolation | Full system compromise |
| **Secret exposure** | Agent leaks API keys, tokens, or credentials in tool output | Unauthorized access to external services |
| **Resource exhaustion** | Agent consumes unbounded compute, API calls, or storage | Cost overrun, denial of service |

**Design principle:** Every defense must work even if the LLM is actively adversarial. Security is enforced by the kernel, not by the agent's cooperation.

---

## 2. Defense-in-Depth Architecture

AgentOS implements seven layered defenses. An attack must bypass ALL layers to succeed.

```
┌──────────────────────────────────────────────────────┐
│  Layer 7: Prompt Injection Scanner                    │
│  32+ regex patterns, NFKC homoglyph normalization,   │
│  confidence-weighted scoring, code-fence suppression  │
├──────────────────────────────────────────────────────┤
│  Layer 6: Intent Validation                           │
│  JSON schema validation, tool manifest verification,  │
│  parameter sanitization                               │
├──────────────────────────────────────────────────────┤
│  Layer 5: Capability Token Enforcement                │
│  HMAC-SHA256 signed, time-limited, per-tool scoping, │
│  deny entries with SSRF blocking                      │
├──────────────────────────────────────────────────────┤
│  Layer 4: Trust Tier System                           │
│  Ed25519 manifest signing, Core/Verified/Community/   │
│  Blocked tiers with escalating isolation              │
├──────────────────────────────────────────────────────┤
│  Layer 3: Execution Sandboxing                        │
│  In-process (Core), Seccomp-BPF (Verified),          │
│  WASM/Wasmtime (Community)                            │
├──────────────────────────────────────────────────────┤
│  Layer 2: Secret Management                           │
│  AES-256-GCM vault, Argon2id KDF, ZeroizingString,  │
│  ProxyVault at tool boundary                          │
├──────────────────────────────────────────────────────┤
│  Layer 1: Audit Trail                                 │
│  Append-only SQLite, SHA-256 hash chain,             │
│  83+ event types, tamper detection                    │
└──────────────────────────────────────────────────────┘
```

---

## 3. Capability Token System

Every tool invocation in AgentOS requires a valid **CapabilityToken** — an HMAC-SHA256 signed authorization that specifies exactly what the bearer is allowed to do.

### Token Structure

```rust
pub struct CapabilityToken {
    pub token_id: String,           // Unique identifier
    pub agent_id: AgentID,          // Bound to specific agent
    pub allowed_tools: BTreeSet<String>,  // Whitelist of tool names
    pub allowed_intents: BTreeSet<String>,// Whitelist of intent types
    pub permissions: PermissionSet, // Granular permission grants + denies
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub signature: Vec<u8>,         // HMAC-SHA256 over all fields
}
```

### Enforcement Flow

1. Agent submits an intent (e.g., "read file at /tmp/data.txt")
2. Kernel extracts the CapabilityToken from the intent
3. Token signature verified against kernel's HMAC key
4. Token expiry checked against current time
5. Requested tool checked against `allowed_tools`
6. Requested resource checked against `permissions.entries`
7. Requested resource checked against `permissions.deny_entries` (deny always wins)
8. If all checks pass → tool executes in appropriate sandbox
9. If any check fails → `PermissionDenied` error + audit log entry

### Deny Entries and SSRF Protection

The `PermissionSet` supports explicit deny entries that take absolute precedence over any grant:

```rust
pub struct PermissionSet {
    pub entries: Vec<PermissionEntry>,  // Grants
    pub deny_entries: Vec<String>,       // Explicit denials
}
```

**Built-in SSRF blocking:** Network permissions automatically deny access to private IP ranges:
- `10.*`, `172.16-31.*`, `192.168.*` (RFC 1918)
- `127.*` (loopback), `::1`, `fe80:` (link-local)
- `::ffff:` (IPv4-mapped), `fd00::/8` (unique local)

Any tool attempting to contact these addresses is denied regardless of token grants.

---

## 4. Trust Tier System

Every tool in AgentOS has a **Trust Tier** that determines its execution environment:

| Tier | Signing Requirement | Execution Environment | Use Case |
|------|--------------------|-----------------------|----------|
| **Core** | Distribution-signed | In-process (no sandbox overhead) | Built-in tools (file-reader, shell-exec, memory) |
| **Verified** | Ed25519 by known publisher | Seccomp-BPF syscall filtering | Reviewed community tools |
| **Community** | Ed25519 by any author | WASM/Wasmtime isolation | Untrusted third-party tools |
| **Blocked** | N/A | Rejected at registration | Known-malicious tools |

### Manifest Signing

Tool manifests are signed using Ed25519 keypairs. The signing process:

1. Author generates a keypair: `agentos tool keygen`
2. Manifest fields are serialized to canonical JSON
3. Ed25519 signature computed over the canonical payload
4. Signature + public key embedded in the manifest TOML
5. At registration, `ToolRegistry` verifies the signature
6. Invalid signatures → `ToolSignatureInvalid` error

---

## 5. Execution Sandboxing

### Seccomp-BPF (Linux)

For `Verified` tier tools, AgentOS applies Seccomp-BPF syscall filtering:

- Only whitelisted syscalls are permitted (read, write, open, close, mmap, etc.)
- Dangerous syscalls (execve, fork, ptrace, mount) are blocked
- Violations trigger `SIGSYS` and are logged to the audit trail

### WASM/Wasmtime (Cross-Platform)

For `Community` tier tools, AgentOS executes the tool inside a WASM sandbox:

- Tool compiled to WASM module, executed via Wasmtime runtime
- No filesystem access unless explicitly granted via WASI capabilities
- No network access unless explicitly granted
- Memory-isolated from the host process
- Execution time-bounded (configurable timeout)

### Path Traversal Protection

File tools implement multi-layer path traversal blocking:

1. **Percent-decoding:** Input paths are decoded (`%2e%2e` → `..`) before any check
2. **Component rejection:** Paths containing `..` (ParentDir) components are rejected
3. **Canonicalization:** `std::fs::canonicalize()` resolves symlinks to real paths
4. **Boundary verification:** Canonical path must start with `data_dir` or an allowed workspace path
5. **Post-creation re-check:** For writes, parent directory is re-canonicalized after creation to catch symlink races

---

## 6. Secret Management

### Vault Architecture

Secrets are stored in an AES-256-GCM encrypted vault with Argon2id key derivation:

- **Encryption:** AES-256-GCM with random 96-bit nonces
- **Key derivation:** Argon2id with configurable memory cost, time cost, and parallelism
- **Memory safety:** All secret values use `ZeroizingString` (from the `zeroize` crate), which overwrites memory on drop
- **Scope isolation:** Secrets are scoped to `Agent` or `Kernel` — agents cannot read kernel-scoped secrets

### ProxyVault at Tool Boundary

Tools never receive raw secrets. Instead, the kernel provides a `ProxyVault` that:

1. Resolves secret references to temporary, scoped proxy tokens
2. Proxy tokens expire after the tool invocation completes
3. The actual secret value never appears in tool input/output
4. All vault access is logged to the audit trail

---

## 7. Audit Trail

### Append-Only Log with Hash Chain

Every security-relevant operation is recorded in an append-only SQLite database with SHA-256 hash chain integrity:

```
Entry N:
  seq: N
  prev_hash: SHA-256(Entry N-1)
  entry_hash: SHA-256(seq | prev_hash | timestamp | trace_id | event_type | ...)
  event_type: ToolRejected
  details: { tool: "shell-exec", reason: "not_allowed" }
  severity: Warning
```

**83+ event types** covering:
- Tool execution (start, complete, fail, reject)
- Permission checks (grant, deny, escalation)
- Agent lifecycle (register, deregister, state change)
- Security events (injection detected, token expired, chain tampered)
- Resource events (budget exceeded, checkpoint written, memory consolidated)

### Tamper Detection

The hash chain provides tamper detection:

- Each entry's `entry_hash` includes the previous entry's hash
- `agentos audit verify` recomputes the entire chain and reports mismatches
- If verification fails at kernel startup, an `AuditChainTampered` event is emitted
- The chain is pruning-safe — handles gaps in sequence numbers

### Verification Command

```bash
$ agentos audit verify
Audit chain verification:
  Entries checked: 1,247
  Status: INTACT
  No tampering detected.
```

---

## 8. Prompt Injection Detection

The `InjectionScanner` analyzes LLM outputs for prompt injection patterns before they reach tool execution:

### Pattern Categories

| Category | Patterns | Threat Level |
|----------|----------|-------------|
| Role override | "you are now", "new directive", "ignore previous" | High |
| System prompt exfil | "repeat your system prompt", "show me your instructions" | High |
| Delimiter injection | Fake JSON tool blocks, ChatML tokens (`<|im_start|>`) | Medium |
| Encoded payloads | Base64-encoded instructions | Medium |
| Privilege escalation | "sudo", "admin mode", "unrestricted" | Medium |
| Data exfiltration | curl/wget to external hosts | High |

### Homoglyph Defense

Attackers may use Unicode fullwidth characters (e.g., `ｉｇｎｏｒｅ` instead of `ignore`) to bypass text matching. The scanner applies **NFKC normalization** before pattern matching, collapsing fullwidth, halfwidth, and compatibility characters to their ASCII equivalents.

### Confidence Scoring

Each detected pattern contributes a weighted confidence score. The aggregate score determines the response:

- **Below threshold (0.5):** No action
- **Above threshold:** `InjectionDetected` event emitted, tool execution blocked or escalated to human review

---

## 9. Cost and Budget Enforcement

AgentOS tracks inference costs at the per-agent level:

- Each LLM call records token usage and estimated cost
- Per-agent daily budgets are enforced (configurable in micro-USD)
- When budget is exceeded, the kernel automatically downgrades the model (e.g., GPT-4o → GPT-3.5)
- Budget exhaustion triggers a `BudgetExceeded` event and blocks further inference
- Cost attribution is logged to the audit trail per inference

---

## 10. Comparison Matrix

| Security Feature | AgentOS | LangGraph | CrewAI | PydanticAI | mcp-agent |
|-----------------|---------|-----------|--------|------------|-----------|
| Capability tokens (HMAC) | Yes | No | No | No | No |
| Trust tier system (Ed25519) | Yes | No | No | No | No |
| Seccomp-BPF sandbox | Yes | No | No | No | No |
| WASM sandbox | Yes | No | No | No | No |
| Append-only audit + hash chain | Yes | Via LangSmith | No | Via Logfire | No |
| Encrypted vault (AES-256-GCM) | Yes | No | No | No | No |
| Path traversal blocking | Yes (multi-layer) | N/A | N/A | N/A | N/A |
| Prompt injection scanner | Yes (32+ patterns) | No | No | No | No |
| SSRF protection | Yes (built-in) | No | No | No | No |
| Cost budget enforcement | Yes | No | No | No | No |
| Homoglyph detection | Yes (NFKC) | No | No | No | No |
| Per-tool permission scoping | Yes | No | No | Pydantic validation | No |

---

## 11. Deployment Recommendations

1. **Always use Trust Tiers:** Set `trust_tier = "community"` for any tool not authored by your team
2. **Configure deny entries:** Explicitly deny access to internal networks, databases, and sensitive paths
3. **Enable audit verification:** Run `agentos audit verify` on a schedule (daily minimum)
4. **Set cost budgets:** Configure per-agent daily budgets to prevent runaway API costs
5. **Monitor escalations:** Review pending escalations daily — they indicate the agent encountered a decision it couldn't make safely
6. **Restrict workspace paths:** Only grant `workspace_paths` access to directories the agent genuinely needs

---

## 12. Conclusion

AgentOS provides the most comprehensive security model in the autonomous agent ecosystem. By treating the LLM as an untrusted CPU and enforcing security at the kernel level — not at the agent level — AgentOS ensures that even a compromised or adversarial LLM cannot exceed its defined boundaries. The combination of capability tokens, trust tiers, multi-layer sandboxing, encrypted secrets, and tamper-evident audit logging makes AgentOS suitable for enterprise environments where autonomous agents must operate safely alongside sensitive data and critical infrastructure.

---

*For technical details, see the source code at `crates/agentos-capability/`, `crates/agentos-sandbox/`, `crates/agentos-vault/`, and `crates/agentos-audit/`.*
