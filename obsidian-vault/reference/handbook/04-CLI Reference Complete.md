---
title: Handbook CLI Reference
tags:
  - docs
  - cli
  - v3
  - reference
date: 2026-03-16
status: complete
effort: 6h
priority: high
---

# CLI Reference

> Exhaustive reference for every `agentos` command, subcommand, flag, and option — extracted directly from the clap definitions in `crates/agentos-cli/src/`.

---

## Global Options

These options apply to every `agentos` invocation.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--config <path>` | `String` | `"config/default.toml"` | Path to kernel config file |
| `--version` | flag | — | Show version information |
| `--help` | flag | — | Show help for any command or subcommand |

**Example:**

```bash
agentos --config /etc/agentos/prod.toml status
```

---

## Connection Model

Most commands require a running kernel and communicate over a Unix domain socket (configured in the config file under `[bus].socket_path`). The following commands are **offline** and do not require a kernel connection:

- `agentos tool keygen`
- `agentos tool sign`
- `agentos tool verify`
- `agentos mcp serve`
- `agentos mcp list`

All other commands will fail with a connection error if the kernel is not running.

---

## `start` — Boot the AgentOS kernel

Starts the kernel process. The kernel loads config, initializes the vault, registers tools, and begins listening on the bus socket. Blocks until Ctrl+C.

The vault passphrase is resolved from:
1. `AGENTOS_VAULT_PASSPHRASE` environment variable
2. Interactive prompt (if env var is not set)

*No additional flags.*

**Example:**

```bash
# Interactive passphrase prompt
agentos start

# Via environment variable (CI/scripts)
export AGENTOS_VAULT_PASSPHRASE="my-secret"
agentos start
```

---

## `stop` — Shut down the AgentOS kernel

Gracefully shuts down the running kernel. The kernel completes in-flight operations, closes the bus socket, and exits.

*No flags or arguments.*

**Example:**

```bash
agentos stop
```

---

## `agent` — Manage LLM agents

### `agent connect`

Connect a new LLM agent to the kernel.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--provider` | `String` | *required* | LLM provider: `ollama`, `openai`, `anthropic`, `gemini`, `custom`, or `custom:<name>` |
| `--model` | `String` | *required* | Model name (e.g. `gpt-4`, `claude-sonnet-4-6`, `llama3`) |
| `--name` | `String` | *required* | Agent display name (must be unique) |
| `--base-url` | `Option<String>` | — | Custom base URL for the LLM provider endpoint |
| `--role` | `Vec<String>` | `["general"]` | Role(s) for the agent. May be repeated. Supported: `orchestrator`, `security-monitor`, `sysops`, `memory-manager`, `tool-manager`, `general` |
| `--grant` | `Vec<String>` | `[]` | Extra permissions to grant at connect time (format: `resource:flags`). May be repeated. |
| `--test` | flag | `false` | Connect in test mode: agent receives an ecosystem-evaluation prompt asking for usability feedback |

**Example:**

```bash
agentos agent connect --provider openai --model gpt-4 --name analyst-1

agentos agent connect --provider anthropic --model claude-sonnet-4-6 --name orchestrator \
  --role orchestrator --role security-monitor

# Grant notify and interact permissions at connect time
agentos agent connect --provider anthropic --model claude-sonnet-4-6 --name worker \
  --grant user.notify:w --grant user.interact:x
```

### `agent list`

List all connected agents. Displays name, provider, and model.

*No flags.*

**Example:**

```bash
agentos agent list
```

### `agent disconnect`

Disconnect an agent by name.

| Argument | Type | Description |
|----------|------|-------------|
| `name` | `String` | Agent name to disconnect |

**Example:**

```bash
agentos agent disconnect analyst-1
```

### `agent message`

Send a direct message from one agent to another.

| Flag / Argument | Type | Description |
|-----------------|------|-------------|
| `--from` | `String` | Sender agent name |
| `to` | `String` | Target agent name (positional) |
| `content` | `String` | Message content (positional) |

**Example:**

```bash
agentos agent message --from orchestrator analyst-1 "Summarize the latest logs"
```

### `agent messages`

List recent messages for an agent.

| Flag / Argument | Type | Default | Description |
|-----------------|------|---------|-------------|
| `agent` | `String` | *required* | Agent name (positional) |
| `--last` | `u32` | `10` | Number of recent messages to show |

**Example:**

```bash
agentos agent messages analyst-1 --last 25
```

### `agent group create`

Create a named agent group for broadcast messaging.

| Flag / Argument | Type | Description |
|-----------------|------|-------------|
| `name` | `String` | Group name (positional) |
| `--members` | `String` | Comma-separated list of agent names |

**Example:**

```bash
agentos agent group create analysts --members "analyst-1,analyst-2,analyst-3"
```

### `agent broadcast`

Broadcast a message to all agents in a group.

| Flag / Argument | Type | Description |
|-----------------|------|-------------|
| `--from` | `String` | Sender agent name |
| `group` | `String` | Target group name (positional) |
| `content` | `String` | Message content (positional) |

**Example:**

```bash
agentos agent broadcast --from orchestrator analysts "Begin analysis phase"
```

### `agent memory show`

Show the current context memory for an agent.

| Argument | Type | Description |
|----------|------|-------------|
| `agent` | `String` | Agent name (positional) |

**Example:**

```bash
agentos agent memory show analyst-1
```

### `agent memory history`

Show the context memory version history for an agent.

| Flag / Argument | Type | Default | Description |
|-----------------|------|---------|-------------|
| `agent` | `String` | *required* | Agent name (positional) |
| `--limit` | `u32` | — | Maximum number of history entries to show |

**Example:**

```bash
agentos agent memory history analyst-1
agentos agent memory history analyst-1 --limit 10
```

### `agent memory rollback`

Rollback an agent's context memory to a specific version.

| Flag / Argument | Type | Description |
|-----------------|------|-------------|
| `agent` | `String` | Agent name (positional) |
| `--version` | `u64` | Version number to rollback to |

**Example:**

```bash
agentos agent memory rollback analyst-1 --version 3
```

### `agent memory clear`

Clear an agent's context memory entirely.

| Argument | Type | Description |
|----------|------|-------------|
| `agent` | `String` | Agent name (positional) |

**Example:**

```bash
agentos agent memory clear analyst-1
```

### `agent memory set`

Set an agent's context memory from a file.

| Flag / Argument | Type | Description |
|-----------------|------|-------------|
| `agent` | `String` | Agent name (positional) |
| `--file` | `String` | Path to a file containing the context memory content |

**Example:**

```bash
agentos agent memory set analyst-1 --file memory-snapshot.json
```

---

## `task` — Manage tasks

### `task run`

Submit a task prompt for execution. If `--agent` is omitted, the kernel auto-routes to the best available agent.

| Flag / Argument | Type | Default | Description |
|-----------------|------|---------|-------------|
| `--agent` | `Option<String>` | — | Agent name to assign the task to. Omit for auto-routing |
| `prompt` | `String` | *required* | The task prompt (positional) |

**Example:**

```bash
# Auto-routed
agentos task run "Summarize the server logs from today"

# Assigned to a specific agent
agentos task run --agent analyst-1 "Find anomalies in the auth logs"
```

### `task list`

List all tasks with their ID, state, assigned agent, and prompt preview.

*No flags.*

**Example:**

```bash
agentos task list
```

### `task logs`

View the execution logs for a specific task.

| Argument | Type | Description |
|----------|------|-------------|
| `task_id` | `String` | Task UUID |

**Example:**

```bash
agentos task logs a3b2c1d0-1234-5678-9abc-def012345678
```

### `task trace`

Show the execution trace for a completed task. Includes tool calls, LLM inferences, and timing data.

| Flag / Argument | Type | Default | Description |
|-----------------|------|---------|-------------|
| `task_id` | `String` | *required* | Task UUID (positional) |
| `--json` | flag | `false` | Output the trace as JSON |
| `--iter` | `Option<u32>` | — | Filter to a specific iteration number |

**Example:**

```bash
agentos task trace a3b2c1d0-1234-5678-9abc-def012345678
agentos task trace a3b2c1d0-1234-5678-9abc-def012345678 --json
agentos task trace a3b2c1d0-1234-5678-9abc-def012345678 --iter 2
```

### `task traces`

List recent task execution traces.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--limit` | `u32` | — | Maximum number of traces to show |
| `--agent` | `Option<String>` | — | Filter traces by agent name |

**Example:**

```bash
agentos task traces
agentos task traces --limit 20
agentos task traces --agent analyst-1
```

### `task cancel`

Cancel a running task.

| Argument | Type | Description |
|----------|------|-------------|
| `task_id` | `String` | Task UUID |

**Example:**

```bash
agentos task cancel a3b2c1d0-1234-5678-9abc-def012345678
```

---

## `tool` — Manage tools

### `tool list`

List all installed tools with name, version, trust tier, and description.

*No flags.*

**Example:**

```bash
agentos tool list
```

### `tool install`

Install a tool from its manifest file. The kernel validates the trust tier and signature before registration.

| Argument | Type | Description |
|----------|------|-------------|
| `path` | `String` | Path to the tool manifest (`.toml`) |

**Example:**

```bash
agentos tool install tools/user/my-tool.toml
```

### `tool remove`

Remove an installed tool by name.

| Argument | Type | Description |
|----------|------|-------------|
| `name` | `String` | Tool name to remove |

**Example:**

```bash
agentos tool remove my-tool
```

### `tool keygen` (offline)

Generate a new Ed25519 keypair for tool signing. **Does not require a running kernel.**

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--output` | `String` | `"tool-keypair.json"` | Path to write the keypair JSON file |

**Example:**

```bash
agentos tool keygen --output my-keys.json
```

The output file contains `pubkey`, `seed`, and `algorithm` fields. Keep the seed secret — only distribute the public key.

### `tool sign` (offline)

Sign a tool manifest with an Ed25519 private key. **Does not require a running kernel.** Performs a self-verification after signing.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--manifest` | `String` | *required* | Path to the tool manifest (`.toml`) to sign |
| `--key` | `String` | *required* | Path to keypair JSON file produced by `tool keygen` |
| `--output` | `Option<String>` | — | Write signed manifest here. Defaults to overwriting the source manifest |

**Example:**

```bash
agentos tool sign --manifest tools/user/my-tool.toml --key my-keys.json

# Write to a separate file
agentos tool sign --manifest my-tool.toml --key my-keys.json --output my-tool-signed.toml
```

### `tool verify` (offline)

Verify the Ed25519 signature on a tool manifest. **Does not require a running kernel.** Exits with code 1 if verification fails.

| Argument | Type | Description |
|----------|------|-------------|
| `manifest` | `String` | Path to the tool manifest (`.toml`) to verify |

**Example:**

```bash
agentos tool verify tools/user/my-tool.toml
```

---

## `secret` — Manage secrets

Secrets are stored in an AES-256-GCM encrypted vault with Argon2id key derivation. Values are never displayed in CLI output.

### `secret set`

Store a secret. The value is entered interactively (hidden input) — never passed as a shell argument.

| Flag / Argument | Type | Default | Description |
|-----------------|------|---------|-------------|
| `name` | `String` | *required* | Secret name, e.g. `OPENAI_API_KEY` (positional) |
| `--scope` | `String` | `"global"` | Access scope: `global`, `agent:<name>`, or `tool:<name>` |

**Example:**

```bash
agentos secret set OPENAI_API_KEY
agentos secret set SLACK_TOKEN --scope agent:notifier
```

### `secret list`

List all stored secrets (metadata only — values are never shown).

*No flags.*

**Example:**

```bash
agentos secret list
```

### `secret revoke`

Delete a secret from the vault.

| Argument | Type | Description |
|----------|------|-------------|
| `name` | `String` | Secret name to revoke |

**Example:**

```bash
agentos secret revoke OLD_API_KEY
```

### `secret rotate`

Replace a secret's value. The new value is entered interactively (hidden input).

| Argument | Type | Description |
|----------|------|-------------|
| `name` | `String` | Secret name to rotate |

**Example:**

```bash
agentos secret rotate OPENAI_API_KEY
```

### `secret lockdown`

Emergency vault lockdown: revokes all proxy tokens and blocks new issuance. Use in security incidents.

*No flags.*

**Example:**

```bash
agentos secret lockdown
```

---

## `perm` — Manage agent permissions

### `perm grant`

Grant a permission to an agent. Optionally set an expiration.

| Flag / Argument | Type | Default | Description |
|-----------------|------|---------|-------------|
| `agent` | `String` | *required* | Agent name (positional) |
| `permission` | `String` | *required* | Permission string, e.g. `fs.user_data:rw` (positional) |
| `--expires` | `Option<u64>` | — | Expiration time in seconds from now |

**Example:**

```bash
agentos perm grant analyst-1 fs.user_data:rw
agentos perm grant worker network.outbound:x --expires 3600
```

### `perm revoke`

Revoke a permission from an agent.

| Flag / Argument | Type | Description |
|-----------------|------|-------------|
| `agent` | `String` | Agent name (positional) |
| `permission` | `String` | Permission string (positional) |

**Example:**

```bash
agentos perm revoke analyst-1 fs.user_data:rw
```

### `perm show`

Show all permissions currently held by an agent.

| Argument | Type | Description |
|----------|------|-------------|
| `agent` | `String` | Agent name |

**Example:**

```bash
agentos perm show analyst-1
```

### `perm profile create`

Create a reusable permission profile.

| Argument | Type | Description |
|----------|------|-------------|
| `name` | `String` | Profile name (positional) |
| `description` | `String` | Profile description (positional) |
| `permissions` | `Vec<String>` | Permission strings (positional, variadic) |

**Example:**

```bash
agentos perm profile create reader "Read-only access" fs.user_data:r fs.app_logs:r
```

### `perm profile delete`

Delete a permission profile.

| Argument | Type | Description |
|----------|------|-------------|
| `name` | `String` | Profile name |

**Example:**

```bash
agentos perm profile delete reader
```

### `perm profile list`

List all permission profiles.

*No flags.*

**Example:**

```bash
agentos perm profile list
```

### `perm profile assign`

Assign a permission profile to an agent. Grants all permissions defined in the profile.

| Argument | Type | Description |
|----------|------|-------------|
| `agent_name` | `String` | Agent name (positional) |
| `profile_name` | `String` | Profile name (positional) |

**Example:**

```bash
agentos perm profile assign analyst-1 reader
```

---

## `role` — Manage OS roles

Roles are named bundles of permissions. Create roles, assign permissions to them, then assign roles to agents.

### `role create`

Create a new role.

| Flag / Argument | Type | Default | Description |
|-----------------|------|---------|-------------|
| `name` | `String` | *required* | Role name (positional) |
| `--description` | `String` | `""` | Role description |

**Example:**

```bash
agentos role create log-reader --description "Can read all log files"
```

### `role delete`

Delete a role.

| Argument | Type | Description |
|----------|------|-------------|
| `name` | `String` | Role name |

**Example:**

```bash
agentos role delete log-reader
```

### `role list`

List all roles with their descriptions and permissions.

*No flags.*

**Example:**

```bash
agentos role list
```

### `role grant`

Grant a permission to a role.

| Argument | Type | Description |
|----------|------|-------------|
| `role` | `String` | Role name (positional) |
| `permission` | `String` | Permission string (positional) |

**Example:**

```bash
agentos role grant log-reader fs.app_logs:r
agentos role grant log-reader fs.system_logs:r
```

### `role revoke`

Revoke a permission from a role.

| Argument | Type | Description |
|----------|------|-------------|
| `role` | `String` | Role name (positional) |
| `permission` | `String` | Permission string (positional) |

**Example:**

```bash
agentos role revoke log-reader fs.system_logs:r
```

### `role assign`

Assign a role to an agent.

| Argument | Type | Description |
|----------|------|-------------|
| `agent` | `String` | Agent name (positional) |
| `role` | `String` | Role name (positional) |

**Example:**

```bash
agentos role assign analyst-1 log-reader
```

### `role remove`

Remove a role from an agent.

| Argument | Type | Description |
|----------|------|-------------|
| `agent` | `String` | Agent name (positional) |
| `role` | `String` | Role name (positional) |

**Example:**

```bash
agentos role remove analyst-1 log-reader
```

---

## `schedule` — Manage scheduled background jobs

### `schedule create`

Create a recurring job on a cron schedule.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--name` | `String` | *required* | Schedule name |
| `--cron` | `String` | *required* | Cron expression (6-field: `sec min hour day month weekday`) |
| `--agent` | `String` | *required* | Agent name to execute the task |
| `--task` | `String` | *required* | Task prompt/description |
| `--permissions` | `String` | `""` | Comma-separated permissions for the task |

**Example:**

```bash
agentos schedule create \
  --name daily-report \
  --cron "0 0 * * * *" \
  --agent analyst \
  --task "Summarize today's logs" \
  --permissions "fs.app_logs:r,fs.system_logs:r"
```

### `schedule list`

List all scheduled jobs with their cron expression, agent, state, next run time, and run count.

*No flags.*

**Example:**

```bash
agentos schedule list
```

### `schedule pause`

Pause a scheduled job (prevents next execution).

| Argument | Type | Description |
|----------|------|-------------|
| `name` | `String` | Schedule name |

**Example:**

```bash
agentos schedule pause daily-report
```

### `schedule resume`

Resume a paused scheduled job.

| Argument | Type | Description |
|----------|------|-------------|
| `name` | `String` | Schedule name |

**Example:**

```bash
agentos schedule resume daily-report
```

### `schedule delete`

Delete a scheduled job permanently.

| Argument | Type | Description |
|----------|------|-------------|
| `name` | `String` | Schedule name |

**Example:**

```bash
agentos schedule delete daily-report
```

---

## `bg` — Manage background tasks

One-shot detached tasks that run independently of the CLI session.

### `bg run`

Launch a one-shot background task.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--name` | `String` | *required* | Background task name |
| `--agent` | `String` | *required* | Agent name to run the task |
| `--task` | `String` | *required* | Task prompt/description |
| `--detach` | `bool` | `false` | Detach the task immediately (return control to shell) |

**Example:**

```bash
agentos bg run --name process-data --agent worker --task "Process all CSV files" --detach
```

### `bg list`

List all background tasks with name, agent, state, start time, and completion time.

*No flags.*

**Example:**

```bash
agentos bg list
```

### `bg logs`

View logs for a background task.

| Flag / Argument | Type | Default | Description |
|-----------------|------|---------|-------------|
| `name` | `String` | *required* | Background task name (positional) |
| `--follow` | `bool` | `false` | Follow logs continuously (like `tail -f`) |

**Example:**

```bash
agentos bg logs process-data
agentos bg logs process-data --follow
```

### `bg kill`

Kill a running background task.

| Argument | Type | Description |
|----------|------|-------------|
| `name` | `String` | Background task name |

**Example:**

```bash
agentos bg kill process-data
```

---

## `status` — Show system status

Displays kernel uptime, connected agent count, active tasks, installed tools, and total audit entries. No subcommands.

*No flags.*

**Example:**

```bash
agentos status
```

---

## `audit` — View audit logs

The audit system uses an append-only SQLite log with Merkle hash chain integrity.

### `audit logs`

View recent audit log entries.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--last` | `u32` | `50` | Number of recent entries to show |

**Example:**

```bash
agentos audit logs --last 100
```

### `audit verify`

Verify the Merkle hash chain integrity of the audit log. Detects any tampering or corruption.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--from` | `Option<i64>` | — | Start verification from this sequence number (default: beginning) |

**Example:**

```bash
agentos audit verify
agentos audit verify --from 1000
```

### `audit snapshots`

List context snapshots for a specific task.

| Flag | Type | Description |
|------|------|-------------|
| `--task` | `String` | Task UUID |

**Example:**

```bash
agentos audit snapshots --task a3b2c1d0-1234-5678-9abc-def012345678
```

### `audit export`

Export the full audit chain as JSONL.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--limit` | `Option<u32>` | — | Maximum number of entries to export |
| `--output` | `Option<String>` | — | Write to file instead of stdout |

**Example:**

```bash
agentos audit export --output audit-dump.jsonl
agentos audit export --limit 500
```

### `audit rollback`

Roll back a task's context to a saved snapshot.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--task` | `String` | *required* | Task UUID |
| `--snapshot` | `Option<String>` | — | Snapshot reference (e.g. `snap_0001`). Defaults to most recent |

**Example:**

```bash
agentos audit rollback --task a3b2c1d0-... --snapshot snap_0003
```

---

## `pipeline` — Manage multi-agent pipelines

Pipelines are multi-step workflows defined in YAML files and executed by the pipeline engine.

### `pipeline install`

Install a pipeline from a YAML definition file.

| Argument | Type | Description |
|----------|------|-------------|
| `path` | `String` | Path to the pipeline YAML file |

**Example:**

```bash
agentos pipeline install pipelines/data-processing.yaml
```

### `pipeline list`

List all installed pipelines with name, version, step count, and description.

*No flags.*

**Example:**

```bash
agentos pipeline list
```

### `pipeline run`

Execute a pipeline with an input string.

| Flag / Argument | Type | Default | Description |
|-----------------|------|---------|-------------|
| `name` | `String` | *required* | Pipeline name (positional) |
| `--input` | `String` | *required* | Input string for the pipeline |
| `--detach` | `bool` | `false` | Run in background (detached) |

**Example:**

```bash
agentos pipeline run data-processing --input "Process Q1 reports"
agentos pipeline run data-processing --input "Process Q1 reports" --detach
```

### `pipeline status`

Get the status of a specific pipeline run, including per-step results.

| Flag / Argument | Type | Description |
|-----------------|------|-------------|
| `name` | `String` | Pipeline name (positional) |
| `--run-id` | `String` | Run UUID |

**Example:**

```bash
agentos pipeline status data-processing --run-id abc123
```

### `pipeline logs`

View step-level logs for a pipeline run.

| Flag / Argument | Type | Description |
|-----------------|------|-------------|
| `name` | `String` | Pipeline name (positional) |
| `--run-id` | `String` | Run UUID |
| `--step` | `String` | Step ID to view logs for |

**Example:**

```bash
agentos pipeline logs data-processing --run-id abc123 --step parse-csv
```

### `pipeline remove`

Remove an installed pipeline.

| Argument | Type | Description |
|----------|------|-------------|
| `name` | `String` | Pipeline name |

**Example:**

```bash
agentos pipeline remove data-processing
```

---

## `cost` — View agent cost and budget reports

### `cost show`

Show cost report with token usage, USD cost, and tool calls — per agent and totals.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--agent` | `Option<String>` | — | Filter to a specific agent. Omit for all agents |

**Example:**

```bash
agentos cost show
agentos cost show --agent analyst-1
```

### `cost retrieval`

Show retrieval refresh/reuse efficiency metrics — how often the kernel refreshes vs reuses cached context.

*No flags.*

**Example:**

```bash
agentos cost retrieval
```

---

## `resource` — Manage resource locks (arbitration)

The resource arbiter prevents concurrent access conflicts between agents.

### `resource list`

List all currently held resource locks with resource ID, mode, holder, and TTL.

*No flags.*

**Example:**

```bash
agentos resource list
```

### `resource release`

Forcibly release a specific resource lock.

| Flag | Type | Description |
|------|------|-------------|
| `--resource` | `String` | Resource ID to release |
| `--agent` | `String` | Agent name that holds the lock |

**Example:**

```bash
agentos resource release --resource "/data/reports.csv" --agent analyst-1
```

### `resource contention`

Show resource contention statistics: which resources have waiters and blocked agents.

*No flags.*

**Example:**

```bash
agentos resource contention
```

### `resource release-all`

Release all resource locks held by an agent.

| Flag | Type | Description |
|------|------|-------------|
| `--agent` | `String` | Agent name whose locks should be released |

**Example:**

```bash
agentos resource release-all --agent worker
```

---

## `escalation` — View and resolve human approval requests

Agents can escalate decisions to a human operator when they need approval. Escalations auto-expire after 5 minutes if unresolved.

### `escalation list`

List escalations. By default, shows only pending (unresolved) escalations.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--all` | `bool` | `false` | Show all escalations including resolved ones |

**Example:**

```bash
agentos escalation list
agentos escalation list --all
```

### `escalation get`

Show full details of a specific escalation including reason, urgency, blocking status, options, and resolution.

| Argument | Type | Description |
|----------|------|-------------|
| `id` | `u64` | Escalation ID |

**Example:**

```bash
agentos escalation get 42
```

### `escalation resolve`

Resolve an escalation with a decision. If the escalation was blocking a task, the task resumes.

| Flag / Argument | Type | Description |
|-----------------|------|-------------|
| `id` | `u64` | Escalation ID (positional) |
| `--decision` / `-d` | `String` | Decision string (e.g. `"Approved"`, `"Denied"`, `"Acknowledged"`) |

**Example:**

```bash
agentos escalation resolve 42 --decision "Approved"
agentos escalation resolve 42 -d "Denied"
```

---

## `snapshot` — Manage task snapshots and rollback

Snapshots capture a task's context state at a point in time for later rollback. Snapshots older than 72 hours are automatically expired.

### `snapshot list`

List all snapshots for a task.

| Flag | Type | Description |
|------|------|-------------|
| `--task` | `String` | Task UUID |

**Example:**

```bash
agentos snapshot list --task a3b2c1d0-1234-5678-9abc-def012345678
```

### `snapshot rollback`

Roll back a task to a specific snapshot or the latest one.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--task` | `String` | *required* | Task UUID |
| `--snapshot` | `Option<String>` | — | Snapshot reference (e.g. `snap_0001`). Defaults to latest |

**Example:**

```bash
agentos snapshot rollback --task a3b2c1d0-... --snapshot snap_0002
agentos snapshot rollback --task a3b2c1d0-...
```

---

## `scratchpad` — Manage agent scratchpad pages

The scratchpad provides Obsidian-inspired markdown pages with wikilink support for agent working memory. Each agent has its own scratchpad namespace.

### `scratchpad list`

List all scratchpad pages for an agent.

| Flag | Type | Description |
|------|------|-------------|
| `--agent` | `String` | Agent name |

**Example:**

```bash
agentos scratchpad list --agent analyst-1
```

### `scratchpad read`

Read a scratchpad page by title.

| Flag | Type | Description |
|------|------|-------------|
| `--title` | `String` | Page title |
| `--agent` | `String` | Agent name |

**Example:**

```bash
agentos scratchpad read --title "Research Notes" --agent analyst-1
```

### `scratchpad delete`

Delete a scratchpad page.

| Flag | Type | Description |
|------|------|-------------|
| `--title` | `String` | Page title |
| `--agent` | `String` | Agent name |

**Example:**

```bash
agentos scratchpad delete --title "Scratch" --agent analyst-1
```

### `scratchpad graph`

Show the wikilink graph for a scratchpad page, including backlinks and forward links.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--title` | `String` | *required* | Page title |
| `--agent` | `String` | *required* | Agent name |
| `--depth` | `u32` | `1` | Graph traversal depth |

**Example:**

```bash
agentos scratchpad graph --title "Research Notes" --agent analyst-1
agentos scratchpad graph --title "Research Notes" --agent analyst-1 --depth 3
```

---

## `event` — Manage event subscriptions and view event history

The event system supports subscription-based reactive messaging with filters, throttling, and priority levels.

### `event subscribe`

Subscribe an agent to an event type with optional filtering and throttling.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--agent` | `String` | *required* | Agent name to subscribe |
| `--event` | `String` | *required* | Event filter: `"all"`, `"category:<name>"`, or exact event type like `"AgentAdded"` |
| `--filter` | `Option<String>` | — | Payload filter expression (e.g. `"cpu_percent > 85 AND severity == Critical"`) |
| `--throttle` | `Option<String>` | — | Throttle policy: `"none"`, `"once_per:<dur>"`, `"max:<count>/<dur>"` (e.g. `"once_per:30s"`) |
| `--priority` | `String` | `"normal"` | Subscription priority: `critical`, `high`, `normal`, `low` |

**Example:**

```bash
agentos event subscribe --agent analyst --event AgentAdded

agentos event subscribe --agent monitor --event CPUSpikeDetected \
  --filter "cpu_percent > 90 AND severity == Critical" \
  --throttle "once_per:30s" \
  --priority high
```

### `event unsubscribe`

Remove an event subscription.

| Argument | Type | Description |
|----------|------|-------------|
| `id` | `String` | Subscription UUID |

**Example:**

```bash
agentos event unsubscribe a3b2c1d0-1234-5678-9abc-def012345678
```

### `event subscriptions list`

List all subscriptions, optionally filtered by agent.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--agent` | `Option<String>` | — | Filter by agent name |

**Example:**

```bash
agentos event subscriptions list
agentos event subscriptions list --agent analyst
```

### `event subscriptions show`

Show full details of a subscription.

| Flag | Type | Description |
|------|------|-------------|
| `--id` | `String` | Subscription UUID |

**Example:**

```bash
agentos event subscriptions show --id a3b2c1d0-...
```

### `event subscriptions enable`

Re-enable a disabled subscription.

| Flag | Type | Description |
|------|------|-------------|
| `--id` | `String` | Subscription UUID |

**Example:**

```bash
agentos event subscriptions enable --id a3b2c1d0-...
```

### `event subscriptions disable`

Disable a subscription without removing it.

| Flag | Type | Description |
|------|------|-------------|
| `--id` | `String` | Subscription UUID |

**Example:**

```bash
agentos event subscriptions disable --id a3b2c1d0-...
```

### `event history`

View recent event history.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--last` | `u32` | `20` | Number of recent events to show |

**Example:**

```bash
agentos event history
agentos event history --last 100
```

---

## `identity` — Manage agent cryptographic identities

Each agent has an Ed25519 keypair generated at connection time for signing and authentication.

### `identity show`

Show an agent's cryptographic identity: public key and signing key status.

| Flag | Type | Description |
|------|------|-------------|
| `--agent` | `String` | Agent name |

**Example:**

```bash
agentos identity show --agent analyst-1
```

### `identity revoke`

Revoke an agent's cryptographic identity and all associated permissions. This is a destructive operation.

| Flag | Type | Description |
|------|------|-------------|
| `--agent` | `String` | Agent name |

**Example:**

```bash
agentos identity revoke --agent compromised-agent
```

---

## `hal` — Manage hardware device access (HAL)

The Hardware Abstraction Layer controls which agents can access which physical devices. New devices start in quarantine until explicitly approved.

### `hal list`

List all registered hardware devices with their ID, type, status, and number of agents granted access.

*No flags.*

**Example:**

```bash
agentos hal list
```

### `hal register`

Register a new hardware device. The device enters quarantine pending approval.

| Flag | Type | Description |
|------|------|-------------|
| `--id` | `String` | Device ID (e.g. `gpu:0`, `usb:1`, `cam:0`) |
| `--type` | `String` | Human-readable device type (e.g. `"nvidia-rtx-4090"`, `"webcam"`) |

**Example:**

```bash
agentos hal register --id gpu:0 --type nvidia-rtx-4090
```

### `hal approve`

Approve a quarantined device for a specific agent.

| Flag / Argument | Type | Description |
|-----------------|------|-------------|
| `device` | `String` | Device ID (positional) |
| `--agent` | `String` | Agent name to grant access to |

**Example:**

```bash
agentos hal approve gpu:0 --agent worker
```

### `hal deny`

Permanently deny a device for all agents. The device cannot be approved until re-registered.

| Argument | Type | Description |
|----------|------|-------------|
| `device` | `String` | Device ID |

**Example:**

```bash
agentos hal deny usb:1
```

### `hal revoke`

Revoke a specific agent's access to a device.

| Flag / Argument | Type | Description |
|-----------------|------|-------------|
| `device` | `String` | Device ID (positional) |
| `--agent` | `String` | Agent name to revoke access from |

**Example:**

```bash
agentos hal revoke gpu:0 --agent worker
```

### `hal query`

Query a HAL driver directly. The driver dispatches the request based on the JSON parameters. Currently available drivers: `usb-storage` (requires the `usb-storage` feature flag at build time).

| Argument | Type | Description |
|----------|------|-------------|
| `driver` | `String` | Driver name (positional, e.g. `usb-storage`) |
| `params` | `String` | JSON object with driver-specific parameters |

**Examples:**

```bash
# List USB filesystems
agentos hal query usb-storage '{"action": "list"}'

# Mount a USB partition
agentos hal query usb-storage '{"action": "mount", "device": "sdb1"}'

# Unmount
agentos hal query usb-storage '{"action": "unmount", "device": "sdb1"}'

# Eject (power off the drive)
agentos hal query usb-storage '{"action": "eject", "device": "sdb1"}'
```

> **Permission:** Requires `hardware.usb-storage:x` and the device `usb-storage:<device>` must be approved in the HAL device registry. See [[18-Advanced Operations#USB Storage Driver]] for full details.

---

## `healthz` — Kernel health check

Check if the kernel health endpoint is responding. Designed for use by Docker HEALTHCHECK, load balancers, and monitoring probes.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--port` | `u16` | `9091` | Port the health endpoint is listening on |

**Example:**

```bash
agentos healthz
agentos healthz --port 9091
```

---

## `log` — Control runtime logging

Controls the kernel's runtime logging configuration, including log level and format. Changes take effect immediately without a restart.

*Refer to the kernel's logging documentation for supported levels and formats.*

**Example:**

```bash
agentos log
```

---

## `notifications` — Manage the user notification inbox

Agents can send messages to the operator via `notify-user` (fire-and-forget) and `ask-user` (blocking question). This command group manages those messages. See [[21-User Notifications and Channels]] for full details.

### `notifications list`

List notifications from the inbox.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--unread` / `-u` | flag | `false` | Show only unread notifications |
| `--limit` / `-n` | `u32` | `50` | Maximum number of notifications to return |

**Example:**

```bash
agentos notifications list
agentos notifications list --unread --limit 20
```

### `notifications read`

Show the full body of a notification and mark it as read. For `Question` messages, also shows the question, options, and any existing response.

| Argument | Type | Description |
|----------|------|-------------|
| `id` | `String` | Notification UUID |

**Example:**

```bash
agentos notifications read a3b2c1d0-1234-5678-9abc-def012345678
```

### `notifications respond`

Submit a response to an interactive `Question` notification. If the question was blocking a task, the task resumes immediately.

| Flag / Argument | Type | Description |
|-----------------|------|-------------|
| `id` | `String` | Notification UUID (positional) |
| `--response` / `-r` | `String` | Your response text |

**Example:**

```bash
agentos notifications respond a3b2c1d0-... --response "Yes, proceed"
```

### `notifications watch`

Poll for new notifications every 5 seconds. Silently registers existing unread notifications on first poll to avoid flooding the terminal. Press Ctrl-C to stop.

*No flags.*

**Example:**

```bash
agentos notifications watch
```

---

## `channel` — Manage external delivery channels

Register external channels (Telegram, ntfy, email) so notifications are delivered beyond the CLI inbox. Credentials are stored in the vault; the channel record stores only non-sensitive routing metadata.

### `channel connect`

Register a new external delivery channel.

| Flag | Type | Required | Default | Description |
|------|------|----------|---------|-------------|
| `--kind` / `-k` | `String` | Yes | — | `telegram`, `ntfy`, `email`, or a custom string |
| `--external-id` / `-e` | `String` | Yes | — | Telegram: `chat_id`; ntfy: topic name; email: to-address |
| `--display-name` / `-d` | `String` | Yes | — | Human-readable label (e.g. `@johndoe`) |
| `--credential-key` / `-c` | `String` | No | `""` | Vault key holding the bot token or password |
| `--reply-topic` | `String` | No | — | ntfy only: topic to listen on for inbound replies |
| `--server-url` | `String` | No | — | ntfy only: server URL (default: `https://ntfy.sh`) |

**Example:**

```bash
# Telegram: first store token in vault, then connect
agentos secret set TELEGRAM_BOT_TOKEN
agentos channel connect --kind telegram --external-id "123456789" \
  --display-name "@myhandle" --credential-key TELEGRAM_BOT_TOKEN

# ntfy
agentos channel connect --kind ntfy --external-id "agentos-alerts" \
  --display-name "ntfy/agentos-alerts"
```

### `channel list`

List all registered channels with ID, kind, display name, external ID, and connection time.

*No flags.*

**Example:**

```bash
agentos channel list
```

### `channel test`

Send a test notification to a channel to verify delivery is working.

| Argument | Type | Description |
|----------|------|-------------|
| `id` | `String` | Channel UUID (from `channel list`) |

**Example:**

```bash
agentos channel test a3b2c1d0-1234-5678-9abc-def012345678
```

### `channel disconnect`

Remove a registered channel.

| Argument | Type | Description |
|----------|------|-------------|
| `id` | `String` | Channel UUID (from `channel list`) |

**Example:**

```bash
agentos channel disconnect a3b2c1d0-1234-5678-9abc-def012345678
```

---

## `mcp` — Model Context Protocol integration

Bidirectional MCP bridge. `serve` and `list` are **offline** commands. `status` requires a running kernel.

See [[22-MCP Integration]] for the full feature guide including configuration, security model, and Claude Desktop setup.

### `mcp serve` (offline)

Expose all registered AgentOS tools as an MCP server over stdin/stdout. Intended for use with Claude Desktop, Cursor, and any MCP-compatible client.

```bash
# Used by MCP clients automatically (stdio transport)
agentos mcp serve

# Test from the shell
echo '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | agentos mcp serve
```

No flags. The command reads tool manifests directly from disk — no running kernel required.

### `mcp list` (offline)

List all MCP servers configured in the current config file. Shows config values only — does not check live connection state.

```bash
agentos mcp list
agentos --config /etc/agentos/prod.toml mcp list
```

### `mcp status`

Query the running kernel for live health of all configured MCP server connections. Requires a running kernel.

```bash
agentos mcp status
```

**Output:**

```
NAME                 STATUS       TOOLS    LAST ERROR
----------------------------------------------------------------------
filesystem           connected    8        -
web-search           disconnected 0        MCP server 'web-search' reconnect failed: ...
```

| Column | Description |
|--------|-------------|
| `NAME` | Server name from `config.mcp.servers[*].name` |
| `STATUS` | `connected` or `disconnected` |
| `TOOLS` | Tool count registered from this server at boot |
| `LAST ERROR` | Last connection-level error, or `-` if none |

---

## `web` — Start the web UI server

### `web serve`

Start the AgentOS web UI. Boots the kernel internally and serves the dashboard at the given address. The vault passphrase is resolved from `AGENTOS_VAULT_PASSPHRASE` or via interactive prompt.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--port` | `u16` | `8080` | Port to bind on |
| `--host` | `String` | `"127.0.0.1"` | IP address to bind on |

**Example:**

```bash
agentos web serve
agentos web serve --port 3000 --host 0.0.0.0
```

The server prints `Web UI: http://<host>:<port>` on startup. Press Ctrl-C to shut down both the web server and the kernel gracefully. SIGTERM is also handled (for systemd).

> **Note:** `web serve` boots its own embedded kernel. Do not run both `agentos start` and `agentos web serve` simultaneously — they would conflict on the bus socket and vault.

---

## `team` — Run and manage agent teams

### `team run`

Run a coordinator + workers agent team. The coordinator orchestrates a set of worker agents and aggregates their results into a final response.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--coordinator` | `String` | *required* | Coordinator agent name |
| `--workers` | `Vec<String>` | *required* | Worker agent names (repeat flag) |
| `--task` | `String` | *required* | Task description |

### `team status`

Show the current status of a running team — coordinator state, per-worker progress, and partial results.

```bash
agentos team run --coordinator orchestrator \
  --workers researcher --workers coder --workers reviewer \
  --task "Build a CSV importer"

agentos team status <team-id>
```

---

## `skill` — Manage autonomous skill packages

A skill bundles a system prompt, a curated tool allowlist, optional event triggers, and a budget into a single installable unit. Core skills ship under `skills/core/`.

| Subcommand | Description |
|---|---|
| `skill list` | List installed skills |
| `skill install <path>` | Install a skill from a directory or archive |
| `skill remove <id>` | Remove an installed skill |
| `skill run <id> --input <prompt>` | Run a skill against an input prompt |
| `skill status <id>` | Inspect the status of a running skill |

```bash
agentos skill list
agentos skill run cost-optimizer --input "find expensive tasks today"
```

---

## `mcp` (extended) — runtime attach, OAuth, status

Beyond the offline `serve`/`list` subcommands, the `mcp` group has runtime attach/detach commands that connect to external MCP servers without restarting the kernel. Attachments persist to SQLite and are restored on next boot.

| Subcommand | Description |
|---|---|
| `mcp status` | Show health, tool count, and last error per attached server |
| `mcp attach <name>` | Attach a new MCP server (`--transport stdio` or `--transport http`) |
| `mcp detach <name>` | Detach a previously attached MCP server |
| `mcp tools` | List MCP tools currently exposed by attached servers |
| `mcp call <name> <tool>` | Direct invocation against a specific MCP tool |

OAuth servers can be attached with `--oauth-connector <connector>`; tokens are stored in the encrypted vault and refreshed automatically. See [[22-MCP Integration]].

```bash
agentos mcp attach github --transport stdio --command "npx -y @modelcontextprotocol/server-github"
agentos mcp attach jira --transport http --url https://mcp.example.com --oauth-connector jira
agentos mcp status
```

---

## `a2a` — Agent-to-Agent protocol

The A2A protocol lets one AgentOS instance discover and delegate tasks to agents on another instance. Each delegated call goes through the same capability checks as a local tool call.

| Subcommand | Description |
|---|---|
| `a2a discover <url>` | Discover agents exposed by a remote AgentOS instance |
| `a2a delegate <agent-url> --task <prompt>` | Delegate a task to a remote agent |

---

## `provider` — LLM provider catalog

Lists every LLM provider known to the kernel. Built-in adapters (Ollama, OpenAI, Anthropic, Gemini) plus the OpenAI-compatible provider catalog loaded from `config/providers.toml` (Mistral, XAI, Cohere, Cerebras, Moonshot, NVIDIA, Hyperbolic, Azure, …).

| Subcommand | Description |
|---|---|
| `provider list` | List built-in adapters and catalog entries |
| `provider show <name>` | Show endpoint, default model, and headers for one provider |

---

## `plugin` — Plugin lifecycle

Plugins are manifest-described bundles that contribute tools, channel adapters, or skills. Discover at boot, signature-verify for non-Core tiers, and activate explicitly.

| Subcommand | Description |
|---|---|
| `plugin list` | List discovered plugins and their state (Discovered/Active/Disabled/Blocked) |
| `plugin enable <id>` | Activate a discovered plugin |
| `plugin disable <id>` | Disable an active plugin |
| `plugin info <id>` | Show manifest details for a plugin |

---

## `onboard` — Interactive setup wizard

Walks the operator through provider selection, API key resolution (stored as env-var references — never to disk), default agent creation, and data-directory setup. Refuses to overwrite a config file with parse errors.

```bash
agentos onboard
```

---

## `doctor` — Diagnose configuration issues

Runs six checks: config file existence, TOML validity, vault directory write probe, audit directory write probe, bus socket reachability, and tool registry sanity. Pass `--fix` to attempt safe auto-repair (creates missing directories, fixes permissions).

```bash
agentos doctor
agentos doctor --fix
```

---

## `config` — Read and write configuration values

Edits `config/default.toml` (or the `--config` override) without editing TOML by hand. Uses `toml_edit` so comments and formatting are preserved. Supports arbitrary depth dotted keys (e.g. `kernel.autonomous_mode.iter_cap`).

| Subcommand | Description |
|---|---|
| `config get <key>` | Read a value by dotted key |
| `config set <key> <value>` | Write a value by dotted key |
| `config list` | List all top-level config sections |

```bash
agentos config get llm.primary
agentos config set kernel.task_timeout_seconds 7200
agentos config list
```

---

## `init` — Scaffold a new AgentOS project

Creates a project directory with a working agent configuration, tool manifests, and an inline README explaining the security model.

| Template | Description |
|---|---|
| `hello-world` | Minimal agent that responds to a prompt |
| `secure-agent` | Agent with restricted CapabilityToken (recommended) |
| `mcp-server` | Agent exposed as an MCP server |
| `multi-agent` | Coordinator + 2 specialist agents |

```bash
agentos init my-project
agentos init my-project --template secure-agent
```

---

## Permission Reference Table

Permissions follow the format `<resource>:<flags>` where flags are `r` (read), `w` (write), and `x` (execute).

| Resource | `r` | `w` | `x` |
|----------|-----|-----|-----|
| `network.logs` | Read logs | — | — |
| `network.outbound` | — | — | Make HTTP calls |
| `process.list` | List processes | — | — |
| `process.kill` | — | — | Kill processes |
| `fs.app_logs` | Read app logs | — | — |
| `fs.system_logs` | Read system logs | — | — |
| `fs.user_data` | Read files | Write files | — |
| `hardware.sensors` | Read values | — | — |
| `hardware.gpu` | Query info | — | Use for compute |
| `cron.jobs` | View scheduled | Create new | Delete / run |
| `memory.semantic` | Read | Write | — |
| `memory.episodic` | Read | — | — |
| `memory.blocks` | Read blocks | Write blocks | — |
| `memory.procedural` | Read procedures | Write procedures | — |
| `agent.message` | Receive msgs | — | Send msgs |
| `agent.broadcast` | Receive | — | Broadcast |
| `agent.delegate` | — | — | Delegate subtasks |
| `agent.registry` | List agents | — | — |
| `task.query` | Query tasks | — | — |
| `user.notify` | — | Send notifications | — |
| `user.interact` | — | — | Ask blocking questions |

**Examples:**

```bash
# Read and write user data
agentos perm grant analyst fs.user_data:rw

# Execute outbound network calls
agentos perm grant worker network.outbound:x

# Read-only access to GPU info
agentos perm grant monitor hardware.gpu:r
```

---

## Quick Reference: All Command Groups

All **36** top-level command groups (an asterisk marks subcommands that work offline without a kernel connection):

| Group | Description | Subcommands |
|-------|-------------|-------------|
| `start` | Boot the kernel | — |
| `stop` | Shut down the kernel | — |
| `agent` | Manage LLM agents | `connect`, `list`, `disconnect`, `message`, `messages`, `group create`, `broadcast`, `memory show`, `memory history`, `memory rollback`, `memory clear`, `memory set` |
| `task` | Manage tasks | `run`, `list`, `logs`, `trace`, `traces`, `cancel`, `resume`, `checkpoints` |
| `tool` | Manage tools | `list`, `install`, `remove`, `keygen`*, `sign`*, `verify`* |
| `secret` | Manage encrypted vault | `set`, `list`, `revoke`, `rotate`, `lockdown` |
| `perm` | Manage permissions | `grant`, `revoke`, `show`, `profile create`, `profile delete`, `profile list`, `profile assign` |
| `role` | Manage OS roles | `create`, `delete`, `list`, `grant`, `revoke`, `assign`, `remove` |
| `schedule` | Manage cron jobs | `create`, `list`, `pause`, `resume`, `delete` |
| `bg` | Manage background tasks | `run`, `list`, `logs`, `kill` |
| `status` | Show system status | — |
| `audit` | View audit logs | `logs`, `verify`, `snapshots`, `export`, `rollback` |
| `pipeline` | Manage pipelines | `install`, `list`, `run`, `status`, `logs`, `remove` |
| `team` | Coordinator + worker teams | `run`, `status` |
| `cost` | View cost reports | `show`, `retrieval` |
| `resource` | Manage resource locks | `list`, `release`, `contention`, `release-all` |
| `escalation` | Human approval requests | `list`, `get`, `resolve` |
| `snapshot` | Task snapshots | `list`, `rollback` |
| `scratchpad` | Agent scratchpad pages | `list`, `read`, `delete`, `graph` |
| `event` | Event subscriptions | `subscribe`, `unsubscribe`, `subscriptions list`, `subscriptions show`, `subscriptions enable`, `subscriptions disable`, `history` |
| `identity` | Agent identities | `show`, `revoke` |
| `hal` | Hardware device access | `list`, `register`, `approve`, `deny`, `revoke`, `query` |
| `healthz` | Kernel health check | — |
| `log` | Control runtime logging | — |
| `notifications` | User notification inbox | `list`, `read`, `respond`, `watch` |
| `channel` | External delivery channels | `connect`, `list`, `test`, `disconnect` |
| `skill` | Autonomous skill packages | `list`, `install`, `remove`, `run`, `status` |
| `mcp` | MCP integration | `serve`*, `list`*, `status`, `attach`, `detach`, `tools`, `call` |
| `a2a` | Agent-to-Agent protocol | `discover`, `delegate` |
| `provider` | LLM provider catalog | `list`, `show` |
| `plugin` | Plugin lifecycle | `list`, `enable`, `disable`, `info` |
| `onboard` | Interactive setup wizard | — |
| `doctor` | Diagnose / auto-repair | — (`--fix` flag) |
| `config` | Read/write configuration | `get`, `set`, `list` |
| `init` | Scaffold new project | — (`--template`) |
| `web` | Web UI server | `serve` |

*\* Offline commands — do not require a running kernel.*
