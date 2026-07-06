# AgentOS Security Posture

> Mapping of NIST SP 800-53 / SOC 2 controls to AgentOS security features.

---

## Control Mapping

| Control ID | Control Name | SOC 2 Category | AgentOS Feature | Location |
|------------|-------------|----------------|-----------------|----------|
| AC-2 | Account Management | CC6.1 | `EnterpriseRole` RBAC — Admin / Operator / Auditor / Agent / Viewer | `crates/agentos-capability/src/roles.rs` |
| AC-3 | Access Enforcement | CC6.1 | HMAC-SHA256 capability tokens; `PermissionSet.check()` validates every tool call | `crates/agentos-capability/src/token.rs` |
| AC-6 | Least Privilege | CC6.3 | Capability tokens scoped to minimum required permissions; `EnterpriseRole::default_allowed_tools()` | `crates/agentos-capability/src/roles.rs` |
| AC-17 | Remote Access | CC6.6 | API key auth on all REST endpoints; HMAC-signed webhook ingress | `crates/agentos-api/` |
| AU-2 | Audit Events | CC7.2 | Append-only SQLite audit log; 83+ `AuditEventType` variants covering all lifecycle events | `crates/agentos-audit/src/log.rs` |
| AU-9 | Audit Log Protection | CC7.2 | SHA-256 chained integrity hash per audit entry; immutable append-only schema | `crates/agentos-audit/src/log.rs` |
| AU-12 | Audit Record Generation | CC7.2 | Every tool execution, permission grant/denial, and vault access emits a structured audit event | `crates/agentos-kernel/src/task_executor.rs` |
| IA-5 | Authenticator Management | CC6.1 | Ed25519 agent identity keys; key generation via `agentctl identity keygen` | `crates/agentos-kernel/src/identity.rs` |
| IR-4 | Incident Handling | CC7.3 | `PendingEscalation` with 5-minute auto-expiry; `sweep_expired()` enforces approval deadlines | `crates/agentos-kernel/src/escalation.rs` |
| SC-8 | Transmission Confidentiality | CC6.7 | TLS (`tokio-rustls`) on all outbound channels; HMAC-signed webhook payloads | `crates/agentos-channels/` |
| SC-28 | Protection of Data at Rest | CC6.1 | AES-256-GCM encrypted vault; Argon2id key derivation; `ZeroizingString` for in-memory secrets | `crates/agentos-vault/src/vault.rs` |
| SI-3 | Malicious Code Protection | CC7.1 | Injection scanner (`injection_scanner.rs`) with Unicode normalisation; `<user_data>` system-prompt guard | `crates/agentos-kernel/src/injection_scanner.rs` |
| SI-10 | Information Input Validation | CC8.1 | `IntentValidator` validates every intent schema before routing; JSON Schema enforcement | `crates/agentos-kernel/src/intent_validator.rs` |

---

## Compliance Summary

| Framework | Controls Addressed | Notes |
|-----------|-------------------|-------|
| NIST SP 800-53 Rev 5 | AC-2, AC-3, AC-6, AC-17, AU-2, AU-9, AU-12, IA-5, IR-4, SC-8, SC-28, SI-3, SI-10 | 13 controls |
| SOC 2 Type II | CC6.1, CC6.3, CC6.6, CC6.7, CC7.1, CC7.2, CC7.3, CC8.1 | 8 Common Criteria |

---

## Related

- [`crates/agentos-capability/`](../../crates/agentos-capability/) — capability tokens & permission sets
- [`crates/agentos-audit/`](../../crates/agentos-audit/) — append-only, hash-chained audit log
- [`crates/agentos-vault/`](../../crates/agentos-vault/) — encrypted secrets store
- [`SECURITY.md`](../../SECURITY.md) — vulnerability disclosure policy
