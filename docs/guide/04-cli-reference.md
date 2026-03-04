# CLI Reference

`agentctl` is the command-line interface for managing AgentOS. All commands communicate with the running kernel over a Unix domain socket.

---

## Global Options

```
agentctl [--config <path>] <command>
```

| Option     | Default               | Description                           |
| ---------- | --------------------- | ------------------------------------- |
| `--config` | `config/default.toml` | Path to the kernel configuration file |

---

## `start` — Boot the Kernel

```bash
agentctl start [--vault-passphrase <passphrase>]
```

Boots the AgentOS kernel. Initializes all subsystems (vault, audit log, tool registry, bus server) and starts accepting connections.

| Option               | Description                                                                               |
| -------------------- | ----------------------------------------------------------------------------------------- |
| `--vault-passphrase` | Vault encryption passphrase. If omitted, prompts interactively (recommended for security) |

**Example:**

```bash
agentctl start
# Enter vault passphrase: ••••••••
# 🚀 Booting AgentOS kernel...
# ✅ Kernel started
```

---

## `agent` — Manage LLM Agents

### `agent connect`

Connect a new LLM agent to the kernel.

```bash
agentctl agent connect --provider <provider> --model <model> --name <name> [--base-url <url>]
```

| Option       | Description                                                                                 |
| ------------ | ------------------------------------------------------------------------------------------- |
| `--provider` | LLM provider: `ollama`, `openai`, `anthropic`, `gemini`, or a custom base URL               |
| `--model`    | Model identifier (e.g., `llama3.2`, `gpt-4o`, `claude-sonnet-4-20250514`, `gemini-1.5-pro`) |
| `--name`     | Unique human-readable name for this agent (e.g., `analyst`, `coder`)                        |
| `--base-url` | Custom API endpoint URL (for custom/self-hosted providers)                                  |

For cloud providers (OpenAI, Anthropic, Gemini), you will be prompted to enter an API key. The key is encrypted and stored in the vault.

**Examples:**

```bash
# Local Ollama
agentctl agent connect --provider ollama --model llama3.2 --name local-agent

# OpenAI
agentctl agent connect --provider openai --model gpt-4o --name researcher

# Custom OpenAI-compatible endpoint
agentctl agent connect --provider custom --model my-model --name custom-agent \
  --base-url http://localhost:8080/v1
```

### `agent list`

List all connected agents with their status.

```bash
agentctl agent list
```

### `agent disconnect`

Disconnect an agent by its UUID.

```bash
agentctl agent disconnect <agent-id>
```

---

## `task` — Manage Tasks

### `task run`

Submit a task to an agent for execution.

```bash
agentctl task run [--agent <name>] "<prompt>"
```

| Option    | Description                                                                              |
| --------- | ---------------------------------------------------------------------------------------- |
| `--agent` | Name of the agent to use. If omitted, the kernel's task router automatically selects one |

**Examples:**

```bash
agentctl task run --agent analyst "Summarize the error logs"
agentctl task run "What is 2 + 2?"
```

### `task list`

List all tasks (active and completed).

```bash
agentctl task list
```

### `task logs`

View logs for a specific task.

```bash
agentctl task logs <task-id>
```

### `task cancel`

Cancel a running task.

```bash
agentctl task cancel <task-id>
```

---

## `tool` — Manage Tools

### `tool list`

List all installed tools.

```bash
agentctl tool list
```

### `tool install`

Install a tool from a manifest file.

```bash
agentctl tool install <manifest-path>
```

### `tool remove`

Remove an installed tool.

```bash
agentctl tool remove <tool-name>
```

---

## `secret` — Manage Secrets

All secrets are encrypted with AES-256-GCM and stored in the vault. Values are never displayed.

### `secret set`

Store a new secret. You will be prompted to enter the value (hidden input).

```bash
agentctl secret set <name> [--scope <scope>]
```

| Option    | Description                                                        |
| --------- | ------------------------------------------------------------------ |
| `--scope` | Access scope: `global` (default), `agent:<name>`, or `tool:<name>` |

**Examples:**

```bash
agentctl secret set OPENAI_API_KEY
agentctl secret set SLACK_TOKEN --scope agent:notifier
agentctl secret set DB_PASSWORD --scope tool:database-query
```

### `secret list`

List all secrets (names and metadata only — values are never shown).

```bash
agentctl secret list
```

### `secret rotate`

Replace a secret's value. The old value is securely overwritten.

```bash
agentctl secret rotate <name>
```

### `secret revoke`

Permanently delete a secret.

```bash
agentctl secret revoke <name>
```

---

## `perm` — Manage Permissions

### `perm grant`

Grant a permission to an agent.

```bash
agentctl perm grant <agent-name> <permission> [--expires <duration>]
```

Permissions use the format `<resource>:<ops>` where ops are `r` (read), `w` (write), `x` (execute).

**Examples:**

```bash
agentctl perm grant analyst network.logs:r
agentctl perm grant analyst fs.user_data:rw
agentctl perm grant analyst process.list:r --expires 2h
```

### `perm revoke`

Revoke a permission from an agent.

```bash
agentctl perm revoke <agent-name> <permission>
```

### `perm show`

Show all permissions for an agent.

```bash
agentctl perm show <agent-name>
```

### `perm profile create`

Create a reusable permission profile.

```bash
agentctl perm profile create <name> --description "<desc>" --permissions "<perm1>,<perm2>,..."
```

### `perm profile delete`

Delete a permission profile.

```bash
agentctl perm profile delete <name>
```

### `perm profile list`

List all permission profiles.

```bash
agentctl perm profile list
```

### `perm profile assign`

Assign a permission profile to an agent (grants all permissions in the profile).

```bash
agentctl perm profile assign <agent-name> <profile-name>
```

---

## `role` — Manage Roles (RBAC)

### `role create`

Create a new role with description and optional permissions.

```bash
agentctl role create <name> --description "<desc>" [--permissions "<perm1>,<perm2>,..."]
```

### `role delete`

Delete a role.

```bash
agentctl role delete <name>
```

### `role list`

List all roles.

```bash
agentctl role list
```

### `role assign`

Assign a role to an agent.

```bash
agentctl role assign <agent-name> <role-name>
```

### `role revoke`

Revoke a role from an agent.

```bash
agentctl role unassign <agent-name> <role-name>
```

---

## `schedule` — Manage Scheduled Jobs

### `schedule create`

Create a recurring scheduled task (cron-like).

```bash
agentctl schedule create \
  --name <job-name> \
  --cron "<cron-expression>" \
  --agent <agent-name> \
  --task "<prompt>" \
  --permissions "<perm1>,<perm2>,..."
```

**Example:**

```bash
agentctl schedule create \
  --name daily-log-summary \
  --cron "0 0 8 * * *" \
  --agent analyst \
  --task "Summarize all application error logs from the last 24 hours" \
  --permissions "fs.app_logs:r,fs.user_data:w"
```

### `schedule list`

List all scheduled jobs.

```bash
agentctl schedule list
```

### `schedule pause`

Pause a scheduled job.

```bash
agentctl schedule pause <job-name>
```

### `schedule resume`

Resume a paused scheduled job.

```bash
agentctl schedule resume <job-name>
```

### `schedule delete`

Delete a scheduled job.

```bash
agentctl schedule delete <job-name>
```

---

## `bg` — Manage Background Tasks

### `bg run`

Start a one-shot background task.

```bash
agentctl bg run \
  --name <task-name> \
  --agent <agent-name> \
  --task "<prompt>" \
  [--detach]
```

| Option     | Description                                           |
| ---------- | ----------------------------------------------------- |
| `--detach` | Run the task in the background and return immediately |

### `bg list`

List all running background tasks.

```bash
agentctl bg list
```

### `bg logs`

View logs for a background task.

```bash
agentctl bg logs <task-name>
```

### `bg kill`

Terminate a running background task.

```bash
agentctl bg kill <task-name>
```

---

## `status` — System Status

Show the current system status: uptime, connected agents, active tasks, installed tools, and total audit entries.

```bash
agentctl status
```

---

## `audit` — Audit Logs

### `audit logs`

View recent audit log entries.

```bash
agentctl audit logs --last <count>
```

**Example:**

```bash
agentctl audit logs --last 50
```

---

## Permission Reference

The following resources can be granted to agents:

| Resource           | `r` (read)       | `w` (write) | `x` (execute)   |
| ------------------ | ---------------- | ----------- | --------------- |
| `network.logs`     | Read logs        | —           | —               |
| `network.outbound` | —                | —           | Make HTTP calls |
| `process.list`     | List processes   | —           | —               |
| `process.kill`     | —                | —           | Kill processes  |
| `fs.app_logs`      | Read app logs    | —           | —               |
| `fs.system_logs`   | Read system logs | —           | —               |
| `fs.user_data`     | Read files       | Write files | —               |
| `hardware.sensors` | Read values      | —           | —               |
| `hardware.gpu`     | Query info       | —           | Use for compute |
| `cron.jobs`        | View scheduled   | Create new  | Delete / run    |
| `memory.semantic`  | Read             | Write       | —               |
| `memory.episodic`  | Read             | —           | —               |
| `agent.message`    | Receive msgs     | —           | Send msgs       |
| `agent.broadcast`  | Receive          | —           | Broadcast       |
