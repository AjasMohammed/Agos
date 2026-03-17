---
title: Advanced Operations
tags:
  - reference
  - advanced
  - operations
  - v3
date: 2026-03-17
status: complete
---

# Advanced Operations

> Reference for advanced AgentOS subsystems: Hardware Abstraction Layer (HAL), resource arbitration, context snapshots and rollback, escalation management, and agent identity.

---

## Hardware Abstraction Layer (HAL)

The HAL (`crates/agentos-hal`) provides a device registry that controls which agents can access hardware resources. All newly detected devices start in a quarantined state and must be explicitly approved before any agent can use them.

### Device Lifecycle

```
Detected → Quarantined → Approved (for specific agents)
                       → Denied   (blocked for all agents)
```

State transitions:

| Transition | Operation |
|---|---|
| Any → Quarantined | Device detected for the first time |
| Quarantined → Approved | Administrator approves the device for one or more agents |
| Approved → Quarantined | Last approved agent's access is revoked |
| Any → Denied | Administrator explicitly blocks the device |
| Denied → _(no transition)_ | Denied devices cannot be approved; re-register to reset |

### Device ID Format

Devices are identified by strings like `gpu:0`, `usb:1`, `cam:0`, `mic:0`. The identifier scheme is `<type>:<index>`.

### HAL CLI

The HAL CLI is available via `agentctl hal`:

```bash
# List all registered devices
agentctl hal list

# Show devices currently in quarantine
agentctl hal quarantine

# Approve a device for a specific agent
agentctl hal approve --device gpu:0 --agent coder

# Deny a device for all agents
agentctl hal deny --device usb:1

# Revoke an agent's access to a device
agentctl hal revoke --device cam:0 --agent researcher
```

### Audit Events

Device lifecycle changes are recorded in the audit log:

| Event | Trigger |
|---|---|
| `HardwareDeviceDetected` | Device first seen by the registry |
| `HardwareDeviceApproved` | Device approved for an agent |
| `HardwareDeviceDenied` | Device denied |
| `HardwareDeviceRevoked` | Agent's access revoked |

### Current Status

The HAL registry and per-device quarantine/approve/deny workflow are fully implemented (Spec §9). The HAL driver layer (system monitoring, process manager, network monitor, GPU metrics) is under active development. The `sys-monitor`, `hardware-info`, `process-manager`, and `network-monitor` tools interact with HAL driver data.

---

## Resource Arbitration

The `ResourceArbiter` (`crates/agentos-kernel/src/resource_arbiter.rs`) enforces shared/exclusive locking on named resources to prevent concurrent conflicts between agents running in parallel (Spec §8).

### Lock Modes

| Mode | Behaviour |
|---|---|
| `Shared` | Multiple agents can hold a shared lock simultaneously (read-only access) |
| `Exclusive` | Only one agent can hold the lock at a time (read/write access) |

A `Shared` lock blocks any new `Exclusive` request. An `Exclusive` lock blocks all other requests.

### Resource ID Format

Resources are identified by strings. Convention:

- `fs:/path/to/file` — filesystem paths
- `browser:0` — browser slot 0
- `api:<service>` — external API rate-limited slot

### FIFO Waiter Queue

When a lock cannot be immediately granted, the requesting agent is placed in a FIFO queue for that resource. When the current holder releases the lock, the next eligible waiter is woken and granted the lock. For shared locks, multiple consecutive shared waiters are woken simultaneously.

### Deadlock Detection

Before queuing any waiter, the arbiter checks for deadlock using a DFS cycle scan on the wait-for graph (`agent → agent-it-is-blocked-on`). If adding the new wait edge would create a cycle, the request is rejected with an error immediately.

Example: Agent A holds `res1`, Agent B holds `res2`. B waits on `res1` (queued). If A then tries to acquire `res2`, the wait-for graph would have A→B→A — a cycle. The arbiter detects this and returns `Err("Deadlock detected: ...")`.

**Priority-based preemption:** If the deadlocked requester has a higher priority than the current holder, the holder is preempted (its lock forcibly released) and the requester is granted the lock. This is used for high-priority system tasks that must not deadlock.

### TTL (Auto-Release)

Locks can have a TTL in seconds. A background sweep (`sweep_expired()`) runs every 10 minutes and releases any locks that have exceeded their TTL. TTL of `0` means no auto-release.

### Resource CLI

```bash
# List all currently held resource locks
agentctl resource list

# Show resource contention (waiters, blocked agents)
agentctl resource contention

# Forcibly release a specific lock
agentctl resource release --resource fs:/var/data/report.md --agent researcher

# Release all locks held by an agent
agentctl resource release-all --agent researcher
```

Output of `agentctl resource list`:

```
Resource                       Mode       Held By              TTL(s)
--------------------------------------------------------------------------
fs:/var/data/report.md         exclusive  researcher           30
fs:/var/data/summary.csv       shared     coder, analyst       0
```

---

## Snapshots and Rollback

Context snapshots save a complete serialized copy of a task's context window so it can be restored later.

### Auto-Snapshot Triggers

The kernel takes a snapshot automatically before:

1. **Write operations** — any tool execution that modifies persistent state (file writes, secret creation)
2. **Budget exhaustion** — when a task's token budget is about to be exceeded

This ensures every destructive or budget-constrained operation has a safe rollback point.

### Snapshot Expiry

A background sweep runs every 10 minutes and deletes snapshots older than 72 hours. This prevents unbounded growth of the audit database. Expired snapshots emit a `SnapshotExpired` audit event.

### Listing Snapshots

```bash
agentctl snapshot list --task <task-id>
```

Output:

```
SNAPSHOT_REF                             ACTION               SIZE         CREATED
snap_0001                                before_write         4096         1742205781
snap_0002                                budget_limit         4128         1742205892

Total: 2 snapshot(s)
```

The same data is accessible via:

```bash
agentctl audit snapshots --task <task-id>
```

### Rolling Back

```bash
# Roll back to the most recent snapshot
agentctl snapshot rollback --task <task-id>

# Roll back to a specific snapshot
agentctl snapshot rollback --task <task-id> --snapshot snap_0001
```

Also accessible via:

```bash
agentctl audit rollback --task <task-id> [--snapshot <ref>]
```

After rollback, the task context is restored to the snapshot state. The task can then resume or be resubmitted.

---

## Escalation Management

Escalations are created when a risk classifier scores an agent action at Level 3 or Level 4. The task is paused and waits for a human operator decision.

### When Escalations Are Created

- **Level 3** — high-risk action requiring human review before proceeding
- **Level 4** — critical action (e.g., destructive file operations, external API calls with irreversible effects)

The escalation record includes the task context, agent ID, the specific action being blocked, a risk summary, and available decision options.

### Auto-Expiry

Escalations that are not resolved within **5 minutes** are automatically denied. The paused task receives a rejection and can handle it as an error or retry.

### Escalation CLI

```bash
# List pending escalations
agentctl escalation list

# List all escalations including resolved ones
agentctl escalation list --all

# Show details of a specific escalation
agentctl escalation get <id>

# Resolve an escalation with a decision
agentctl escalation resolve <id> --decision "Approved"
agentctl escalation resolve <id> --decision "Denied"
agentctl escalation resolve <id> --decision "Acknowledged"
```

Output of `agentctl escalation list`:

```
ID     TASK         URGENCY    BLOCKING   STATUS   SUMMARY
----------------------------------------------------------------------
42     abc12345     high       yes        pending  Agent wants to delete all files in /var/...
43     def67890     medium     no         pending  Agent wants to call external payment API...
```

Output of `agentctl escalation get 42`:

```
Escalation #42
============================================================
Task ID:      abc12345-...
Agent ID:     coder
Reason:       High-risk file deletion in system directory
Urgency:      high
Blocking:     yes
Status:       pending

Summary:
  Agent is attempting to delete 47 files under /var/lib/...

Decision point:
  Should the agent proceed with bulk file deletion?

Options:
  - Approve
  - Deny
  - Request confirmation for each file
```

### Resolution

When an escalation is resolved, the kernel receives the decision and resumes the paused task with the decision injected into its context. The task can then act on the approval or denial.

```
Escalation #42 resolved: Approved
Task abc12345 resumed.
```

---

## Identity Management

Each agent has an Ed25519 keypair used for cryptographic identity. The keypair is generated when the agent is first connected and stored securely in the kernel.

### Viewing an Agent's Identity

```bash
agentctl identity show --agent <name>
```

Output:

```
Agent:       coder
ID:          a7f3b2c1-...
Public Key:  ed25519:3a7f9b2c4d1e...
Signing Key: present
```

The public key is safe to share. The signing key (private key) is held only in kernel memory and is never exported.

### Revoking an Identity

```bash
agentctl identity revoke --agent <name>
```

This permanently revokes the agent's cryptographic identity and all associated permissions. The agent will need to be reconnected to generate a new keypair and receive new permissions.

```
Identity and permissions revoked for agent 'coder'.
```

Revocation is useful when:

- An agent is suspected of compromise
- An agent's role changes and old permissions must be cleared
- Cleaning up a decommissioned agent

---

## Related

- [[14-Audit Log]] — audit events for all advanced operations
- [[08-Security Model]] — capability tokens and permission enforcement
- [[16-Configuration Reference]] — relevant config keys
- [[15-LLM Configuration]] — agent connection and provider setup
