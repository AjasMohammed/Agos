---
title: CLI Reference
tags: [reference, cli]
---

# CLI Reference

The `agentctl` binary is the primary interface to AgentOS.

```
agentctl [--config <path>] <COMMAND>
```

## System Commands

### `start`
Boot the kernel and begin accepting connections.
```bash
agentctl start [--vault_passphrase <pass>]
```

### `status`
Show kernel status (connected agents, active tasks, uptime).
```bash
agentctl status
```

---

## Agent Management

### `agent connect`
Connect a new LLM agent to the kernel.
```bash
agentctl agent connect --provider <provider> --model <model> --name <name> [--base_url <url>]
```
Providers: `ollama`, `openai`, `anthropic`, `gemini`, `custom`

### `agent list`
List all connected agents with status.
```bash
agentctl agent list
```

### `agent disconnect`
Disconnect an agent by name.
```bash
agentctl agent disconnect <name>
```

### `agent message`
Send a direct message to an agent.
```bash
agentctl agent message <to_agent> <content>
```

### `agent messages`
View an agent's message inbox.
```bash
agentctl agent messages <agent_name> [--last <n>]
```

### `agent group`
Create an agent group for broadcasting.
```bash
agentctl agent group create --name <group_name> --members <agent1,agent2,...>
```

### `agent broadcast`
Broadcast a message to all agents in a group.
```bash
agentctl agent broadcast --group <group_name> <content>
```

---

## Task Management

### `task run`
Execute a task, optionally targeting a specific agent.
```bash
agentctl task run [--agent <name>] <prompt>
```

### `task list`
List all tasks with status.
```bash
agentctl task list
```

### `task logs`
View execution logs for a task.
```bash
agentctl task logs <task_id>
```

### `task cancel`
Cancel a running or queued task.
```bash
agentctl task cancel <task_id>
```

---

## Tool Management

### `tool list`
List all registered tools.
```bash
agentctl tool list
```

### `tool install`
Install a tool from a manifest file.
```bash
agentctl tool install <manifest_path>
```

### `tool remove`
Remove an installed tool.
```bash
agentctl tool remove <tool_name>
```

---

## Secrets Management

### `secret set`
Store a secret in the encrypted vault.
```bash
agentctl secret set --scope <scope> <name> <value>
```
Scopes: `global`, `agent:<name>`, `tool:<name>`

### `secret list`
List all secrets (metadata only, no values).
```bash
agentctl secret list
```

### `secret rotate`
Rotate a secret with a new value.
```bash
agentctl secret rotate <name> <new_value>
```

### `secret revoke`
Delete a secret from the vault.
```bash
agentctl secret revoke <name>
```

---

## Permission Management

### `perm grant`
Grant permissions to an agent.
```bash
agentctl perm grant <agent> <permission> [<permission>...] [--expires <seconds>]
```
Format: `<resource>:<ops>` where ops = r/w/x (e.g., `fs.user_data:rw`, `process.exec:x`)

### `perm revoke`
Revoke permissions from an agent.
```bash
agentctl perm revoke <agent> <permission>
```

### `perm show`
Show all permissions for an agent.
```bash
agentctl perm show <agent>
```

### `perm profile`
Manage reusable permission profiles.
```bash
agentctl perm profile create <name> <description> <perms...>
agentctl perm profile list
agentctl perm profile assign <agent> <profile_name>
```

---

## Role Management

### `role create`
Create a named role.
```bash
agentctl role create <name> <description>
```

### `role assign`
Assign a role to an agent.
```bash
agentctl role assign <agent> <role>
```

### `role list`
List all roles.
```bash
agentctl role list
```

### `role delete`
Delete a role.
```bash
agentctl role delete <name>
```

---

## Schedule Management

### `schedule create`
Create a cron-based scheduled job.
```bash
agentctl schedule create --name <name> --cron <expr> --agent <agent> --task <prompt>
```

### `schedule list` / `pause` / `resume` / `delete`
```bash
agentctl schedule list
agentctl schedule pause <id>
agentctl schedule resume <id>
agentctl schedule delete <id>
```

---

## Background Tasks

### `bg run`
Run a task in the background.
```bash
agentctl bg run [--detach] --agent <agent> <task>
```

### `bg list` / `logs` / `kill`
```bash
agentctl bg list
agentctl bg logs <task_id>
agentctl bg kill <task_id>
```

---

## Pipeline Commands

### `pipeline install`
Install a pipeline from YAML definition.
```bash
agentctl pipeline install <yaml_path>
```

### `pipeline run`
Execute a pipeline.
```bash
agentctl pipeline run --name <name> --input <string> [--detach]
```

### `pipeline status` / `logs` / `list` / `remove`
```bash
agentctl pipeline list
agentctl pipeline status --name <name> --run_id <id>
agentctl pipeline logs --name <name> --run_id <id> --step <step_id>
agentctl pipeline remove <name>
```

---

## Audit

### `audit logs`
View the audit trail.
```bash
agentctl audit logs [--limit <n>] [--severity <level>]
```
Severity levels: `info`, `warn`, `error`, `security`

---

## Permission Resource Classes

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
| `network.outbound` | Outbound HTTP |
| `network.inbound` | Inbound connections |
| `network.logs` | Network logs |
| `hardware.sensors` | Hardware sensors |
| `hardware.gpu` | GPU access |
| `cron.jobs` | Scheduled jobs |
| `agent.message` | Agent messaging |
| `agent.broadcast` | Broadcast messages |
| `context.write` | Context window (internal) |
