---
title: Handbook CLI Reference
tags:
  - docs
  - cli
  - v3
  - plan
date: 2026-03-13
status: planned
effort: 6h
priority: high
---

# Handbook CLI Reference

> Write the exhaustive CLI reference chapter documenting all 18 `agentctl` command groups, every subcommand, all flags/options, and usage examples.

---

## Why This Subtask
The CLI is the primary user interface to AgentOS. The existing CLI reference (`docs/guide/04-cli-reference.md`) only covers 8 of 18 command groups and is missing the V3 commands (event, cost, escalation, resource, snapshot, identity, pipeline details). This chapter must be the single authoritative source for all CLI usage.

---

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Command groups documented | 8 (`start`, `agent`, `task`, `tool`, `secret`, `perm`, `role`, `schedule`, `bg`, `status`, `audit`) | All 18: `start`, `agent`, `task`, `tool`, `secret`, `perm`, `role`, `schedule`, `bg`, `status`, `audit`, `pipeline`, `cost`, `resource`, `escalation`, `snapshot`, `event`, `identity` |
| Subcommands per group | Partial (e.g., `agent` shows 3 of 7) | Every subcommand documented |
| Flags/options | Most listed | Every flag with type, default value, and description |
| Examples | Some | At least one example per subcommand |

---

## What to Do

Read every CLI command file and extract all clap definitions. These are the authoritative source files:

| File | Command Group | Subcommands to Document |
|------|---------------|------------------------|
| `crates/agentos-cli/src/main.rs` | Global options, `start` | `--config`, `--vault-passphrase` |
| `crates/agentos-cli/src/commands/agent.rs` | `agent` | `connect`, `list`, `disconnect`, `message`, `messages`, `group create`, `broadcast` |
| `crates/agentos-cli/src/commands/task.rs` | `task` | `run`, `list`, `logs`, `cancel` |
| `crates/agentos-cli/src/commands/tool.rs` | `tool` | `list`, `install`, `remove`, `keygen`, `sign`, `verify` |
| `crates/agentos-cli/src/commands/secret.rs` | `secret` | `set`, `list`, `revoke`, `rotate`, `lockdown` |
| `crates/agentos-cli/src/commands/perm.rs` | `perm` | `grant`, `revoke`, `show`, `profile create`, `profile delete`, `profile list`, `profile assign` |
| `crates/agentos-cli/src/commands/role.rs` | `role` | `create`, `delete`, `list`, `grant`, `revoke`, `assign`, `remove` |
| `crates/agentos-cli/src/commands/schedule.rs` | `schedule` | `create`, `list`, `pause`, `resume`, `delete` |
| `crates/agentos-cli/src/commands/bg.rs` | `bg` | `run`, `list`, `logs`, `kill` |
| `crates/agentos-cli/src/commands/status.rs` | `status` | (no subcommands) |
| `crates/agentos-cli/src/commands/audit.rs` | `audit` | `logs`, `verify`, `snapshots`, `export`, `rollback` |
| `crates/agentos-cli/src/commands/pipeline.rs` | `pipeline` | `install`, `list`, `run`, `status`, `logs`, `remove` |
| `crates/agentos-cli/src/commands/cost.rs` | `cost` | `show`, `retrieval` |
| `crates/agentos-cli/src/commands/resource.rs` | `resource` | `list`, `release`, `contention`, `release-all` |
| `crates/agentos-cli/src/commands/escalation.rs` | `escalation` | `list`, `get`, `resolve` |
| `crates/agentos-cli/src/commands/snapshot.rs` | `snapshot` | `list`, `rollback` |
| `crates/agentos-cli/src/commands/event.rs` | `event` | `subscribe`, `unsubscribe`, `subscriptions list`, `subscriptions show`, `subscriptions enable`, `subscriptions disable`, `history` |
| `crates/agentos-cli/src/commands/identity.rs` | `identity` | `show`, `revoke` |

### Document format for each command group

For each command group, use this structure:

```markdown
## `<group>` -- <short description>

### `<group> <subcommand>`

<one-sentence description>

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--flag` | `string` | `""` | What it does |

**Example:**

\```bash
agentctl <group> <subcommand> --flag value
\```
```

### Important details to capture from source

For each subcommand, extract from the `#[derive(Subcommand)]` and `#[arg(...)]` attributes:
- Flag name (long and short forms)
- Type (String, bool, u64, Option<String>, etc.)
- Default value (from `default_value` or `default_value_t`)
- Whether it is required or optional
- The doc comment (/// line above the struct field)

### Global options

Document these global options from `main.rs`:
- `--config <path>` -- default: `config/default.toml`
- `--version` -- show version

### Offline vs online commands

Note that `tool keygen`, `tool sign`, and `tool verify` are offline commands that do not require a running kernel. All other commands require a kernel connection via Unix domain socket.

### Permission reference table

Include the permission reference table at the end of the chapter (reproduced from `docs/guide/04-cli-reference.md`):

| Resource | `r` | `w` | `x` |
|----------|-----|-----|-----|
| `network.logs` | Read logs | -- | -- |
| `network.outbound` | -- | -- | Make HTTP calls |
| `process.list` | List processes | -- | -- |
| `process.kill` | -- | -- | Kill processes |
| `fs.app_logs` | Read app logs | -- | -- |
| `fs.system_logs` | Read system logs | -- | -- |
| `fs.user_data` | Read files | Write files | -- |
| `hardware.sensors` | Read values | -- | -- |
| `hardware.gpu` | Query info | -- | Use for compute |
| `cron.jobs` | View scheduled | Create new | Delete / run |
| `memory.semantic` | Read | Write | -- |
| `memory.episodic` | Read | -- | -- |
| `agent.message` | Receive msgs | -- | Send msgs |
| `agent.broadcast` | Receive | -- | Broadcast |

---

## Files Changed

| File | Change |
|------|--------|
| `obsidian-vault/reference/handbook/04-CLI Reference Complete.md` | Create new |

---

## Prerequisites
[[01-foundation-chapters]] should be complete so the chapter can reference architectural concepts.

---

## Test Plan
- File exists at `obsidian-vault/reference/handbook/04-CLI Reference Complete.md`
- All 18 command groups are documented with H2 headers
- Every subcommand found in the source files has a corresponding section
- Every `#[arg]` flag is listed in the flag table for its subcommand
- At least one usage example per subcommand
- Offline commands are noted as not requiring a kernel connection

---

## Verification
```bash
# File exists
test -f obsidian-vault/reference/handbook/04-CLI\ Reference\ Complete.md

# All 18 command groups present
for cmd in start agent task tool secret perm role schedule bg status audit pipeline cost resource escalation snapshot event identity; do
  grep -q "## \`$cmd\`" obsidian-vault/reference/handbook/04-CLI\ Reference\ Complete.md || echo "MISSING: $cmd"
done
```
