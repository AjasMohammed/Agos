---
title: Kernel Mediated Capabilities Data Flow
tags:
  - kernel
  - security
  - capabilities
  - flow
date: 2026-04-12
status: complete
effort: 0.5d
priority: high
---

# Kernel Mediated Capabilities Data Flow

> How capability requests flow from agent intent through kernel mediation to system execution and back.

---

## Primary Flow: Agent → Kernel → System → Agent

```
┌─────────────────────────────────────────────────────────────────────┐
│                         AGENT CONTEXT                               │
│                                                                     │
│  LLM decides: "I need to install flask to run this web app"         │
│  Emits tool call: env-install { package: "flask", version: ">=3.0" }│
└────────────────────────┬────────────────────────────────────────────┘
                         │
                         ▼
┌─────────────────────────────────────────────────────────────────────┐
│                    TOOL EXECUTION LAYER                              │
│                    (task_executor.rs)                                │
│                                                                     │
│  1. Resolve tool: "env-install" → EnvInstallTool                    │
│  2. Permission check: token.check("env.install", Execute)?          │
│  3. Fire HookEvent::ToolPre { tool: "env-install", agent_id }       │
│     └── ApprovalHook checks RiskClass (WriteScoped)                 │
│     └── PolicyHook checks package allowlist                         │
└────────────────────────┬────────────────────────────────────────────┘
                         │
                    ┌────┴────┐
                    │ Allowed? │
                    └────┬────┘
                    ▼         ▼
               ┌────┐    ┌──────┐
               │ NO │    │ YES  │
               └──┬─┘    └──┬───┘
                  │         │
                  ▼         ▼
┌──────────────────┐  ┌──────────────────────────────────────────────┐
│ NEGOTIATION PATH │  │           CAPABILITY PROVIDER                 │
│                  │  │           (EnvProvider)                        │
│ Create           │  │                                               │
│ PendingEscalation│  │  1. Resolve agent workspace path              │
│ with context:    │  │  2. Determine package manager (pip/npm/cargo) │
│ - package name   │  │  3. Validate version constraints              │
│ - risk class     │  │  4. Execute install in workspace:             │
│ - agent id       │  │     pip install flask>=3.0                    │
│                  │  │       --target=/data/agents/<id>/env/pylib    │
│ Wait for         │  │       --no-cache-dir                         │
│ operator         │  │  5. Capture: name, version, size, deps       │
│ approval/deny    │  │  6. Write audit event (PackageInstalled)      │
│                  │  │  7. Return structured result                  │
└──────────────────┘  └──────────────────────┬───────────────────────┘
                                             │
                                             ▼
                      ┌──────────────────────────────────────────────┐
                      │              STRUCTURED RESULT                │
                      │                                               │
                      │  {                                            │
                      │    "status": "ok",                            │
                      │    "package": "flask",                        │
                      │    "version": "3.1.0",                        │
                      │    "install_path": "/data/agents/<id>/env/",  │
                      │    "dependencies_installed": 7,               │
                      │    "total_size_bytes": 4521984                │
                      │  }                                            │
                      └──────────────────────┬───────────────────────┘
                                             │
                                             ▼
                      ┌──────────────────────────────────────────────┐
                      │          AGENT CONTEXT (updated)              │
                      │                                               │
                      │  Tool result injected into context window.    │
                      │  LLM sees structured install confirmation.    │
                      │  Proceeds to next step: run pytest.           │
                      └──────────────────────────────────────────────┘
```

---

## Dynamic Capability Negotiation Flow

```
Agent requests capability it doesn't currently hold
         │
         ▼
┌───────────────────────────────────────────┐
│         CAPABILITY BROKER                  │
│         (kernel/capability_broker.rs)       │
│                                            │
│  1. Parse capability request               │
│     { domain: "env", action: "install",    │
│       resource: "flask" }                  │
│                                            │
│  2. Check static token grants              │
│     → Not in current token                 │
│                                            │
│  3. Check policy engine                    │
│     → Is "flask" in Python allowlist?      │
│                                            │
│  4a. If policy allows:                     │
│      Mint scoped ephemeral token           │
│      { env.install:x, ttl: 60s,           │
│        resource: "flask" }                 │
│      → Auto-granted, proceed               │
│                                            │
│  4b. If policy requires approval:          │
│      Create PendingEscalation              │
│      { capability: "env.install",          │
│        resource: "flask",                  │
│        agent_id, task_id,                  │
│        expires_at: now + 5min }            │
│      → Block task, await operator          │
│                                            │
│  4c. If policy denies:                     │
│      Return CapabilityDenied error         │
│      → Agent sees denial reason            │
└───────────────────────────────────────────┘
```

---

## Managed Build Flow

```
Agent: "Run cargo test in this workspace"
         │
         ▼
┌────────────────────────────────────────────┐
│          BUILD PROVIDER                     │
│                                             │
│  1. Validate workspace exists               │
│  2. Check build.run:x permission            │
│  3. Prepare execution environment:          │
│     - Landlock: write to workspace only     │
│     - cgroups v2: 2GB mem, 4 CPU, 60s      │
│     - Network: disabled (default)           │
│     - Seccomp: base + build profile         │
│  4. Spawn: bwrap ... cargo test --          │
│       --message-format=json 2>&1            │
│  5. Parse cargo test JSON output            │
│  6. Enforce output size limit (10MB)        │
│  7. Kill on timeout                         │
│  8. Audit: BuildExecuted event              │
└────────────────────┬───────────────────────┘
                     │
                     ▼
┌────────────────────────────────────────────┐
│          STRUCTURED BUILD RESULT            │
│                                             │
│  {                                          │
│    "status": "failed",                      │
│    "exit_code": 101,                        │
│    "tests": {                               │
│      "total": 42, "passed": 40,             │
│      "failed": 2, "ignored": 0             │
│    },                                       │
│    "failures": [                            │
│      {                                      │
│        "name": "test_auth_flow",            │
│        "message": "assertion failed...",    │
│        "file": "src/auth.rs",              │
│        "line": 127                          │
│      }                                      │
│    ],                                       │
│    "duration_ms": 8234,                     │
│    "memory_peak_mb": 312                    │
│  }                                          │
└────────────────────────────────────────────┘
```

---

## Managed Process Lifecycle Flow

```
Agent spawns a managed process
         │
         ▼
┌────────────────────────────────────────────┐
│          PROCESS PROVIDER                   │
│                                             │
│  1. Validate binary against allowlist       │
│  2. Check proc.spawn:x permission           │
│  3. Create cgroup for process:              │
│     /sys/fs/cgroup/agentos/<agent_id>/      │
│       <process_id>/                         │
│     memory.max = 512M                       │
│     cpu.max = 100000 100000                 │
│     pids.max = 64                           │
│  4. Spawn process in cgroup                 │
│  5. Register in agent's process table       │
│  6. Return process handle                   │
└────────────────────┬───────────────────────┘
                     │
                     ▼
┌────────────────────────────────────────────┐
│          PROCESS TABLE (per-agent)          │
│                                             │
│  Agent A:                                   │
│  ┌──────┬────────┬────────┬──────────────┐ │
│  │ PID  │ Binary │ Status │ Resources    │ │
│  ├──────┼────────┼────────┼──────────────┤ │
│  │ 1001 │ python │ Running│ 128MB / 512M │ │
│  │ 1002 │ node   │ Running│ 64MB / 256M  │ │
│  └──────┴────────┴────────┴──────────────┘ │
│                                             │
│  Agent B: (cannot see Agent A's processes)  │
│  ┌──────┬────────┬────────┬──────────────┐ │
│  │ PID  │ Binary │ Status │ Resources    │ │
│  ├──────┼────────┼────────┼──────────────┤ │
│  │ 1003 │ cargo  │ Running│ 256MB / 1G   │ │
│  └──────┴────────┴────────┴──────────────┘ │
└────────────────────────────────────────────┘
```

---

## Network Proxy Flow

```
Agent: net.http { url: "https://api.github.com/repos/...", method: "GET" }
         │
         ▼
┌────────────────────────────────────────────┐
│          NETWORK PROVIDER                   │
│                                             │
│  1. Parse destination: api.github.com:443   │
│  2. Check destination allowlist:            │
│     ✓ "*.github.com" matches               │
│  3. Check SSRF: not a private IP            │
│  4. Check rate limit: agent A has 50/min    │
│     remaining for github.com                │
│  5. Execute request via kernel HTTP client   │
│  6. Capture: status, headers, body (capped) │
│  7. Audit: NetworkRequest event             │
│  8. Return structured response              │
└────────────────────────────────────────────┘
         │
         ▼
   If destination NOT on allowlist:
         │
         ▼
┌────────────────────────────────────────────┐
│  Create PendingEscalation:                  │
│  "Agent A wants to access internal-db:5432" │
│  Operator reviews → approve/deny            │
│  If approved: add to session allowlist      │
└────────────────────────────────────────────┘
```

---

## Storage Zone Flow

```
Agent: storage.zone.create { path: "/home/user/projects/myapp", access: "rw" }
         │
         ▼
┌────────────────────────────────────────────┐
│          STORAGE PROVIDER                   │
│                                             │
│  1. Validate path against zone policy:      │
│     ✓ "/home/user/projects/*" is allowed    │
│     ✗ "/etc/*" would be denied              │
│     ✗ "../../" path traversal blocked       │
│  2. Check storage.zone.create:x permission  │
│  3. Check quota: agent has 2GB remaining    │
│  4. Create zone record:                     │
│     { zone_id, agent_id, path, access,      │
│       quota_bytes, created_at }             │
│  5. Configure Landlock for this zone:       │
│     Add write grant for /home/user/         │
│       projects/myapp/                       │
│  6. Audit: StorageZoneCreated event         │
│  7. Return zone handle                      │
└────────────────────────────────────────────┘
```

---

## Cross-Cutting: Audit Trail Granularity

```
Current audit (tool-level):
  [2026-04-12 10:00:01] ToolExecuted { tool: "shell-exec", agent: A, success: true }

KMC audit (per-resource):
  [2026-04-12 10:00:01] PackageInstalled { agent: A, package: "flask", version: "3.1.0",
                          workspace: "/data/agents/a/env/", size: 4521984 }
  [2026-04-12 10:00:02] ProcessSpawned { agent: A, binary: "python", pid: 1001,
                          cgroup: "/agentos/a/1001", memory_limit: "512M" }
  [2026-04-12 10:00:03] NetworkRequest { agent: A, dest: "api.github.com:443",
                          method: "GET", status: 200, bytes: 12345 }
  [2026-04-12 10:00:04] StorageZoneCreated { agent: A, path: "/home/user/projects/myapp",
                          access: "rw", quota: "2G" }
  [2026-04-12 10:00:05] BuildExecuted { agent: A, command: "cargo test",
                          workspace: "/home/user/projects/myapp",
                          exit_code: 0, tests_passed: 42, duration_ms: 8234 }
```

---

## Related

- [[Kernel Mediated Capabilities Plan]]
- [[Kernel Mediated Capabilities Research]]
