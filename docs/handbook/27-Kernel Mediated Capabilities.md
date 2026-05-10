---
title: Kernel-Mediated Capabilities
tags:
  - capabilities
  - security
  - handbook
  - reference
  - v4
date: 2026-04-12
status: complete
effort: 4h
priority: high
---

# Kernel-Mediated Capabilities (KMC)

> Agents interact with the system through kernel-mediated abstractions instead of raw OS access. The kernel validates, audits, and policy-controls every system interaction.

---

## Philosophy

Traditional agent frameworks give agents raw shell access inside Docker containers. This works but requires Docker, provides no per-action audit trail, no policy control, and fails catastrophically on prompt injection.

AgentOS takes a different approach: **bring system capabilities inside the ecosystem.** Agents declare what they need (install a package, start a server, access a directory), and the kernel decides how to provide it safely.

This follows the same pattern as Android (system services), WASI (capability handles), iOS (scoped storage), and browsers (fetch + CORS).

---

## Architecture

```
Agent Intent: "Install numpy and run pytest"
         |
         v
+-------------------------------------+
|       KERNEL CAPABILITY BROKER       |
|                                      |
|  CapabilityProvider trait             |
|  +-- EnvProvider      (env.*)        |
|  +-- StorageProvider  (storage.*)    |
|  +-- ProcessProvider  (proc.*)       |
|  +-- NetworkProvider  (net.*)        |
|  +-- BuildProvider    (build.*)      |
|                                      |
|  For each request:                   |
|  1. Validate capability token        |
|  2. Check policy (allowlists)        |
|  3. Execute via managed abstraction  |
|  4. Audit with per-resource detail   |
|  5. Return structured result         |
+-------------------------------------+
         |
         v
Agent receives: { "package": "numpy", "version": "1.26.4" }
(structured data, not raw stdout)
```

---

## The Five Capability Domains

### 1. Managed Environments (`env.*`)

Create isolated workspaces for package management. Each workspace is scoped per-agent with a specific ecosystem.

**Tools:**

| Tool | Action | Description |
|------|--------|-------------|
| `env-create` | `env.create` | Create a workspace (Python venv, Node.js node_modules, Rust target, or generic) |
| `env-install` | `env.install` | Install a package (validated against allowlist) |
| `env-list` | `env.list` | List installed packages |
| `env-destroy` | `env.destroy` | Remove workspace and all packages |

**Permissions:** `env.create:x`, `env.install:x`, `env.list:r`, `env.destroy:x`, `net.outbound:x` (for install)

**Package Policy:** Three modes per ecosystem:
- `curated` (default) -- only packages on the allowlist (in `config/allowlists/`)
- `open` -- any package allowed
- `locked` -- all packages require operator approval

**Example:**
```json
// Create a Python workspace
{ "name": "my-project", "ecosystem": "python" }

// Install a package
{ "package": "flask", "version": ">=3.0", "workspace": "my-project" }
```

### 2. Managed Storage Zones (`storage.*`)

Expand filesystem access beyond `data_dir` through policy-controlled zones.

**Tools:**

| Tool | Action | Description |
|------|--------|-------------|
| `storage-zone-create` | `storage.zone.create` | Request access to a directory |
| `storage-zone-list` | `storage.zone.list` | List active zones |
| `storage-zone-revoke` | `storage.zone.revoke` | Revoke a zone |

**Path Policy:**
- **Default allowed:** `/home/*/projects/**`, `/home/*/Desktop/**`, `/tmp/agentos-*/**`
- **Always denied:** `/etc/**`, `/root/**`, `~/.ssh/**`, `~/.gnupg/**`, `~/.aws/**`, `/var/**`, `/usr/**`, `/boot/**`, `/proc/**`, `/sys/**`
- Deny always takes precedence over allow
- Overlapping zones use longest-prefix match for access level

**Zone Access Levels:**
- `ro` (read-only) -- file tools can read but not write
- `rw` (read-write) -- full access

**File Tool Integration:** Once a zone is active, all 8 file tools (reader, writer, editor, delete, move, glob, grep, diff) automatically recognize files within the zone. Write tools enforce `ReadOnly` zones.

**Example:**
```json
{ "path": "/home/user/projects/myapp", "access": "rw" }
```

### 3. Managed Processes (`proc.*`)

Spawn, monitor, signal, and manage background processes with resource controls.

**Tools:**

| Tool | Action | Description |
|------|--------|-------------|
| `proc-spawn` | `proc.spawn` | Start a background process |
| `proc-signal` | `proc.signal` | Send signal (SIGTERM, SIGKILL, etc.) |
| `proc-output` | `proc.output` | Read recent stdout/stderr (ring buffer) |
| `proc-list` | `proc.list` | List agent's processes |
| `proc-wait` | `proc.wait` | Wait for process to exit |

**Binary Controls:**
- Agents must use **bare names** (no paths) -- `python`, not `/usr/bin/python`
- Default allowlist: python, python3, node, npm, cargo, git, make, sh, bash, curl, etc.
- Deny list (always blocked): sudo, su, rm, systemctl, iptables, chmod, etc.
- Deny takes precedence over allow

**Resource Limits:**
- Max 8 processes per agent (configurable)
- Output captured in 500-line ring buffer per process
- Wall-clock timeout (default 1 hour) -- auto-kill on expiry

**Example:**
```json
// Start a dev server
{ "binary": "python", "args": ["-m", "http.server", "8080"] }

// Check its output
{ "process_id": "proc-1", "lines": 20 }

// Stop it
{ "process_id": "proc-1", "signal": "SIGTERM" }
```

### 4. Managed Networking (`net.*`)

HTTP requests and DNS lookups through a policy-controlled proxy.

**Tools:**

| Tool | Action | Description |
|------|--------|-------------|
| `net-http` | `net.http` | Make an HTTP request |
| `net-dns` | `net.dns` | Resolve a hostname |

**SSRF Defense (7 layers):**
1. Hostname checked against deny/allow glob patterns
2. IP addresses parsed and checked for private ranges (IPv4 + IPv6)
3. IPv4-mapped IPv6 addresses caught (`[::ffff:10.0.0.1]`)
4. DNS resolution before connect -- resolved IPs checked against private ranges
5. DNS rebinding defense in `net.dns` action
6. HTTP redirects disabled (`Policy::none`) -- agents see 3xx and must follow explicitly
7. Rate limiting per agent per destination (default 60 rpm)

**Default Allowed Destinations:** `*.github.com`, `api.openai.com`, `api.anthropic.com`, `pypi.org`, `registry.npmjs.org`, `crates.io`, `*.googleapis.com`

**Default Denied:** `169.254.169.254` (cloud metadata), all RFC 1918 ranges (`10.*`, `172.16-31.*`, `192.168.*`), loopback (`127.*`), IPv6 private/link-local

**Example:**
```json
{ "url": "https://api.github.com/repos/owner/repo", "method": "GET" }
```

### 5. Managed Builds (`build.*`)

Compile code, run tests, and lint with structured output parsing.

**Tools:**

| Tool | Action | Description |
|------|--------|-------------|
| `build-run` | `build.run` | Execute a build command |
| `build-test` | `build.test` | Run tests (auto-detects ecosystem) |
| `build-lint` | `build.lint` | Run linter (auto-detects ecosystem) |

**Ecosystem Auto-Detection:**
- `Cargo.toml` present --> Rust (`cargo test`, `cargo clippy`)
- `package.json` present --> Node.js (`npm test`, `npx eslint`)
- `pyproject.toml` / `setup.py` present --> Python (`pytest`, `python -m flake8`)
- `go.mod` present --> Go (`go test ./...`, `go vet ./...`)
- `Makefile` present --> Make (`make test`)

**Structured Output:** Test results are parsed into JSON with pass/fail counts:
```json
{
  "status": "failed",
  "exit_code": 1,
  "tests": {
    "total": 42, "passed": 40, "failed": 2, "ignored": 0,
    "failures": [
      { "name": "test_auth_flow", "message": "assertion failed..." }
    ]
  },
  "duration_ms": 8234
}
```

**Command Allowlist:** Commands must match an allowed prefix at a word boundary: `cargo test`, `npm test`, `pytest`, `make`, etc.

---

## Policy Engine

The policy engine evaluates capability requests against prioritized rules.

### Profiles

| Profile | Description |
|---------|-------------|
| **Development** | Broad access -- env/build/proc domains auto-allowed, deny sensitive paths |
| **Production** | Curated allowlists for packages, escalation for unknowns |
| **Restricted** | All network denied, everything else requires approval |

### Rule Structure

```
Priority 100 (checked first): DENY /etc/** for storage.*
Priority 10: ALLOW * for env.* (development profile)
Default (no match): ESCALATE (requires operator approval)
```

Deny rules always take precedence over allow rules at the same priority.

### Dynamic Capability Negotiation

The capability broker mints ephemeral grants for runtime capability requests:
1. Agent requests a capability it doesn't hold
2. Policy engine evaluates: Allow / Deny / Escalate
3. If allowed: ephemeral grant issued (1-hour TTL, scoped to specific resource)
4. If escalated: PendingEscalation created for operator review
5. If denied: error returned immediately

Grants are per-agent, automatically swept on expiry, and revoked on agent disconnect.

---

## Security Model

### Defense in Depth

| Layer | Protection |
|-------|-----------|
| **CapabilityToken** | HMAC-SHA256 signed permission proof per task |
| **Provider Policy** | Per-domain allowlists/denylists (packages, binaries, destinations, paths) |
| **Input Validation** | Character-level allowlists for names, versions, paths (no shell metacharacters) |
| **Deny > Allow** | Deny rules checked before allow rules in every provider |
| **Per-Agent Isolation** | All data structures keyed by agent_id; agents can't see each other's resources |
| **Audit Trail** | Every capability action logged with per-resource detail |
| **SSRF Defense** | 7-layer protection including IPv6, DNS rebinding, redirect blocking |
| **No Shell Injection** | All commands use `Command::new().args()`, never `sh -c` |

### Audit Events

Every KMC action produces structured audit entries:

| Event | Description |
|-------|-------------|
| `CapabilityRequested` | Agent requested a managed capability |
| `CapabilityExecuted` | Capability action completed successfully |
| `CapabilityFailed` | Capability action failed |
| `EnvironmentCreated` / `PackageInstalled` / `EnvironmentDestroyed` | Environment lifecycle |
| `StorageZoneCreated` / `StorageZoneRevoked` | Storage zone lifecycle |
| `ManagedProcessSpawned` / `ManagedProcessSignaled` / `ManagedProcessTerminated` | Process lifecycle |
| `NetworkRequestExecuted` / `NetworkDestinationBlocked` | Network activity |
| `BuildExecuted` / `BuildFailed` | Build activity |

---

## Permissions Reference

| Permission | Tools | Description |
|-----------|-------|-------------|
| `env.create:x` | env-create | Create workspaces |
| `env.install:x` | env-install | Install packages |
| `env.list:r` | env-list | List packages |
| `env.destroy:x` | env-destroy | Destroy workspaces |
| `net.outbound:x` | env-install | Network for package downloads |
| `storage.zone.create:x` | storage-zone-create | Request filesystem zones |
| `storage.zone.list:r` | storage-zone-list | List zones |
| `storage.zone.revoke:x` | storage-zone-revoke | Revoke zones |
| `proc.spawn:x` | proc-spawn | Spawn processes |
| `proc.signal:x` | proc-signal | Signal processes |
| `proc.output:r` | proc-output | Read process output |
| `proc.list:r` | proc-list | List processes |
| `proc.wait:r` | proc-wait | Wait for processes |
| `build.run:x` | build-run | Run build commands |
| `build.test:x` | build-test | Run tests |
| `build.lint:x` | build-lint | Run linters |
| `net.http:x` | net-http | HTTP requests |
| `net.dns:r` | net-dns | DNS resolution |

The `--root` flag at agent connect grants `*:rwxqo` which includes all KMC permissions.

---

## Configuration

### Package Allowlists

Located in `config/allowlists/`:

| File | Ecosystem | Default Count |
|------|-----------|---------------|
| `python.toml` | Python | 35 packages (flask, django, numpy, pytest, etc.) |
| `nodejs.toml` | Node.js | 28 packages (express, jest, typescript, etc.) |
| `rust.toml` | Rust | 11 packages (cargo-watch, ripgrep, etc.) |

### Key Config Values

| Setting | Default | Description |
|---------|---------|-------------|
| `python_policy` | `curated` | Package policy: curated/open/locked |
| `max_zones_per_agent` | 10 | Maximum storage zones per agent |
| `max_processes_per_agent` | 8 | Maximum managed processes per agent |
| `default_rate_limit_rpm` | 60 | HTTP requests per minute per destination |
| `build_timeout_secs` | 300 | Build command timeout (5 minutes) |
| `install_timeout_secs` | 120 | Package install timeout (2 minutes) |

---

## Crate Map

| Crate | Module | Provider |
|-------|--------|----------|
| `agentos-kernel` | `capability_provider.rs` | `CapabilityProvider` trait |
| `agentos-kernel` | `capability_registry.rs` | `CapabilityRegistry` (BTreeMap) |
| `agentos-kernel` | `capability_dispatch.rs` | `KernelCapabilityDispatcher` |
| `agentos-kernel` | `capability_broker.rs` | `CapabilityBroker` (dynamic grants) |
| `agentos-kernel` | `policy_engine.rs` | `PolicyEngine` (rules + profiles) |
| `agentos-kernel` | `managed_env.rs` | `EnvProvider` |
| `agentos-kernel` | `managed_storage.rs` | `StorageProvider` + `ZoneTable` |
| `agentos-kernel` | `managed_process.rs` | `ProcessProvider` + `ProcessTable` |
| `agentos-kernel` | `managed_network.rs` | `NetworkProvider` + `RateLimiter` |
| `agentos-kernel` | `managed_build.rs` | `BuildProvider` + output parsers |
| `agentos-tools` | `kmc_tools.rs` | 17 bridge tools (macro-generated) |
| `agentos-types` | `registry_query.rs` | Cross-crate traits (`CapabilityDispatcher`, `StorageZoneQuery`) |
