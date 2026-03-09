---
title: Capability and Permissions
tags: [reference, security, permissions]
---

# Capability and Permissions

AgentOS uses a dual-layer security model: **capability tokens** for per-task authorization and a **permission matrix** for resource access control.

**Source:** `crates/agentos-capability/src/`

## Capability Tokens

### How They Work

1. Kernel generates a random **256-bit signing key** at boot
2. When a task starts, the kernel **issues a token** scoped to that task
3. Every tool call must present a valid token
4. Tokens are **HMAC-SHA256 signed** - unforgeable without the key
5. Tokens are **time-limited** (TTL-based expiry)

### Token Structure

```rust
pub struct CapabilityToken {
    pub task_id: TaskID,
    pub agent_id: AgentID,
    pub allowed_tools: BTreeSet<ToolID>,
    pub allowed_intents: BTreeSet<IntentTypeFlag>,
    pub permissions: PermissionSet,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub signature: Vec<u8>,  // HMAC-SHA256
}
```

### Validation Checks

When a tool call arrives, the capability engine validates:
1. **Signature** - HMAC matches (not tampered)
2. **Expiry** - Token hasn't expired
3. **Tool allowed** - Tool ID is in `allowed_tools`
4. **Intent allowed** - Intent type is in `allowed_intents`
5. **Permissions** - Required permission bits are set

## Permission Model

### PermissionEntry

Linux-style rwx per resource:

```rust
pub struct PermissionEntry {
    pub resource: String,    // e.g., "fs.user_data"
    pub read: bool,
    pub write: bool,
    pub execute: bool,
    pub expires_at: Option<DateTime<Utc>>,
}
```

### Operations

| Operation | Description |
|---|---|
| `grant()` | Add or update a permission entry |
| `revoke()` | Clear specific permission bits |
| `check()` | Verify rwx for a resource |
| `intersect()` | Compute effective permissions (token scoping) |

### Resource Classes

| Resource | Description |
|---|---|
| `fs.user_data` | User data directory |
| `fs.system` | System files |
| `fs.app_logs` | Application logs |
| `memory.semantic` | Semantic memory store |
| `memory.episodic` | Episodic memory store |
| `process.exec` | Execute processes |
| `process.list` | List processes |
| `process.kill` | Kill processes |
| `network.outbound` | Outbound HTTP requests |
| `network.inbound` | Inbound connections |
| `hardware.sensors` | Hardware sensors |
| `hardware.gpu` | GPU access |
| `cron.jobs` | Scheduled jobs |
| `agent.message` | Agent-to-agent messaging |
| `agent.broadcast` | Broadcast messages |
| `context.write` | Context window modification |

### Permission Format

CLI format: `<resource>:<ops>`

```
fs.user_data:rw    → read + write
process.exec:x     → execute only
memory.semantic:r  → read only
network.outbound:rx → read + execute
```

## RBAC (Role-Based Access Control)

### Roles

Reusable bundles of permissions:

```bash
# Create a role
agentctl role create researcher "Can read files and search memory"

# Grant permissions to the role (via agent assigned to role)
agentctl perm grant researcher "fs.user_data:r" "memory.semantic:r"

# Assign role to agent
agentctl role assign analyst researcher
```

### Effective Permissions

An agent's effective permissions = **base role** + **assigned roles** + **direct grants**

The "base" role is automatically created with minimal `fs.user_data:rw` permissions.

## Time-Limited Permissions

```bash
# Grant permission that expires in 1 hour
agentctl perm grant analyst "network.outbound:rx" --expires 3600
```

After expiry, the permission is automatically ineffective (checked at validation time).

## Permission Profiles

Reusable permission templates:

```bash
agentctl perm profile create web-scraper "HTTP access + file write" \
  "network.outbound:rx" "fs.user_data:rw"

agentctl perm profile assign analyst web-scraper
```
