# Security

Security in AgentOS is a core requirement, not a feature. This page is candid about which
controls are **load-bearing boundaries** you can rely on, versus which are **hardening** —
defense-in-depth that reduces risk but can be defeated by a determined adversary. Treating
hardening as a boundary is how people get surprised.

## Load-bearing boundaries

An adversary should not be able to cross these:

- **Capability tokens** — HMAC-SHA256, validated on **every** tool call. No ambient
  authority; agents start with zero permissions.
- **Encrypted vault** — AES-256-GCM with Argon2id key derivation. Secrets are held in
  `Zeroize`/`Zeroizing` and never written in plaintext, never placed in env vars or agent
  context.
- **Append-only audit log** — Merkle-chained SQLite; tamper-evident, 83+ event types.
- **Trust tiers** — `Blocked` tools are hard-rejected by the kernel; `Community`/`Verified`
  tools require valid Ed25519 manifest signatures. `Core` tools are distribution-trusted.
- **OS-level sandbox** — seccomp-BPF (Linux) for tool execution, and bubblewrap
  (`bwrap --unshare-all`) process isolation for the `script` / `shell-exec` tools.
- **Permission enforcement** — `PermissionSet` does path-prefix matching, deny entries,
  SSRF / private-range blocking, and rejects any path containing `..`.

## Hardening — NOT a hard boundary

These catch mistakes, not motivated attackers:

- **Injection scanner** — heuristic pattern matching on untrusted input. It reduces
  prompt-injection risk; it does not guarantee prevention.
- **Interactive approval prompts** — advisory friction for risky tool calls (see the
  `[approval]` modes in [Configuration](./configuration.md)), not an enforcement boundary on
  their own.
- **Output / secret redaction in logs** — best-effort scrubbing before display; do not rely
  on it to contain a secret.

If you find a way to cross a *load-bearing* boundary, please report it (see below).
Bypassing *hardening* is expected and welcome as a hardening PR/issue, not a vulnerability
report.

## Known limitations

- **Seccomp sandboxing is Linux-only** (gated behind `#[cfg(target_os = "linux")]`).
  macOS/Windows degrade gracefully without it — Linux is the primary target.
- WASM tool sandboxing via Wasmtime is not yet enforced for all tools.
- The HAL device quarantine workflow is partially implemented.

## The systemd / bwrap caveat

The hardened `deploy/agentos.service` sets `RestrictNamespaces=true` and
`SystemCallFilter=@system-service`, which **blocks** the bwrap-based `script` / `shell-exec`
tools — they fail closed with `EPERM` (safe, but those tools won't run). To enable them under
systemd, relax both directives as documented in the unit file, or sandbox via containers/WASM
instead. See [systemd deployment](./deploy/systemd.md).

## Verifying releases

Release binaries are published on GitHub Releases. Minisign signing is wired into the release
pipeline: from the first signed release onward, each artifact ships with a detached `minisign`
signature (`.sig`), and the release owner publishes the corresponding public key at tag time.
Once that key is published (it will live at `packaging/signing/agentos-release.pub`), verify a
download — when a `.sig` is present — with:

```bash
# Run once the signed release and its public key are published:
minisign -Vm agentos-<target> -P "$(cat packaging/signing/agentos-release.pub)"
sha256sum -c agentos-<target>.sha256
```

The [one-line installer](./quickstart.md) verifies the SHA-256 checksum (mandatory) and the
minisign signature (when a `.sig` is available) before executing the binary — it refuses to
install on a verification failure. An **SBOM** (CycloneDX `bom.json`) is attached to every
GitHub release for dependency scanning.

## Reporting a vulnerability

**Do not open a public GitHub issue for security vulnerabilities.** Send a report via
[GitHub Security Advisories](https://github.com/AjasMohammed/Agos/security/advisories/new),
including a description, reproduction steps, and the affected crate(s)/version(s). We aim to
acknowledge within 48 hours and resolve critical issues within 7 days.
