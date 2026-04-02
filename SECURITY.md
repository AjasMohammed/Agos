# Security Policy

## Supported Versions

| Version | Supported |
|---------|-----------|
| 0.x (current) | Yes |

## Reporting a Vulnerability

**Do not open a public GitHub issue for security vulnerabilities.**

Send a report via [GitHub Security Advisories](https://github.com/agentos/agentos/security/advisories/new).

Include:
- Description of the vulnerability
- Steps to reproduce
- Affected crate(s) and version(s)
- Proposed fix (optional)

We aim to acknowledge reports within 48 hours and resolve critical issues within 7 days.

## Security Model

AgentOS is designed with security as a core requirement:

- **Capability tokens**: Every tool call requires a signed `CapabilityToken` — no ambient authority
- **Audit log**: Append-only SQLite with Merkle chain — tamper-evident, 83+ event types
- **Secrets vault**: AES-256-GCM encryption, Argon2id key derivation, `Zeroize` on drop
- **Injection scanning**: All user-controlled input is scanned before reaching tool calls
- **Path traversal blocking**: File tools reject any path containing `..`
- **SSRF blocking**: `PermissionSet` blocks internal network ranges by default
- **Tool signing**: Ed25519 manifest signatures for Community and Verified tier tools

## Known Limitations

- Seccomp sandboxing is Linux-only (gated behind `#[cfg(target_os = "linux")]`)
- WASM tool sandboxing via Wasmtime is not yet enforced for all tools
- The HAL device quarantine workflow is partially implemented
