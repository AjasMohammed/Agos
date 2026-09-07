# CLI Reference

`agentos` is the command-line interface for managing AgentOS. All commands communicate with the running kernel over a Unix domain socket.

---

## Global Options

```
agentos [--config <path>] <command>
```

| Option     | Default               | Description                           |
| ---------- | --------------------- | ------------------------------------- |
| `--config` | `config/default.toml` | Path to the kernel configuration file |

---

## `start` — Boot the Kernel

```bash
agentos start [--vault-passphrase <passphrase>]
```

Boots the AgentOS kernel. Initializes all subsystems (vault, audit log, tool registry, bus server) and starts accepting connections.

| Option               | Description                                                                               |
| -------------------- | ----------------------------------------------------------------------------------------- |
| `--vault-passphrase` | Vault encryption passphrase. If omitted, prompts interactively (recommended for security) |

**Example:**

```bash
agentos start
# Enter vault passphrase: ••••••••
# 🚀 Booting AgentOS kernel...
# ✅ Kernel started
```

---

## `agent` — Manage LLM Agents

### `agent connect`

Connect a new LLM agent to the kernel.

```bash
agentos agent connect --provider <provider> --model <model> --name <name> [--base-url <url>]
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
agentos agent connect --provider ollama --model llama3.2 --name local-agent

# OpenAI
agentos agent connect --provider openai --model gpt-4o --name researcher

# Custom OpenAI-compatible endpoint
agentos agent connect --provider custom --model my-model --name custom-agent \
  --base-url http://localhost:8080/v1
```

### `agent list`

List all connected agents with their status.

```bash
agentos agent list
```

### `agent disconnect`

Disconnect an agent by its UUID.

```bash
agentos agent disconnect <agent-id>
```

---

## `task` — Manage Tasks

### `task run`

Submit a task to an agent for execution.

```bash
agentos task run [--agent <name>] "<prompt>"
```

| Option    | Description                                                                              |
| --------- | ---------------------------------------------------------------------------------------- |
| `--agent` | Name of the agent to use. If omitted, the kernel's task router automatically selects one |

**Examples:**

```bash
agentos task run --agent analyst "Summarize the error logs"
agentos task run "What is 2 + 2?"
```

### `task list`

List all tasks (active and completed).

```bash
agentos task list
```

### `task logs`

View logs for a specific task.

```bash
agentos task logs <task-id>
```

### `task cancel`

Cancel a running task.

```bash
agentos task cancel <task-id>
```

---

## `tool` — Manage Tools

### `tool list`

List all installed tools.

```bash
agentos tool list
```

### `tool install`

Install a tool from a manifest file.

```bash
agentos tool install <manifest-path>
```

### `tool remove`

Remove an installed tool.

```bash
agentos tool remove <tool-name>
```

---

## `secret` — Manage Secrets

All secrets are encrypted with AES-256-GCM and stored in the vault. Values are never displayed.

### `secret set`

Store a new secret. You will be prompted to enter the value (hidden input).

```bash
agentos secret set <name> [--scope <scope>]
```

| Option    | Description                                                        |
| --------- | ------------------------------------------------------------------ |
| `--scope` | Access scope: `global` (default), `agent:<name>`, or `tool:<name>` |

**Examples:**

```bash
agentos secret set OPENAI_API_KEY
agentos secret set SLACK_TOKEN --scope agent:notifier
agentos secret set DB_PASSWORD --scope tool:database-query
```

### `secret list`

List all secrets (names and metadata only — values are never shown).

```bash
agentos secret list
```

### `secret rotate`

Replace a secret's value. The old value is securely overwritten.

```bash
agentos secret rotate <name>
```

### `secret revoke`

Permanently delete a secret.

```bash
agentos secret revoke <name>
```

---

## `perm` — Manage Permissions

### `perm grant`

Grant a permission to an agent.

```bash
agentos perm grant <agent-name> <permission> [--expires <duration>]
```

Permissions use the format `<resource>:<ops>` where ops are `r` (read), `w` (write), `x` (execute).

**Examples:**

```bash
agentos perm grant analyst network.logs:r
agentos perm grant analyst fs.user_data:rw
agentos perm grant analyst process.list:r --expires 2h
```

### `perm revoke`

Revoke a permission from an agent.

```bash
agentos perm revoke <agent-name> <permission>
```

### `perm show`

Show all permissions for an agent.

```bash
agentos perm show <agent-name>
```

### `perm profile create`

Create a reusable permission profile.

```bash
agentos perm profile create <name> --description "<desc>" --permissions "<perm1>,<perm2>,..."
```

### `perm profile delete`

Delete a permission profile.

```bash
agentos perm profile delete <name>
```

### `perm profile list`

List all permission profiles.

```bash
agentos perm profile list
```

### `perm profile assign`

Assign a permission profile to an agent (grants all permissions in the profile).

```bash
agentos perm profile assign <agent-name> <profile-name>
```

---

## `role` — Manage Roles (RBAC)

### `role create`

Create a new role with description and optional permissions.

```bash
agentos role create <name> --description "<desc>" [--permissions "<perm1>,<perm2>,..."]
```

### `role delete`

Delete a role.

```bash
agentos role delete <name>
```

### `role list`

List all roles.

```bash
agentos role list
```

### `role assign`

Assign a role to an agent.

```bash
agentos role assign <agent-name> <role-name>
```

### `role revoke`

Revoke a role from an agent.

```bash
agentos role unassign <agent-name> <role-name>
```

---

## `schedule` — Manage Scheduled Jobs

### `schedule create`

Create a recurring scheduled task (cron-like).

```bash
agentos schedule create \
  --name <job-name> \
  --cron "<cron-expression>" \
  --agent <agent-name> \
  --task "<prompt>" \
  --permissions "<perm1>,<perm2>,..."
```

**Example:**

```bash
agentos schedule create \
  --name daily-log-summary \
  --cron "0 0 8 * * *" \
  --agent analyst \
  --task "Summarize all application error logs from the last 24 hours" \
  --permissions "fs.app_logs:r,fs.user_data:w"
```

### `schedule list`

List all scheduled jobs.

```bash
agentos schedule list
```

### `schedule pause`

Pause a scheduled job.

```bash
agentos schedule pause <job-name>
```

### `schedule resume`

Resume a paused scheduled job.

```bash
agentos schedule resume <job-name>
```

### `schedule delete`

Delete a scheduled job.

```bash
agentos schedule delete <job-name>
```

---

## `bg` — Manage Background Tasks

### `bg run`

Start a one-shot background task.

```bash
agentos bg run \
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
agentos bg list
```

### `bg logs`

View logs for a background task.

```bash
agentos bg logs <task-name>
```

### `bg kill`

Terminate a running background task.

```bash
agentos bg kill <task-name>
```

---

## `status` — System Status

Show the current system status: uptime, connected agents, active tasks, installed tools, and total audit entries.

```bash
agentos status
```

---

## `audit` — Audit Logs

### `audit logs`

View recent audit log entries.

```bash
agentos audit logs --last <count>
```

**Example:**

```bash
agentos audit logs --last 50
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
