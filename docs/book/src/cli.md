# CLI Reference

`agentos` is the command-line interface for managing AgentOS. Most commands talk to a running
kernel over a Unix domain socket; a few (key generation, signing, `doctor`, `config`) run
offline.

> This page is maintained by hand for now. It will later be auto-generated from the clap
> command definitions so it can never drift from the binary.

## Global options

```
agentos [--config <path>] <command>
```

| Option | Default | Description |
|--------|---------|-------------|
| `--config` | `config/default.toml` | Path to the kernel configuration file (or set `AGENTOS_CONFIG`). |

## Lifecycle

| Command | Description |
|---------|-------------|
| `agentos start [--vault-passphrase <p>]` | Boot the kernel. Prompts for the vault passphrase if omitted. |
| `agentos gateway run` | Boot the kernel and connect every channel in `[gateway]` as a bot daemon. See [Gateway-first](./deploy/gateway.md). |
| `agentos stop` | Gracefully shut down the running kernel. |
| `agentos status` | System status: uptime, agents, tasks, tools, audit entries. |
| `agentos healthz [--port <n>]` | Probe the health endpoint (used by the Docker `HEALTHCHECK`). |

## Setup & diagnostics

| Command | Description |
|---------|-------------|
| `agentos onboard` | Interactive setup wizard. **API keys are never written to disk.** |
| `agentos doctor [--fix]` | Diagnose configuration issues; `--fix` auto-repairs. |
| `agentos config get/set/list` | Read/write config values without editing TOML. |
| `agentos init <name> [--template <t>]` | Scaffold a project (`hello-world`, `secure-agent`, `mcp-server`, `multi-agent`). |
| `agentos log set-level <level>` | Change the runtime log level without a restart. |

## Agents & tasks

```bash
agentos agent connect --provider <p> --model <m> --name <name> [--base-url <url>]
agentos agent list
agentos agent disconnect <agent-id>

agentos task run [--agent <name>] [--thinking <level>] "<prompt>"
agentos task list
agentos task logs <task-id>
agentos task cancel <task-id>
agentos task resume <task-id>          # resume from a checkpoint
agentos task checkpoints               # list recoverable tasks
```

`--provider` is `ollama`, `openai`, `anthropic`, `gemini`, or `custom` (with `--base-url`).
For cloud providers you are prompted for an API key, which is encrypted into the vault.

## Tools, secrets, permissions

```bash
agentos tool list
agentos tool install <manifest-path>
agentos tool remove <tool-name>
agentos tool keygen / sign / verify      # offline Ed25519 manifest signing

agentos secret set <name> [--scope <scope>]   # global | agent:<name> | tool:<name>
agentos secret list / rotate <name> / revoke <name>

agentos perm grant <agent> <resource:ops> [--expires <dur>]   # ops: r, w, x
agentos perm revoke <agent> <resource:ops>
agentos perm show <agent>
agentos perm profile create/list/delete/assign
```

## RBAC & approvals

```bash
agentos role create/list/delete/assign/unassign
agentos approval ...        # set approval mode + learned "allow always" policy
agentos workspace ...       # grant/revoke/list filesystem workspace grants
agentos escalation ...      # view and resolve human approval requests
agentos prefs review/accept/reject/stats   # user-preference adaptation proposals
```

## Scheduling & background work

```bash
agentos schedule create --name <n> --cron "<expr>" --agent <a> --task "<prompt>" --permissions "<p1,p2>"
agentos schedule list/pause/resume/delete

agentos bg run --name <n> --agent <a> --task "<prompt>" [--detach]
agentos bg list/logs/kill
```

## Multi-agent & integrations

```bash
agentos pipeline ...     # multi-step workflow orchestration
agentos team ...         # coordinator + worker agent teams
agentos skill install/remove/list
agentos plugin list/enable/disable/info
agentos channel ...      # bidirectional notification channels
agentos mcp serve/tools/call/...   # MCP — see ./mcp.md
agentos a2a ...          # Agent-to-Agent protocol
agentos provider list    # built-in + catalog LLM providers
```

## Inspection & ops

```bash
agentos audit logs --last <count>
agentos cost ...         # cost and budget reports
agentos resource ...     # resource locks (arbitration)
agentos snapshot ...     # task snapshots and rollback
agentos scratchpad ...   # agent scratchpad notes
agentos event ...        # event subscriptions and history
agentos identity ...     # agent cryptographic identities
agentos hal ...          # hardware device access (HAL)
agentos notifications ...
agentos web serve [--host <h>] [--port <n>]
```

## Permission reference

Permissions use `<resource>:<ops>` where ops are `r` (read), `w` (write), `x` (execute).
Common resources: `fs.user_data`, `fs.app_logs`, `fs.system_logs`, `network.logs`,
`network.outbound`, `process.list`, `process.kill`, `hardware.sensors`, `hardware.gpu`,
`cron.jobs`, `memory.semantic`, `memory.episodic`, `agent.message`, `agent.broadcast`.
Agents start with **zero** permissions.
