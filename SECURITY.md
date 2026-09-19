# Security Policy

## Supported Versions

| Version | Supported |
|---------|-----------|
| 0.x (current) | Yes |

## Reporting a Vulnerability

**Do not open a public GitHub issue for security vulnerabilities.**

Send a report via [GitHub Security Advisories](https://github.com/AjasMohammed/Agos/security/advisories/new).

Include:
- Description of the vulnerability
- Steps to reproduce
- Affected crate(s) and version(s)
- Proposed fix (optional)

We aim to acknowledge reports within 48 hours and resolve critical issues within 7 days.

If a report goes unacknowledged for 90 days, the reporter may publish, and a GitHub Security Advisory will be issued for it even if no fix has landed. Unanswered CVEs are how agent frameworks lose trust; we would rather publish an open advisory than sit on one.

## Security Model

AgentOS is designed with security as a core requirement:

- **Capability tokens**: Every tool call requires a signed `CapabilityToken` — no ambient authority
- **Audit log**: Append-only SQLite with Merkle chain — tamper-evident, 83+ event types
- **Secrets vault**: AES-256-GCM encryption, Argon2id key derivation, `Zeroize` on drop
- **Injection scanning**: All user-controlled input is scanned before reaching tool calls
- **Path traversal blocking**: File tools reject any path containing `..`
- **SSRF blocking**: `PermissionSet` blocks internal network ranges by default
- **Tool signing**: Ed25519 manifest signatures for Community and Verified tier tools

## Trust Boundaries — what is and isn't a security boundary

We are explicit about which controls are *load-bearing boundaries* you can rely
on versus which are *hardening* (defense-in-depth that reduces risk but can be
defeated by a determined adversary). Treating hardening as a boundary is how
people get surprised.

**Load-bearing boundaries** (an adversary should not be able to cross these):

- **Capability tokens** — HMAC-SHA256, validated on every tool call. No ambient authority.
- **Encrypted vault** — AES-256-GCM + Argon2id; secrets held in `Zeroize`/`Zeroizing` and never written in plaintext.
- **Append-only audit log** — Merkle-chained SQLite; tamper-evident.
- **Trust tiers** — `Blocked` tools are hard-rejected by the kernel; Community/Verified require valid Ed25519 signatures.
- **OS-level sandbox** — seccomp-BPF (Linux) for tool execution and bubblewrap (`bwrap --unshare-all`) process isolation for `script`/`shell-exec` tools.
- **Permission enforcement** — `PermissionSet` does path-prefix matching, deny entries, SSRF/private-range blocking, and rejects any path containing `..`.

**Hardening — NOT a hard boundary** (catches mistakes, not motivated attackers):

- **Injection scanner** — heuristic pattern matching on untrusted input. It reduces prompt-injection risk; it does not guarantee prevention.
- **Interactive approval prompts** — advisory friction for risky tool calls, not an enforcement boundary on their own.
- **Output/secret redaction in logs** — best-effort scrubbing before display; do not rely on it to contain a secret.

If you find a way to cross a *load-bearing* boundary, please report it (above).
Bypassing *hardening* is expected and welcome as a hardening PR/issue, not a
vulnerability report.

**Deployment caveat:** the hardened `deploy/agentos.service` sets
`RestrictNamespaces=true` + `SystemCallFilter=@system-service`, which **blocks**
the bwrap-based `script`/`shell-exec` tools (they fail closed with EPERM — safe,
but those tools won't run). To enable them under systemd, relax both directives
as documented in the unit file, or sandbox via containers/WASM instead.

## Verifying Releases

Release binaries are published on GitHub Releases. Each artifact ships with a
detached `minisign` signature (`.sig`). The public key is committed at
`packaging/signing/agentos-release.pub` (key id `0692DEA1023C9472`):

```
RWRylDwCod6SBrcNGIz6wZsrWW5Y9o3I+OT/opftcrq4tK/KhgXvtdKl
```

To verify a download:

```bash
# minisign (https://jedisct1.github.io/minisign/) or rsign2 (cargo install rsign2)
minisign -Vm agentos-<target> -x agentos-<target>.sig -P RWRylDwCod6SBrcNGIz6wZsrWW5Y9o3I+OT/opftcrq4tK/KhgXvtdKl
# and/or check the SHA-256
sha256sum -c agentos-<target>.sha256
```

The one-line installer verifies the signature before executing the binary. An
SBOM (CycloneDX `bom.json`) is attached to every release for dependency scanning.

## Known Limitations

Honest list of what the current release does **not** defend against. Each item names the operator mitigation. Items move out of this list only when a regression test lands.

- **Single operator only.** REST scopes such as `memory:r` or `agents:r` gate the *verb*, not the *row*: any key holding a scope can read every agent's or task's data by id. Do not issue keys to mutually untrusted users. Multi-tenant isolation is out of scope before v2.
- **`operator_token` is a shared bearer secret.** Anyone holding it has full control-plane access; there is no per-operator identity or audit attribution. Mitigation: API and health servers bind `127.0.0.1` by default and the API server is off by default (`[api] enabled = false`). Expose only behind a reverse proxy that adds its own authentication. `operator_token` has no runtime rotation endpoint: change it in config and restart. Individual API keys can be revoked (`DELETE /api/v1/keys/{id}`) or rotated via `POST /api/v1/auth/refresh` when `[api] refresh_enabled = true` (off by default).
- **Vault records carry no additional authenticated data (AAD).** Ciphertexts are AES-256-GCM authenticated, but a record could be swapped for another *valid* record from the same vault by someone with write access to the SQLite file. Mitigation: file permissions on `data/`; an attacker with that access already has the audit log and config.
- **The prompt-injection scanner is a regex denylist.** It is defense-in-depth, not the boundary. The boundary is `<user_data>` wrapping in the system prompt plus tool-call approval (`RiskClass`) and capability tokens. Expect novel injections to pass the scanner.
- **Scheduler maps are bounded per agent, not globally.** `max_queued_per_agent` caps queue depth; a very large number of agents can still grow `tasks`/`child_map` until restart.
- Seccomp sandboxing is Linux-only (gated behind `#[cfg(target_os = "linux")]`); macOS/Windows degrade gracefully without it.
- WASM tool sandboxing via Wasmtime is not yet enforced for all tools.
- The HAL device quarantine workflow is partially implemented.
- Under the hardened systemd unit, bwrap `script`/`shell-exec` tools are disabled unless namespaces/syscalls are relaxed (see Deployment caveat above).
