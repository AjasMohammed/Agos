# CLI Reference

> **Generated** from `agentos --help` by `docs/book/gen-cli-ref.sh`.
> Do not edit by hand — re-run the script after CLI changes so this page
> can never drift from the binary. The version embeds via clap
> `#[command(version)]`.

Most commands talk to a running kernel over a Unix domain socket; a few
(key generation, signing, `doctor`, `config`) run offline.

## `agentos`

```text
AgentOS — Control CLI for the LLM-native operating system

Usage: agentos [OPTIONS] <COMMAND>

Commands:
  start            Boot the AgentOS kernel
  gateway          Run AgentOS as a long-lived messaging gateway ("run as a bot")
  stop             Gracefully shut down the running kernel
  agent            Manage LLM agents
  task             Manage tasks
  tool             Manage tools
  secret           Manage secrets
  perm             Manage agent permissions
  prefs            Review user preference adaptation proposals
  profile          Manage learned user-profile facts
  recommendations  View and respond to proactive recommendations (accept or dismiss)
  personalization  Manage personalization data — status, export, and right-to-forget
  role             Manage OS roles
  status           Show system status
  audit            View audit logs
  schedule         Manage scheduled background jobs
  bg               Manage background tasks
  pipeline         Manage multi-agent pipelines
  team             Run and manage agent teams (coordinator + workers)
  cost             View agent cost and budget reports
  resource         Manage resource locks (arbitration)
  escalation       View and resolve human approval requests from agents
  snapshot         Manage task snapshots and rollback
  scratchpad       Manage agent scratchpad notes
  event            Manage event subscriptions and view event history
  identity         Manage agent cryptographic identities
  hal              Manage hardware device access (HAL)
  web              Web UI server
  log              Control runtime logging (log level, format)
  healthz          Check if the kernel health endpoint is responding (used by Docker HEALTHCHECK)
  notifications    View and respond to agent notifications
  channel          Manage bidirectional notification channels (Telegram, ntfy, email)
  skill            Manage autonomous skill packages (system prompt + tools + triggers + budget)
  mcp              MCP (Model Context Protocol) adapter — import/export tools via the standard protocol
  a2a              A2A (Agent-to-Agent) protocol — discover and delegate to external agents
  provider         List and inspect available LLM providers (built-in + catalog)
  plugin           Manage plugins — list, enable, disable, and inspect plugin manifests
  workspace        Grant, revoke, or list user filesystem workspace grants
  approval         Manage tool-call approval mode and learned "allow always" policy
  onboard          Interactive setup wizard — configure providers, agents, and data paths
  doctor           Diagnose configuration issues and optionally auto-repair them
  config           Read or write configuration values without editing TOML manually
  init             Scaffold a new AgentOS project from a template
  help             Print this message or the help of the given subcommand(s)

Options:
      --config <CONFIG>  Path to kernel config file [env: AGENTOS_CONFIG=/home/ajas/.agentos/config.toml] [default: config/default.toml]
  -h, --help             Print help
  -V, --version          Print version
```

## `agentos start`

```text
Boot the AgentOS kernel

Usage: agentos start

Options:
  -h, --help  Print help
```

## `agentos gateway`

```text
Run AgentOS as a long-lived messaging gateway ("run as a bot")

Usage: agentos gateway <COMMAND>

Commands:
  run   Boot the kernel and connect all configured channels as a daemon
  help  Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos gateway run`

```text
Boot the kernel and connect all configured channels as a daemon

Usage: agentos gateway run

Options:
  -h, --help  Print help
```

## `agentos stop`

```text
Gracefully shut down the running kernel

Usage: agentos stop

Options:
  -h, --help  Print help
```

## `agentos agent`

```text
Manage LLM agents

Usage: agentos agent <COMMAND>

Commands:
  connect     Connect a new LLM agent
  ping        Probe an LLM backend's reachability without registering an agent
  list        List connected agents
  disconnect  Disconnect an agent
  remove      Permanently remove an agent: deletes the profile, memory, scratchpad, inboxes, checkpoints, and any schedules it created. Vault secrets and the audit log are preserved. Re-adding the agent triggers a fresh onboarding task
  message     Send a message to an agent
  messages    List messages for an agent
  set-url     Change the LLM endpoint URL for a connected agent (takes effect immediately)
  group       Manage agent groups
  memory      Manage agent context memory
  broadcast   Broadcast a message to a group
  help        Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos agent connect`

```text
Connect a new LLM agent

Usage: agentos agent connect [OPTIONS] --provider <PROVIDER> --model <MODEL> --name <NAME>

Options:
      --provider <PROVIDER>  LLM provider
      --model <MODEL>        Model name
      --name <NAME>          Agent display name
      --base-url <BASE_URL>  Base URL for the LLM endpoint. Defaults to AGENTOS_LLM_URL env var if set, otherwise falls back to the provider catalog URL or llm.custom_base_url in config. Example: http://localhost:1234/v1 [env: AGENTOS_LLM_URL=]
      --role <ROLES>         Role(s) for the agent — may be repeated (e.g. --role orchestrator). Supported: orchestrator, security-monitor, sysops, memory-manager, tool-manager, general. Defaults to "general" if omitted
      --test                 Connect in test mode: the agent receives an ecosystem-evaluation prompt instead of starting idle, and is asked to provide usability feedback
      --grant <GRANTS>       Extra permissions to grant on connect (format: resource:flags, e.g. process.exec:x). May be repeated: --grant process.exec:x --grant fs.data:rw
      --root                 Grant full root access to the ecosystem (all permissions)
      --no-health-check      Skip the pre-flight LLM health check. By default the kernel probes the provider's backend before registering the agent and refuses to register if the backend is unreachable. Use this flag to register anyway (e.g. if the backend is slow to start)
  -h, --help                 Print help
```

### `agentos agent ping`

```text
Probe an LLM backend's reachability without registering an agent.

Builds the same adapter `agent connect` would, runs `health_check()`, and prints the status. Useful for validating `--base-url`, API key setup, and model availability before committing to a connect.

Usage: agentos agent ping [OPTIONS] --provider <PROVIDER> --model <MODEL>

Options:
      --provider <PROVIDER>
          LLM provider (ollama, openai, anthropic, gemini, or a catalog name)

      --model <MODEL>
          Model name

      --base-url <BASE_URL>
          Optional base URL override (defaults to provider catalog / env / config)
          
          [env: AGENTOS_LLM_URL=]

      --name <NAME>
          Optional agent name used to look up per-agent vault keys (e.g. `<name>_openai_api_key`). Falls back to the global key if omitted

  -h, --help
          Print help (see a summary with '-h')
```

### `agentos agent list`

```text
List connected agents

Usage: agentos agent list

Options:
  -h, --help  Print help
```

### `agentos agent disconnect`

```text
Disconnect an agent

Usage: agentos agent disconnect <NAME>

Arguments:
  <NAME>  Agent name to disconnect

Options:
  -h, --help  Print help
```

### `agentos agent remove`

```text
Permanently remove an agent: deletes the profile, memory, scratchpad, inboxes, checkpoints, and any schedules it created. Vault secrets and the audit log are preserved. Re-adding the agent triggers a fresh onboarding task

Usage: agentos agent remove [OPTIONS] <NAME>

Arguments:
  <NAME>  Agent name to remove

Options:
      --yes   Skip the confirmation prompt
  -h, --help  Print help
```

### `agentos agent message`

```text
Send a message to an agent

Usage: agentos agent message --from <FROM> <TO> <CONTENT>

Arguments:
  <TO>       Target agent name
  <CONTENT>  Message content

Options:
      --from <FROM>  Sender agent name
  -h, --help         Print help
```

### `agentos agent messages`

```text
List messages for an agent

Usage: agentos agent messages [OPTIONS] <AGENT>

Arguments:
  <AGENT>  Agent name

Options:
      --last <LAST>  Number of recent messages to show [default: 10]
  -h, --help         Print help
```

### `agentos agent set-url`

```text
Change the LLM endpoint URL for a connected agent (takes effect immediately)

Usage: agentos agent set-url <NAME> <URL>

Arguments:
  <NAME>  Agent name
  <URL>   New base URL (e.g. http://localhost:5678/v1)

Options:
  -h, --help  Print help
```

### `agentos agent group`

```text
Manage agent groups

Usage: agentos agent group <COMMAND>

Commands:
  create  Create a new agent group
  help    Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos agent memory`

```text
Manage agent context memory

Usage: agentos agent memory <COMMAND>

Commands:
  show      Show the current context memory for an agent
  history   Show context memory version history
  rollback  Rollback to a specific version
  clear     Clear the agent's context memory
  set       Set context memory from a file
  help      Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos agent broadcast`

```text
Broadcast a message to a group

Usage: agentos agent broadcast --from <FROM> <GROUP> <CONTENT>

Arguments:
  <GROUP>    Target group name
  <CONTENT>  Message content

Options:
      --from <FROM>  Sender agent name
  -h, --help         Print help
```

## `agentos task`

```text
Manage tasks

Usage: agentos task <COMMAND>

Commands:
  run          Run a task
  list         List all tasks
  logs         View task logs
  cancel       Cancel a running task
  resume       Resume a task from its latest checkpoint
  checkpoints  List all tasks that have checkpoints available for resume
  trace        Show execution trace for a completed task
  traces       List recent task execution traces
  help         Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos task run`

```text
Run a task

Usage: agentos task run [OPTIONS] <PROMPT>

Arguments:
  <PROMPT>  The task prompt

Options:
      --agent <AGENT>        Agent to assign the task to (if left empty, auto-routing is used)
      --autonomous           Run without iteration or timeout limits. Use for long-running autonomous workflows that must run to natural completion
      --no-checkpoint        Skip checkpointing for this task (ephemeral execution)
      --thinking <THINKING>  Extended thinking level: off, low, medium, high, max (Anthropic models only). Higher levels give better reasoning at increased token cost and latency [default: off]
  -i, --interactive          If the kernel pauses for human approval, prompt inline at the terminal instead of returning to the shell. Requires a TTY on stdin; non-interactive runs fall back to the existing behaviour
  -h, --help                 Print help
```

### `agentos task list`

```text
List all tasks

Usage: agentos task list

Options:
  -h, --help  Print help
```

### `agentos task logs`

```text
View task logs

Usage: agentos task logs <TASK_ID>

Arguments:
  <TASK_ID>  Task ID

Options:
  -h, --help  Print help
```

### `agentos task cancel`

```text
Cancel a running task

Usage: agentos task cancel <TASK_ID>

Arguments:
  <TASK_ID>  Task ID

Options:
  -h, --help  Print help
```

### `agentos task resume`

```text
Resume a task from its latest checkpoint

Usage: agentos task resume <TASK_ID>

Arguments:
  <TASK_ID>  Task ID

Options:
  -h, --help  Print help
```

### `agentos task checkpoints`

```text
List all tasks that have checkpoints available for resume

Usage: agentos task checkpoints

Options:
  -h, --help  Print help
```

### `agentos task trace`

```text
Show execution trace for a completed task

Usage: agentos task trace [OPTIONS] <TASK_ID>

Arguments:
  <TASK_ID>  Task ID

Options:
      --json         Output as raw JSON instead of formatted text
      --iter <ITER>  Show only a specific iteration number (1-based)
  -h, --help         Print help
```

### `agentos task traces`

```text
List recent task execution traces

Usage: agentos task traces [OPTIONS]

Options:
      --limit <LIMIT>  Maximum number of traces to show [default: 20]
      --agent <AGENT>  Filter traces by agent ID
  -h, --help           Print help
```

## `agentos tool`

```text
Manage tools

Usage: agentos tool <COMMAND>

Commands:
  list     List installed tools
  install  Install a tool from a local manifest file (verifies trust-tier signature)
  remove   Remove an installed tool
  search   Search the tool registry for available tools
  add      Install a tool from the registry by name (downloads, verifies, hot-loads)
  publish  Publish a signed tool manifest to the registry
  keygen   Generate a new Ed25519 keypair for tool signing
  sign     Sign a tool manifest with an Ed25519 private key
  verify   Verify the Ed25519 signature on a tool manifest
  help     Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos tool list`

```text
List installed tools

Usage: agentos tool list

Options:
  -h, --help  Print help
```

### `agentos tool install`

```text
Install a tool from a local manifest file (verifies trust-tier signature)

Usage: agentos tool install <PATH>

Arguments:
  <PATH>  Path to the tool manifest (.toml)

Options:
  -h, --help  Print help
```

### `agentos tool remove`

```text
Remove an installed tool

Usage: agentos tool remove <NAME>

Arguments:
  <NAME>  Tool name to remove

Options:
  -h, --help  Print help
```

### `agentos tool search`

```text
Search the tool registry for available tools

Usage: agentos tool search [OPTIONS] <QUERY>

Arguments:
  <QUERY>  Search query (matches name, description, tags, author)

Options:
      --limit <LIMIT>        Maximum results to return [default: 20]
      --registry <REGISTRY>  Registry URL override (default: from config or AGENTOS_REGISTRY env)
  -h, --help                 Print help
```

### `agentos tool add`

```text
Install a tool from the registry by name (downloads, verifies, hot-loads)

Usage: agentos tool add [OPTIONS] <NAME>

Arguments:
  <NAME>  Tool name in the registry

Options:
      --version <VERSION>    Specific version to install (default: latest)
      --yes                  Skip confirmation prompt
      --registry <REGISTRY>  Registry URL override
  -h, --help                 Print help
```

### `agentos tool publish`

```text
Publish a signed tool manifest to the registry

Usage: agentos tool publish [OPTIONS] <MANIFEST>

Arguments:
  <MANIFEST>  Path to the tool manifest (.toml)

Options:
      --key <KEY>            Path to keypair JSON file (default: sign with existing signature in manifest)
      --registry <REGISTRY>  Registry URL override
  -h, --help                 Print help
```

### `agentos tool keygen`

```text
Generate a new Ed25519 keypair for tool signing

Usage: agentos tool keygen [OPTIONS]

Options:
      --output <OUTPUT>  Write keypair JSON to this file [default: tool-keypair.json]
  -h, --help             Print help
```

### `agentos tool sign`

```text
Sign a tool manifest with an Ed25519 private key

Usage: agentos tool sign [OPTIONS] --manifest <MANIFEST> --key <KEY>

Options:
      --manifest <MANIFEST>  Path to the tool manifest (.toml) to sign
      --key <KEY>            Path to keypair JSON file produced by `tool keygen`
      --output <OUTPUT>      Write the signed manifest to this path (defaults to overwriting the source)
  -h, --help                 Print help
```

### `agentos tool verify`

```text
Verify the Ed25519 signature on a tool manifest

Usage: agentos tool verify <MANIFEST>

Arguments:
  <MANIFEST>  Path to the tool manifest (.toml) to verify

Options:
  -h, --help  Print help
```

## `agentos secret`

```text
Manage secrets

Usage: agentos secret <COMMAND>

Commands:
  set       Set a secret (value entered interactively — never in shell args)
  list      List all secrets (metadata only — values never shown)
  revoke    Revoke (delete) a secret
  rotate    Rotate a secret (new value entered interactively)
  lockdown  Emergency vault lockdown: revoke all proxy tokens and block new issuance
  help      Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos secret set`

```text
Set a secret (value entered interactively — never in shell args)

Usage: agentos secret set [OPTIONS] <NAME>

Arguments:
  <NAME>  Secret name (e.g. OPENAI_API_KEY)

Options:
      --scope <SCOPE>  Scope: "global", "agent:<name>", or "tool:<name>" [default: global]
  -h, --help           Print help
```

### `agentos secret list`

```text
List all secrets (metadata only — values never shown)

Usage: agentos secret list

Options:
  -h, --help  Print help
```

### `agentos secret revoke`

```text
Revoke (delete) a secret

Usage: agentos secret revoke <NAME>

Arguments:
  <NAME>  Secret name

Options:
  -h, --help  Print help
```

### `agentos secret rotate`

```text
Rotate a secret (new value entered interactively)

Usage: agentos secret rotate <NAME>

Arguments:
  <NAME>  Secret name

Options:
  -h, --help  Print help
```

### `agentos secret lockdown`

```text
Emergency vault lockdown: revoke all proxy tokens and block new issuance

Usage: agentos secret lockdown

Options:
  -h, --help  Print help
```

## `agentos perm`

```text
Manage agent permissions

Usage: agentos perm <COMMAND>

Commands:
  grant    Grant a permission to an agent
  revoke   Revoke a permission from an agent
  show     Show all permissions for an agent
  profile  Manage permission profiles
  help     Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos perm grant`

```text
Grant a permission to an agent

Usage: agentos perm grant [OPTIONS] <AGENT> <PERMISSION>

Arguments:
  <AGENT>       Agent name
  <PERMISSION>  Permission string (e.g. "fs.user_data:rw")

Options:
      --expires <EXPIRES>  Expiration time in seconds
  -h, --help               Print help
```

### `agentos perm revoke`

```text
Revoke a permission from an agent

Usage: agentos perm revoke <AGENT> <PERMISSION>

Arguments:
  <AGENT>       Agent name
  <PERMISSION>  Permission string

Options:
  -h, --help  Print help
```

### `agentos perm show`

```text
Show all permissions for an agent

Usage: agentos perm show <AGENT>

Arguments:
  <AGENT>  Agent name

Options:
  -h, --help  Print help
```

### `agentos perm profile`

```text
Manage permission profiles

Usage: agentos perm profile <COMMAND>

Commands:
  create  Create a new permission profile
  delete  Delete a permission profile
  list    List all permission profiles
  assign  Assign a permission profile to an agent
  help    Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

## `agentos prefs`

```text
Review user preference adaptation proposals

Usage: agentos prefs <COMMAND>

Commands:
  review  List pending user preference proposals
  accept  Accept a proposal and write it to context memory
  reject  Reject a proposal
  stats   Show queue stats
  help    Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos prefs review`

```text
List pending user preference proposals

Usage: agentos prefs review [OPTIONS]

Options:
      --limit <LIMIT>  [default: 50]
  -h, --help           Print help
```

### `agentos prefs accept`

```text
Accept a proposal and write it to context memory

Usage: agentos prefs accept <PROPOSAL_ID>

Arguments:
  <PROPOSAL_ID>  

Options:
  -h, --help  Print help
```

### `agentos prefs reject`

```text
Reject a proposal

Usage: agentos prefs reject <PROPOSAL_ID>

Arguments:
  <PROPOSAL_ID>  

Options:
  -h, --help  Print help
```

### `agentos prefs stats`

```text
Show queue stats

Usage: agentos prefs stats

Options:
  -h, --help  Print help
```

## `agentos profile`

```text
Manage learned user-profile facts

Usage: agentos profile <COMMAND>

Commands:
  list    List learned user-profile facts
  show    Show a single user-profile fact
  edit    Edit a learned user-profile fact
  forget  Forget a learned user-profile fact
  help    Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos profile list`

```text
List learned user-profile facts

Usage: agentos profile list [OPTIONS]

Options:
      --limit <LIMIT>  [default: 50]
  -h, --help           Print help
```

### `agentos profile show`

```text
Show a single user-profile fact

Usage: agentos profile show <ID>

Arguments:
  <ID>  

Options:
  -h, --help  Print help
```

### `agentos profile edit`

```text
Edit a learned user-profile fact

Usage: agentos profile edit [OPTIONS] <ID>

Arguments:
  <ID>  

Options:
      --value <VALUE>            
      --confidence <CONFIDENCE>  
      --category <CATEGORY>      
  -h, --help                     Print help
```

### `agentos profile forget`

```text
Forget a learned user-profile fact

Usage: agentos profile forget <ID>

Arguments:
  <ID>  

Options:
  -h, --help  Print help
```

## `agentos recommendations`

```text
View and respond to proactive recommendations (accept or dismiss)

Usage: agentos recommendations <COMMAND>

Commands:
  list     List recent proactive recommendations
  accept   Accept a recommendation and boost the originating interest
  dismiss  Dismiss a recommendation and lower the originating interest weight
  help     Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos recommendations list`

```text
List recent proactive recommendations

Usage: agentos recommendations list [OPTIONS]

Options:
      --limit <LIMIT>  [default: 20]
  -h, --help           Print help
```

### `agentos recommendations accept`

```text
Accept a recommendation and boost the originating interest

Usage: agentos recommendations accept <ID>

Arguments:
  <ID>  

Options:
  -h, --help  Print help
```

### `agentos recommendations dismiss`

```text
Dismiss a recommendation and lower the originating interest weight

Usage: agentos recommendations dismiss <ID>

Arguments:
  <ID>  

Options:
  -h, --help  Print help
```

## `agentos personalization`

```text
Manage personalization data — status, export, and right-to-forget

Usage: agentos personalization <COMMAND>

Commands:
  status  Show personalization subsystem status (enabled flags, row counts, retention windows)
  export  Export all personalization data (profile, interests, recommendations) as JSON
  forget  Permanently wipe all personalization data (right-to-forget)
  help    Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos personalization status`

```text
Show personalization subsystem status (enabled flags, row counts, retention windows)

Usage: agentos personalization status

Options:
  -h, --help  Print help
```

### `agentos personalization export`

```text
Export all personalization data (profile, interests, recommendations) as JSON.

Writes to stdout by default; use --out to save to a file.

Usage: agentos personalization export [OPTIONS]

Options:
  -o, --out <OUT>
          Optional output file path (default: stdout)

  -h, --help
          Print help (see a summary with '-h')
```

### `agentos personalization forget`

```text
Permanently wipe all personalization data (right-to-forget).

Clears the profile store, interests store, recommendations store, and accepted-preference context-memory entries. This operation is irreversible.

Usage: agentos personalization forget [OPTIONS]

Options:
      --yes
          Skip the confirmation prompt

  -h, --help
          Print help (see a summary with '-h')
```

## `agentos role`

```text
Manage OS roles

Usage: agentos role <COMMAND>

Commands:
  create  Create a new role
  delete  Delete a role
  list    List all roles
  grant   Grant a permission to a role
  revoke  Revoke a permission from a role
  assign  Assign a role to an agent
  remove  Remove a role from an agent
  help    Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos role create`

```text
Create a new role

Usage: agentos role create <NAME> [DESCRIPTION]

Arguments:
  <NAME>         Name of the role
  [DESCRIPTION]  Description of the role (optional positional)

Options:
  -h, --help  Print help
```

### `agentos role delete`

```text
Delete a role

Usage: agentos role delete <NAME>

Arguments:
  <NAME>  Name of the role to delete

Options:
  -h, --help  Print help
```

### `agentos role list`

```text
List all roles

Usage: agentos role list

Options:
  -h, --help  Print help
```

### `agentos role grant`

```text
Grant a permission to a role

Usage: agentos role grant <ROLE> <PERMISSION>

Arguments:
  <ROLE>        Role name
  <PERMISSION>  Permission string (e.g., fs.user_data:rw)

Options:
  -h, --help  Print help
```

### `agentos role revoke`

```text
Revoke a permission from a role

Usage: agentos role revoke <ROLE> <PERMISSION>

Arguments:
  <ROLE>        Role name
  <PERMISSION>  Permission string (e.g., fs.user_data:rw)

Options:
  -h, --help  Print help
```

### `agentos role assign`

```text
Assign a role to an agent

Usage: agentos role assign <AGENT> <ROLE>

Arguments:
  <AGENT>  Agent name
  <ROLE>   Role name

Options:
  -h, --help  Print help
```

### `agentos role remove`

```text
Remove a role from an agent

Usage: agentos role remove <AGENT> <ROLE>

Arguments:
  <AGENT>  Agent name
  <ROLE>   Role name

Options:
  -h, --help  Print help
```

## `agentos status`

```text
Show system status

Usage: agentos status

Options:
  -h, --help  Print help
```

## `agentos audit`

```text
View audit logs

Usage: agentos audit <COMMAND>

Commands:
  logs       View recent audit log entries
  verify     Verify the Merkle hash chain integrity
  snapshots  List context snapshots for a task
  export     Export the full audit chain as JSONL
  rollback   Roll back a task's context to a saved snapshot
  help       Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos audit logs`

```text
View recent audit log entries

Usage: agentos audit logs [OPTIONS]

Options:
      --last <LAST>  Number of recent entries to show [default: 50]
  -h, --help         Print help
```

### `agentos audit verify`

```text
Verify the Merkle hash chain integrity

Usage: agentos audit verify [OPTIONS]

Options:
      --from <FROM>  Start verification from this sequence number (default: beginning)
  -h, --help         Print help
```

### `agentos audit snapshots`

```text
List context snapshots for a task

Usage: agentos audit snapshots --task <TASK>

Options:
      --task <TASK>  Task ID to list snapshots for
  -h, --help         Print help
```

### `agentos audit export`

```text
Export the full audit chain as JSONL

Usage: agentos audit export [OPTIONS]

Options:
      --limit <LIMIT>    Maximum number of entries to export
      --output <OUTPUT>  Write to file instead of stdout
  -h, --help             Print help
```

### `agentos audit rollback`

```text
Roll back a task's context to a saved snapshot

Usage: agentos audit rollback [OPTIONS] --task <TASK>

Options:
      --task <TASK>          Task ID to roll back
      --snapshot <SNAPSHOT>  Snapshot reference (e.g. snap_0001). Defaults to most recent
  -h, --help                 Print help
```

## `agentos schedule`

```text
Manage scheduled background jobs

Usage: agentos schedule <COMMAND>

Commands:
  create  Create a recurring job
  list    List scheduled jobs
  pause   Pause a scheduled job
  resume  Resume a paused scheduled job
  delete  Delete a scheduled job
  help    Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos schedule create`

```text
Create a recurring job

Usage: agentos schedule create [OPTIONS] --name <NAME> --cron <CRON> --agent <AGENT> --task <TASK>

Options:
      --name <NAME>                Name of the schedule
      --cron <CRON>                Cron expression (5-field: 'min hr dom mon dow', or 6-field with seconds: 'sec min hr dom mon dow')
      --agent <AGENT>              Name of the agent to run the task
      --task <TASK>                Prompt/task description
      --permissions <PERMISSIONS>  Permissions required for the task (comma-separated, e.g., 'fs.user_data:rw') [default: ""]
  -h, --help                       Print help
```

### `agentos schedule list`

```text
List scheduled jobs

Usage: agentos schedule list

Options:
  -h, --help  Print help
```

### `agentos schedule pause`

```text
Pause a scheduled job

Usage: agentos schedule pause <NAME>

Arguments:
  <NAME>  Name or ID (UUID) of the schedule

Options:
  -h, --help  Print help
```

### `agentos schedule resume`

```text
Resume a paused scheduled job

Usage: agentos schedule resume <NAME>

Arguments:
  <NAME>  Name or ID (UUID) of the schedule

Options:
  -h, --help  Print help
```

### `agentos schedule delete`

```text
Delete a scheduled job

Usage: agentos schedule delete <NAME>

Arguments:
  <NAME>  Name or ID (UUID) of the schedule

Options:
  -h, --help  Print help
```

## `agentos bg`

```text
Manage background tasks

Usage: agentos bg <COMMAND>

Commands:
  run   Run a one-shot background task (detached)
  list  List background tasks
  logs  Follow logs for a background task
  kill  Kill a running background task
  help  Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos bg run`

```text
Run a one-shot background task (detached)

Usage: agentos bg run [OPTIONS] --name <NAME> --agent <AGENT> --task <TASK>

Options:
      --name <NAME>    Name of the background task
      --agent <AGENT>  Name of the agent to run the task
      --task <TASK>    Prompt/task description
      --detach         Detach the task immediately
  -h, --help           Print help
```

### `agentos bg list`

```text
List background tasks

Usage: agentos bg list

Options:
  -h, --help  Print help
```

### `agentos bg logs`

```text
Follow logs for a background task

Usage: agentos bg logs [OPTIONS] <NAME>

Arguments:
  <NAME>  Name or task ID (UUID) of the background task

Options:
      --follow  Follow the logs continuously
  -h, --help    Print help
```

### `agentos bg kill`

```text
Kill a running background task

Usage: agentos bg kill <NAME>

Arguments:
  <NAME>  Name or task ID (UUID) of the background task

Options:
  -h, --help  Print help
```

## `agentos pipeline`

```text
Manage multi-agent pipelines

Usage: agentos pipeline <COMMAND>

Commands:
  install  Install a pipeline from a YAML file
  list     List installed pipelines
  run      Run a pipeline
  status   Get pipeline run status
  logs     View step-level logs for a pipeline run
  remove   Remove an installed pipeline
  help     Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos pipeline install`

```text
Install a pipeline from a YAML file

Usage: agentos pipeline install <PATH>

Arguments:
  <PATH>  Path to the pipeline YAML file

Options:
  -h, --help  Print help
```

### `agentos pipeline list`

```text
List installed pipelines

Usage: agentos pipeline list

Options:
  -h, --help  Print help
```

### `agentos pipeline run`

```text
Run a pipeline

Usage: agentos pipeline run [OPTIONS] --input <INPUT> <NAME>

Arguments:
  <NAME>  Pipeline name

Options:
      --input <INPUT>  Input string for the pipeline
      --detach         Run in background (detached)
      --agent <AGENT>  Agent whose permissions govern pipeline execution
  -h, --help           Print help
```

### `agentos pipeline status`

```text
Get pipeline run status

Usage: agentos pipeline status --run-id <RUN_ID> <NAME>

Arguments:
  <NAME>  Pipeline name

Options:
      --run-id <RUN_ID>  Run ID
  -h, --help             Print help
```

### `agentos pipeline logs`

```text
View step-level logs for a pipeline run

Usage: agentos pipeline logs --run-id <RUN_ID> --step <STEP> <NAME>

Arguments:
  <NAME>  Pipeline name

Options:
      --run-id <RUN_ID>  Run ID
      --step <STEP>      Step ID to view logs for
  -h, --help             Print help
```

### `agentos pipeline remove`

```text
Remove an installed pipeline

Usage: agentos pipeline remove <NAME>

Arguments:
  <NAME>  Pipeline name

Options:
  -h, --help  Print help
```

## `agentos team`

```text
Run and manage agent teams (coordinator + workers)

Usage: agentos team <COMMAND>

Commands:
  run   Run a team defined in a TOML config file against its declared goal
  list  List active team runs (by coordinator task)
  help  Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos team run`

```text
Run a team defined in a TOML config file against its declared goal

Usage: agentos team run --config <CONFIG>

Options:
  -c, --config <CONFIG>  Path to the team TOML config file
  -h, --help             Print help
```

### `agentos team list`

```text
List active team runs (by coordinator task)

Usage: agentos team list

Options:
  -h, --help  Print help
```

## `agentos cost`

```text
View agent cost and budget reports

Usage: agentos cost <COMMAND>

Commands:
  show       Show cost report for all agents or a specific agent
  retrieval  Show retrieval refresh/reuse efficiency metrics
  help       Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos cost show`

```text
Show cost report for all agents or a specific agent

Usage: agentos cost show [OPTIONS]

Options:
      --agent <AGENT>  Agent name (omit for all agents)
  -h, --help           Print help
```

### `agentos cost retrieval`

```text
Show retrieval refresh/reuse efficiency metrics

Usage: agentos cost retrieval

Options:
  -h, --help  Print help
```

## `agentos resource`

```text
Manage resource locks (arbitration)

Usage: agentos resource <COMMAND>

Commands:
  list         List all currently held resource locks
  release      Forcibly release a specific resource lock held by an agent
  contention   Show resource contention statistics (waiters, blocked agents)
  release-all  Release all resource locks held by an agent
  help         Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos resource list`

```text
List all currently held resource locks

Usage: agentos resource list

Options:
  -h, --help  Print help
```

### `agentos resource release`

```text
Forcibly release a specific resource lock held by an agent

Usage: agentos resource release --resource <RESOURCE> --agent <AGENT>

Options:
      --resource <RESOURCE>  Resource ID to release
      --agent <AGENT>        Agent name that holds the lock
  -h, --help                 Print help
```

### `agentos resource contention`

```text
Show resource contention statistics (waiters, blocked agents)

Usage: agentos resource contention

Options:
  -h, --help  Print help
```

### `agentos resource release-all`

```text
Release all resource locks held by an agent

Usage: agentos resource release-all --agent <AGENT>

Options:
      --agent <AGENT>  Agent name whose locks should be released
  -h, --help           Print help
```

## `agentos escalation`

```text
View and resolve human approval requests from agents

Usage: agentos escalation <COMMAND>

Commands:
  list     List pending escalations awaiting human review
  get      Show details of a specific escalation
  resolve  Resolve an escalation with a decision
  help     Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos escalation list`

```text
List pending escalations awaiting human review

Usage: agentos escalation list [OPTIONS]

Options:
      --all   Show all escalations including resolved ones
  -h, --help  Print help
```

### `agentos escalation get`

```text
Show details of a specific escalation

Usage: agentos escalation get <ID>

Arguments:
  <ID>  Escalation ID

Options:
  -h, --help  Print help
```

### `agentos escalation resolve`

```text
Resolve an escalation with a decision

Usage: agentos escalation resolve --decision <DECISION> <ID>

Arguments:
  <ID>  Escalation ID to resolve

Options:
  -d, --decision <DECISION>  Decision string (e.g. "Approved", "Denied", "Acknowledged")
  -h, --help                 Print help
```

## `agentos snapshot`

```text
Manage task snapshots and rollback

Usage: agentos snapshot <COMMAND>

Commands:
  list      List snapshots for a task
  rollback  Roll back a task to a specific snapshot (or the latest)
  help      Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos snapshot list`

```text
List snapshots for a task

Usage: agentos snapshot list --task <TASK>

Options:
      --task <TASK>  Task ID
  -h, --help         Print help
```

### `agentos snapshot rollback`

```text
Roll back a task to a specific snapshot (or the latest)

Usage: agentos snapshot rollback [OPTIONS] --task <TASK>

Options:
      --task <TASK>          Task ID
      --snapshot <SNAPSHOT>  Snapshot reference (e.g. snap_0001). Defaults to the latest
  -h, --help                 Print help
```

## `agentos scratchpad`

```text
Manage agent scratchpad notes

Usage: agentos scratchpad <COMMAND>

Commands:
  list    List all scratchpad pages for an agent
  read    Read a scratchpad page by title
  delete  Delete a scratchpad page
  graph   Show the wikilink graph for a page
  help    Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos scratchpad list`

```text
List all scratchpad pages for an agent

Usage: agentos scratchpad list --agent <AGENT>

Options:
      --agent <AGENT>  
  -h, --help           Print help
```

### `agentos scratchpad read`

```text
Read a scratchpad page by title

Usage: agentos scratchpad read --agent <AGENT> <TITLE>

Arguments:
  <TITLE>  

Options:
      --agent <AGENT>  
  -h, --help           Print help
```

### `agentos scratchpad delete`

```text
Delete a scratchpad page

Usage: agentos scratchpad delete --agent <AGENT> <TITLE>

Arguments:
  <TITLE>  

Options:
      --agent <AGENT>  
  -h, --help           Print help
```

### `agentos scratchpad graph`

```text
Show the wikilink graph for a page

Usage: agentos scratchpad graph [OPTIONS] --agent <AGENT> <TITLE>

Arguments:
  <TITLE>  

Options:
      --agent <AGENT>  
      --depth <DEPTH>  [default: 2]
  -h, --help           Print help
```

## `agentos event`

```text
Manage event subscriptions and view event history

Usage: agentos event <COMMAND>

Commands:
  subscribe      Subscribe an agent to an event type
  unsubscribe    Remove an event subscription
  subscriptions  Manage event subscriptions
  history        View recent event history
  help           Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos event subscribe`

```text
Subscribe an agent to an event type

Usage: agentos event subscribe [OPTIONS] --agent <AGENT> --event <EVENT>

Options:
      --agent <AGENT>        Name of the agent to subscribe
      --event <EVENT>        Event filter: "all", "category:<name>", or exact event type like "AgentAdded"
      --filter <FILTER>      Optional payload filter expression (e.g. "cpu_percent > 85 AND severity == Critical")
      --throttle <THROTTLE>  Throttle policy: "none", "once_per:<dur>", "max:<count>/<dur>" (e.g. "once_per:30s")
      --priority <PRIORITY>  Subscription priority: critical, high, normal, low [default: normal]
  -h, --help                 Print help
```

### `agentos event unsubscribe`

```text
Remove an event subscription

Usage: agentos event unsubscribe <ID>

Arguments:
  <ID>  Subscription ID to remove

Options:
  -h, --help  Print help
```

### `agentos event subscriptions`

```text
Manage event subscriptions

Usage: agentos event subscriptions <COMMAND>

Commands:
  list     List all subscriptions (optionally filtered by agent)
  show     Show details of a subscription
  enable   Enable a subscription
  disable  Disable a subscription
  help     Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos event history`

```text
View recent event history

Usage: agentos event history [OPTIONS]

Options:
      --last <LAST>  Number of recent events to show [default: 20]
  -h, --help         Print help
```

## `agentos identity`

```text
Manage agent cryptographic identities

Usage: agentos identity <COMMAND>

Commands:
  show    Show an agent's Ed25519 cryptographic identity
  revoke  Revoke an agent's cryptographic identity and permissions
  help    Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos identity show`

```text
Show an agent's Ed25519 cryptographic identity

Usage: agentos identity show <AGENT>

Arguments:
  <AGENT>  Agent name

Options:
  -h, --help  Print help
```

### `agentos identity revoke`

```text
Revoke an agent's cryptographic identity and permissions

Usage: agentos identity revoke <AGENT>

Arguments:
  <AGENT>  Agent name

Options:
  -h, --help  Print help
```

## `agentos hal`

```text
Manage hardware device access (HAL)

Usage: agentos hal <COMMAND>

Commands:
  list      List all registered hardware devices and their status
  register  Register a new device (places it in quarantine pending approval)
  approve   Approve a quarantined device for a specific agent
  deny      Quarantine a device and deny access for all agents
  revoke    Revoke a specific agent's access to a device
  help      Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos hal list`

```text
List all registered hardware devices and their status

Usage: agentos hal list

Options:
  -h, --help  Print help
```

### `agentos hal register`

```text
Register a new device (places it in quarantine pending approval)

Usage: agentos hal register --id <ID> --type <DEVICE_TYPE>

Options:
      --id <ID>             Device ID (e.g. gpu:0, usb:1, cam:0)
      --type <DEVICE_TYPE>  Human-readable device type (e.g. "nvidia-rtx-4090", "webcam")
  -h, --help                Print help
```

### `agentos hal approve`

```text
Approve a quarantined device for a specific agent

Usage: agentos hal approve --agent <AGENT> <DEVICE>

Arguments:
  <DEVICE>  Device ID to approve

Options:
      --agent <AGENT>  Agent name to grant access to
  -h, --help           Print help
```

### `agentos hal deny`

```text
Quarantine a device and deny access for all agents

Usage: agentos hal deny <DEVICE>

Arguments:
  <DEVICE>  Device ID to deny

Options:
  -h, --help  Print help
```

### `agentos hal revoke`

```text
Revoke a specific agent's access to a device

Usage: agentos hal revoke --agent <AGENT> <DEVICE>

Arguments:
  <DEVICE>  Device ID

Options:
      --agent <AGENT>  Agent name to revoke access from
  -h, --help           Print help
```

## `agentos web`

```text
Web UI server

Usage: agentos web <COMMAND>

Commands:
  serve  Start the web UI server
  help   Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos web serve`

```text
Start the web UI server

Usage: agentos web serve [OPTIONS]

Options:
      --port <PORT>  Port to bind the web server on [default: 8080]
      --host <HOST>  Host/IP to bind on [default: 127.0.0.1]
  -h, --help         Print help
```

## `agentos log`

```text
Control runtime logging (log level, format)

Usage: agentos log <COMMAND>

Commands:
  set-level  Change the active log level at runtime (no restart required)
  help       Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos log set-level`

```text
Change the active log level at runtime (no restart required)

Usage: agentos log set-level <LEVEL>

Arguments:
  <LEVEL>  Log level: trace | debug | info | warn | error Also accepts compound directives, e.g. "agentos=debug,agentos_kernel=trace"

Options:
  -h, --help  Print help
```

## `agentos healthz`

```text
Check if the kernel health endpoint is responding (used by Docker HEALTHCHECK)

Usage: agentos healthz [OPTIONS]

Options:
      --port <PORT>  Health server port [default: 9091]
  -h, --help         Print help
```

## `agentos notifications`

```text
View and respond to agent notifications

Usage: agentos notifications <COMMAND>

Commands:
  list     List notifications from the user inbox
  read     Show a notification's full body and mark it as read
  respond  Respond to an interactive (Question) notification
  watch    Poll for new notifications every 5 seconds (press Ctrl-C to stop)
  help     Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos notifications list`

```text
List notifications from the user inbox

Usage: agentos notifications list [OPTIONS]

Options:
  -u, --unread         Show only unread notifications
  -n, --limit <LIMIT>  Maximum number of notifications to show [default: 50]
  -h, --help           Print help
```

### `agentos notifications read`

```text
Show a notification's full body and mark it as read

Usage: agentos notifications read <ID>

Arguments:
  <ID>  Notification ID

Options:
  -h, --help  Print help
```

### `agentos notifications respond`

```text
Respond to an interactive (Question) notification

Usage: agentos notifications respond --response <RESPONSE> <ID>

Arguments:
  <ID>  Notification ID

Options:
  -r, --response <RESPONSE>  Your response text
  -h, --help                 Print help
```

### `agentos notifications watch`

```text
Poll for new notifications every 5 seconds (press Ctrl-C to stop)

Usage: agentos notifications watch

Options:
  -h, --help  Print help
```

## `agentos channel`

```text
Manage bidirectional notification channels (Telegram, ntfy, email)

Usage: agentos channel <COMMAND>

Commands:
  connect     Connect a bidirectional notification channel
  set-agent   Set or clear the default agent for inbound channel chat
  disconnect  Disconnect a registered channel
  list        List all registered channels
  test        Send a test notification to a channel
  help        Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos channel connect`

```text
Connect a bidirectional notification channel

Usage: agentos channel connect [OPTIONS] --kind <KIND> --display-name <DISPLAY_NAME>

Options:
  -k, --kind <KIND>
          Channel kind: telegram, ntfy, email
  -e, --external-id <EXTERNAL_ID>
          Channel-specific external identifier (Telegram chat_id, ntfy topic, email address). Optional for Telegram — omit to auto-discover from the first /start message
  -d, --display-name <DISPLAY_NAME>
          Human-readable display name for this channel
  -c, --credential-key <CREDENTIAL_KEY>
          Vault key where the credential (bot token, password) is stored [default: ""]
      --reply-topic <REPLY_TOPIC>
          ntfy reply-topic for inbound messages
      --server-url <SERVER_URL>
          ntfy server URL (default: https://ntfy.sh)
      --webhook-url <WEBHOOK_URL>
          Public URL for Telegram webhook mode (e.g. "https://example.com"). When set, Telegram pushes updates to this URL instead of long-polling
      --active-agent <ACTIVE_AGENT>
          Default agent for inbound chat on this channel (Telegram `/agent` uses this)
  -h, --help
          Print help
```

### `agentos channel set-agent`

```text
Set or clear the default agent for inbound channel chat

Usage: agentos channel set-agent [OPTIONS] --id <ID>

Options:
      --id <ID>        Channel ID (from `channel list`)
      --agent <AGENT>  Agent name (omit or pass empty string to clear)
  -h, --help           Print help
```

### `agentos channel disconnect`

```text
Disconnect a registered channel

Usage: agentos channel disconnect <ID>

Arguments:
  <ID>  Channel ID (from `channel list`)

Options:
  -h, --help  Print help
```

### `agentos channel list`

```text
List all registered channels

Usage: agentos channel list

Options:
  -h, --help  Print help
```

### `agentos channel test`

```text
Send a test notification to a channel

Usage: agentos channel test <ID>

Arguments:
  <ID>  Channel ID (from `channel list`)

Options:
  -h, --help  Print help
```

## `agentos skill`

```text
Manage autonomous skill packages (system prompt + tools + triggers + budget)

Usage: agentos skill <COMMAND>

Commands:
  install   Install a skill from a directory containing SKILL.toml
  remove    Remove an installed skill
  list      List all installed skills
  run       Run a skill by name
  status    Show detailed status of a skill
  new       Create a new skill project from a template
  validate  Validate a SKILL.toml manifest without installing it
  publish   Publish a skill to a local package index
  search    Search the local package index for skills
  help      Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos skill install`

```text
Install a skill from a directory containing SKILL.toml

Usage: agentos skill install <PATH>

Arguments:
  <PATH>  Path to the skill directory

Options:
  -h, --help  Print help
```

### `agentos skill remove`

```text
Remove an installed skill

Usage: agentos skill remove <NAME>

Arguments:
  <NAME>  Skill name to remove

Options:
  -h, --help  Print help
```

### `agentos skill list`

```text
List all installed skills

Usage: agentos skill list

Options:
  -h, --help  Print help
```

### `agentos skill run`

```text
Run a skill by name

Usage: agentos skill run [OPTIONS] <NAME>

Arguments:
  <NAME>  Skill name to run

Options:
      --input <INPUT>  Optional input text for the skill
  -h, --help           Print help
```

### `agentos skill status`

```text
Show detailed status of a skill

Usage: agentos skill status <NAME>

Arguments:
  <NAME>  Skill name

Options:
  -h, --help  Print help
```

### `agentos skill new`

```text
Create a new skill project from a template

Usage: agentos skill new <NAME>

Arguments:
  <NAME>  Skill name (becomes the directory name)

Options:
  -h, --help  Print help
```

### `agentos skill validate`

```text
Validate a SKILL.toml manifest without installing it

Usage: agentos skill validate [PATH]

Arguments:
  [PATH]  Path to the skill directory containing SKILL.toml [default: .]

Options:
  -h, --help  Print help
```

### `agentos skill publish`

```text
Publish a skill to a local package index

Usage: agentos skill publish [OPTIONS] [PATH]

Arguments:
  [PATH]  Path to the skill directory containing SKILL.toml [default: .]

Options:
      --index <INDEX>  Path to the package index JSON file
  -h, --help           Print help
```

### `agentos skill search`

```text
Search the local package index for skills

Usage: agentos skill search [OPTIONS] <QUERY>

Arguments:
  <QUERY>  Search query (matches name, description, tags, author)

Options:
      --index <INDEX>  Path to the package index JSON file
  -h, --help           Print help
```

## `agentos mcp`

```text
MCP (Model Context Protocol) adapter — import/export tools via the standard protocol

Usage: agentos mcp <COMMAND>

Commands:
  serve         Expose all registered AgentOS tools as an MCP server
  tools         List available MCP tools (requires no kernel connection)
  call          Call a single MCP tool and print the result
  list          List MCP server connections configured in the kernel config file
  status        Show live connection health for all configured MCP servers
  attach        Attach an MCP server to the running kernel at runtime
  oauth-store   Store an OAuth2 credential in the vault for MCP server authentication
  detach        Detach an MCP server from the running kernel
  a2a-discover  Discover a remote A2A agent's capabilities (fetch its Agent Card)
  a2a-delegate  Delegate a task to a remote A2A agent
  a2a-card      Show this agent's A2A card (what external agents would see)
  help          Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos mcp serve`

```text
Expose all registered AgentOS tools as an MCP server.

Default transport is stdio (reads stdin, writes stdout) for use with Claude Desktop, Cursor, and similar local MCP clients.

Use `--transport http` to expose via HTTP POST for remote clients.

Examples: # stdio (default) — pipe from Claude Desktop config agentos mcp serve

# HTTP — listen on port 3002 with bearer-token auth agentos mcp serve --transport http --port 3002 --token mysecret

Usage: agentos mcp serve [OPTIONS]

Options:
      --transport <TRANSPORT>
          Transport mode: "stdio" (default) or "http"
          
          [default: stdio]

      --port <PORT>
          Port to listen on (HTTP transport only, default: 3002)
          
          [default: 3002]

      --token <TOKEN>
          Bearer token required for HTTP clients (HTTP transport only). If omitted, no authentication is required

  -h, --help
          Print help (see a summary with '-h')
```

### `agentos mcp tools`

```text
List available MCP tools (requires no kernel connection).

Loads the tool runner from config and prints all available tool names.

Usage: agentos mcp tools

Options:
  -h, --help
          Print help (see a summary with '-h')
```

### `agentos mcp call`

```text
Call a single MCP tool and print the result.

Example: agentos mcp call --tool file-reader --input '{"path": "notes.txt"}'

Usage: agentos mcp call [OPTIONS] --tool <TOOL>

Options:
      --tool <TOOL>
          Name of the tool to invoke

      --input <INPUT>
          JSON input for the tool (defaults to empty object)
          
          [default: {}]

  -h, --help
          Print help (see a summary with '-h')
```

### `agentos mcp list`

```text
List MCP server connections configured in the kernel config file

Usage: agentos mcp list

Options:
  -h, --help  Print help
```

### `agentos mcp status`

```text
Show live connection health for all configured MCP servers.

Requires a running kernel. Reports each server's name, connection state, registered tool count, and last error (if any).

Usage: agentos mcp status

Options:
  -h, --help
          Print help (see a summary with '-h')
```

### `agentos mcp attach`

```text
Attach an MCP server to the running kernel at runtime.

Spawns the server process (stdio) or opens an HTTP connection, performs the MCP handshake, and registers its tools immediately — no restart needed.

The attachment is persisted to SQLite and automatically restored on kernel restart. Use `mcp detach` to remove it permanently.

Examples: agentos mcp attach filesystem -- npx -y @modelcontextprotocol/server-filesystem /tmp agentos mcp attach github --env GITHUB_TOKEN=vault:github_token -- npx -y @modelcontextprotocol/server-github agentos mcp attach remote --url http://localhost:8080/mcp --token mytoken agentos mcp attach zomato --url https://mcp-server.zomato.com/mcp --oauth-connector zomato

Usage: agentos mcp attach [OPTIONS] <NAME> [-- <COMMAND_AND_ARGS>...]

Arguments:
  <NAME>
          Unique name for this server (used in logs, status, and detach)

  [COMMAND_AND_ARGS]...
          Command and arguments for stdio transport (everything after `--`).
          
          Example: `-- npx -y @modelcontextprotocol/server-filesystem /tmp`

Options:
      --url <URL>
          HTTP endpoint URL (for HTTP transport). Mutually exclusive with trailing command

      --token <TOKEN>
          Static Bearer auth token for HTTP transport. Mutually exclusive with `--oauth-connector`

      --oauth-connector <CONNECTOR_ID>
          OAuth2 connector ID referencing a credential stored via `mcp oauth-store`. Enables automatic token refresh and retry on 401. Mutually exclusive with `--token`

      --timeout <TIMEOUT>
          Per-request timeout in seconds (default: 30)

      --env <KEY=VALUE>
          Environment variable for the subprocess in KEY=VALUE format.
          
          Use `vault:SECRET_NAME` as the value to read from the kernel vault: --env GITHUB_TOKEN=vault:github_token
          
          Can be repeated for multiple variables: --env FOO=bar --env BAZ=vault:my_secret

  -h, --help
          Print help (see a summary with '-h')
```

### `agentos mcp oauth-store`

```text
Store an OAuth2 credential in the vault for MCP server authentication.

The credential is encrypted at rest (AES-256-GCM) and referenced by `--oauth-connector` in `mcp attach`. Token refresh is handled automatically.

Examples: # Store a Zomato OAuth credential (obtain the initial token via Claude Desktop or browser) agentos mcp oauth-store zomato \ --provider zomato \ --access-token "eyJ..." \ --refresh-token "dGhp..." \ --token-endpoint "https://accounts.zomato.com/oauth/token" \ --client-id "myapp_client_id" \ --client-secret "myapp_secret" \ --scopes "order:read,order:write" \ --expires-in 3600

Usage: agentos mcp oauth-store [OPTIONS] --access-token <ACCESS_TOKEN> --token-endpoint <TOKEN_ENDPOINT> --client-id <CLIENT_ID> <CONNECTOR_ID>

Arguments:
  <CONNECTOR_ID>
          Unique identifier for this credential (e.g. "zomato", "github"). Used in `mcp attach --oauth-connector <ID>`

Options:
      --provider <PROVIDER>
          Human-readable provider name (e.g. "zomato", "github")
          
          [default: custom]

      --access-token <ACCESS_TOKEN>
          OAuth2 access token obtained from the provider

      --refresh-token <REFRESH_TOKEN>
          OAuth2 refresh token (used to obtain new access tokens on expiry)

      --token-endpoint <TOKEN_ENDPOINT>
          OAuth2 token endpoint URL for refresh requests.
          
          Example: https://accounts.zomato.com/oauth/token

      --client-id <CLIENT_ID>
          OAuth2 client ID registered with the provider

      --client-secret <CLIENT_SECRET>
          OAuth2 client secret (for confidential clients)

      --scopes <SCOPES>
          Comma-separated scopes granted by this token (e.g. "order:read,order:write")

      --expires-in <SECONDS>
          Token lifetime in seconds. Used to compute when the token expires. If omitted, the token is treated as non-expiring

  -h, --help
          Print help (see a summary with '-h')
```

### `agentos mcp detach`

```text
Detach an MCP server from the running kernel.

Closes the connection and removes the server from the supervisor. Requires a running kernel.

Usage: agentos mcp detach <NAME>

Arguments:
  <NAME>
          Name of the server to detach (as given to `mcp attach` or configured at boot)

Options:
  -h, --help
          Print help (see a summary with '-h')
```

### `agentos mcp a2a-discover`

```text
Discover a remote A2A agent's capabilities (fetch its Agent Card).

Example: agentos mcp a2a-discover http://remote-agent.example.com

Usage: agentos mcp a2a-discover <URL>

Arguments:
  <URL>
          Base URL of the remote agent (e.g. http://localhost:3001)

Options:
  -h, --help
          Print help (see a summary with '-h')
```

### `agentos mcp a2a-delegate`

```text
Delegate a task to a remote A2A agent.

Example: agentos mcp a2a-delegate --url http://remote --capability echo --input '{"msg":"hi"}'

Usage: agentos mcp a2a-delegate [OPTIONS] --url <URL> --capability <CAPABILITY>

Options:
      --url <URL>
          Base URL of the remote A2A agent

      --capability <CAPABILITY>
          Capability name to invoke

      --input <INPUT>
          JSON input for the capability (default: {})
          
          [default: {}]

      --token <TOKEN>
          Bearer token for authenticating with the remote agent

  -h, --help
          Print help (see a summary with '-h')
```

### `agentos mcp a2a-card`

```text
Show this agent's A2A card (what external agents would see)

Usage: agentos mcp a2a-card

Options:
  -h, --help  Print help
```

## `agentos a2a`

```text
A2A (Agent-to-Agent) protocol — discover and delegate to external agents

Usage: agentos a2a <COMMAND>

Commands:
  card      Display this agent's A2A identity card
  discover  Discover an external A2A agent's capabilities
  delegate  Delegate a task to an external A2A agent
  tasks     List active A2A task delegations
  help      Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos a2a card`

```text
Display this agent's A2A identity card

Usage: agentos a2a card [OPTIONS]

Options:
      --url <URL>  A2A server URL (default: http://localhost:3001) [default: http://localhost:3001]
  -h, --help       Print help
```

### `agentos a2a discover`

```text
Discover an external A2A agent's capabilities

Usage: agentos a2a discover <AGENT_URL>

Arguments:
  <AGENT_URL>  Base URL of the external agent

Options:
  -h, --help  Print help
```

### `agentos a2a delegate`

```text
Delegate a task to an external A2A agent

Usage: agentos a2a delegate [OPTIONS] --agent <AGENT> --capability <CAPABILITY>

Options:
      --agent <AGENT>            Base URL of the external agent
      --capability <CAPABILITY>  Capability name to invoke
      --input <INPUT>            Input JSON for the capability [default: {}]
      --token <TOKEN>            Bearer token for authenticating with the external agent
      --wait                     Poll until task completes and print result
  -h, --help                     Print help
```

### `agentos a2a tasks`

```text
List active A2A task delegations

Usage: agentos a2a tasks [OPTIONS]

Options:
      --url <URL>  A2A server URL (default: http://localhost:3001) [default: http://localhost:3001]
  -h, --help       Print help
```

## `agentos provider`

```text
List and inspect available LLM providers (built-in + catalog)

Usage: agentos provider <COMMAND>

Commands:
  list     List all available LLM providers (built-in + catalog)
  set-url  Override the base URL for a catalog provider (persisted to providers.toml)
  add      Add or replace a provider in the catalog. Connects any HTTP LLM with configurable auth scheme, endpoint paths, and capabilities
  remove   Remove a provider from the catalog
  probe    Probe `<base_url><models_path>` and refresh the `models` list
  help     Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos provider list`

```text
List all available LLM providers (built-in + catalog)

Usage: agentos provider list

Options:
  -h, --help  Print help
```

### `agentos provider set-url`

```text
Override the base URL for a catalog provider (persisted to providers.toml)

Usage: agentos provider set-url <NAME> <URL>

Arguments:
  <NAME>  Provider name (e.g. lmstudio, groq)
  <URL>   New base URL (e.g. http://localhost:5678/v1)

Options:
  -h, --help  Print help
```

### `agentos provider add`

```text
Add or replace a provider in the catalog. Connects any HTTP LLM with configurable auth scheme, endpoint paths, and capabilities

Usage: agentos provider add [OPTIONS] --name <NAME> --base-url <BASE_URL> --default-model <DEFAULT_MODEL>

Options:
      --name <NAME>
          Catalog name (lowercase, alphanumeric + `-_.`)
      --display-name <DISPLAY_NAME>
          Human-readable display name (defaults to `name`)
      --base-url <BASE_URL>
          Base URL, e.g. `https://api.example.com/v1`
      --api-key-env <API_KEY_ENV>
          Environment variable holding the API key. Empty = no auth [default: ""]
      --compatible-with <COMPATIBLE_WITH>
          Wire format: openai, anthropic, gemini, ollama [default: openai]
      --default-model <DEFAULT_MODEL>
          Default model id (used when agent connects without `--model`)
      --models <MODELS>
          Available model ids (comma separated)
      --vision-models <VISION_MODELS>
          Vision-capable model ids (comma separated)
      --context-window <CONTEXT_WINDOW>
          Override context window in tokens
      --max-output-tokens <MAX_OUTPUT_TOKENS>
          Override max output tokens
      --supports-images <BOOL>
          Override `supports_images` (true|false). Unset = use adapter default [possible values: true, false]
      --supports-tool-calling <BOOL>
          Override `supports_tool_calling`. Unset = adapter default (true) [possible values: true, false]
      --supports-streaming <BOOL>
          Override `supports_streaming`. Unset = adapter default (true) [possible values: true, false]
      --supports-prompt-caching <BOOL>
          Override `supports_prompt_caching`. Unset = adapter default (false) [possible values: true, false]
      --allow-private-hosts
          Permit private/loopback/link-local `base_url`. Required for localhost providers like lmstudio, ollama, vllm
      --auth-header <AUTH_HEADER>
          Auth header name (default `Authorization`). Use `api-key` for Azure
      --auth-prefix <AUTH_PREFIX>
          Auth value prefix (default `"Bearer "`). Use `""` for raw key
      --chat-path <CHAT_PATH>
          Chat completions path (default `/chat/completions`)
      --models-path <MODELS_PATH>
          Models list path (default `/models`)
      --header <EXTRA_HEADERS>
          Extra static headers, repeatable: --header "X-Foo=bar"
  -h, --help
          Print help
```

### `agentos provider remove`

```text
Remove a provider from the catalog

Usage: agentos provider remove <NAME>

Arguments:
  <NAME>  Provider name

Options:
  -h, --help  Print help
```

### `agentos provider probe`

```text
Probe `<base_url><models_path>` and refresh the `models` list

Usage: agentos provider probe <NAME>

Arguments:
  <NAME>  Provider name

Options:
  -h, --help  Print help
```

## `agentos plugin`

```text
Manage plugins — list, enable, disable, and inspect plugin manifests

Usage: agentos plugin <COMMAND>

Commands:
  list     List all discovered plugins and their status
  enable   Activate a plugin by ID
  disable  Deactivate a plugin by ID
  info     Show full details for a specific plugin
  help     Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos plugin list`

```text
List all discovered plugins and their status

Usage: agentos plugin list

Options:
  -h, --help  Print help
```

### `agentos plugin enable`

```text
Activate a plugin by ID

Usage: agentos plugin enable <PLUGIN_ID>

Arguments:
  <PLUGIN_ID>  Plugin ID (e.g. "discord", "memory-embeddings")

Options:
  -h, --help  Print help
```

### `agentos plugin disable`

```text
Deactivate a plugin by ID

Usage: agentos plugin disable <PLUGIN_ID>

Arguments:
  <PLUGIN_ID>  Plugin ID

Options:
  -h, --help  Print help
```

### `agentos plugin info`

```text
Show full details for a specific plugin

Usage: agentos plugin info <PLUGIN_ID>

Arguments:
  <PLUGIN_ID>  Plugin ID

Options:
  -h, --help  Print help
```

## `agentos workspace`

```text
Grant, revoke, or list user filesystem workspace grants.

A grant lets one agent (or every agent) read, write, and/or execute commands inside a specific host directory tree. Without a grant, file tools that target an absolute path outside `data_dir` return `PermissionDenied`.

Usage: agentos workspace <COMMAND>

Commands:
  grant   Grant an agent (or every agent) access to a host directory
  revoke  Revoke an active grant. `--agent` must match the original scope (omit for a global grant; supply for an agent-scoped one)
  list    List active grants. With `--agent`, show grants that apply to that agent (its own + global). Otherwise show every active grant
  help    Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help (see a summary with '-h')
```

### `agentos workspace grant`

```text
Grant an agent (or every agent) access to a host directory.

Examples: agentos workspace grant ~/project --mode rw agentos workspace grant /tmp/work --mode rwx --agent research-bot

Usage: agentos workspace grant [OPTIONS] <PATH>

Arguments:
  <PATH>
          Absolute path or `~/...`. Subpaths are also covered

Options:
      --mode <MODE>
          Permission bits: any combination of `r`, `w`, `x` (default: `rw`)
          
          [default: rw]

      --agent <AGENT>
          Scope to a single agent by display name or `AgentID` UUID (default: global, applies to every agent)

      --yes
          Skip the broad-grant confirmation prompt for paths like `~/Desktop`

  -h, --help
          Print help (see a summary with '-h')
```

### `agentos workspace revoke`

```text
Revoke an active grant. `--agent` must match the original scope (omit for a global grant; supply for an agent-scoped one)

Usage: agentos workspace revoke [OPTIONS] <PATH>

Arguments:
  <PATH>  

Options:
      --agent <AGENT>  
  -h, --help           Print help
```

### `agentos workspace list`

```text
List active grants. With `--agent`, show grants that apply to that agent (its own + global). Otherwise show every active grant

Usage: agentos workspace list [OPTIONS]

Options:
      --agent <AGENT>  
  -h, --help           Print help
```

## `agentos approval`

```text
Manage tool-call approval mode and learned "allow always" policy.

Modes control when the kernel auto-approves vs. escalates a tool call for human review: auto       — approve everything except ControlPlane operations ask_edit   — approve readonly; prompt for writes/exec/control-plane ask_always — prompt for everything except trivially-cheap reads deny       — hard-deny anything that would otherwise prompt

Usage: agentos approval <COMMAND>

Commands:
  mode    Manage the approval mode (global default + per-agent overrides)
  allow   Add a learned "allow always" policy entry. Future calls matching this rule will be auto-approved without prompting, even when the mode would normally escalate
  list    List active learned approval policy entries
  revoke  Revoke a learned policy entry by its numeric `id` (see `approval list`)
  help    Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help (see a summary with '-h')
```

### `agentos approval mode`

```text
Manage the approval mode (global default + per-agent overrides)

Usage: agentos approval mode <COMMAND>

Commands:
  get    Show the current global mode + per-agent overrides
  set    Set the global approval mode
  clear  Clear a per-agent mode override and fall back to the global default
  help   Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos approval allow`

```text
Add a learned "allow always" policy entry. Future calls matching this rule will be auto-approved without prompting, even when the mode would normally escalate

Usage: agentos approval allow [OPTIONS] <TOOL>

Arguments:
  <TOOL>  Tool name (e.g. `file-writer`)

Options:
      --path <PATH>    Optional glob to match against the payload's `path` field (e.g. `/home/alice/project/**`)
      --agent <AGENT>  Scope this entry to a single agent by display name (default: all agents)
  -h, --help           Print help
```

### `agentos approval list`

```text
List active learned approval policy entries

Usage: agentos approval list

Options:
  -h, --help  Print help
```

### `agentos approval revoke`

```text
Revoke a learned policy entry by its numeric `id` (see `approval list`)

Usage: agentos approval revoke <ID>

Arguments:
  <ID>  

Options:
  -h, --help  Print help
```

## `agentos onboard`

```text
Interactive setup wizard — configure providers, agents, and data paths

Usage: agentos onboard

Options:
  -h, --help  Print help
```

## `agentos doctor`

```text
Diagnose configuration issues and optionally auto-repair them

Usage: agentos doctor [OPTIONS]

Options:
      --fix   Attempt to auto-repair detected issues
  -h, --help  Print help
```

## `agentos config`

```text
Read or write configuration values without editing TOML manually

Usage: agentos config <COMMAND>

Commands:
  get   Read a configuration value by dotted key (e.g. llm.primary)
  set   Write a configuration value by dotted key
  list  List all top-level config sections
  help  Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `agentos config get`

```text
Read a configuration value by dotted key (e.g. llm.primary)

Usage: agentos config get <KEY>

Arguments:
  <KEY>  Dotted key path (e.g. llm.primary)

Options:
  -h, --help  Print help
```

### `agentos config set`

```text
Write a configuration value by dotted key

Usage: agentos config set <KEY> <VALUE>

Arguments:
  <KEY>    Dotted key path (e.g. llm.primary)
  <VALUE>  New value to set

Options:
  -h, --help  Print help
```

### `agentos config list`

```text
List all top-level config sections

Usage: agentos config list

Options:
  -h, --help  Print help
```

## `agentos init`

```text
Scaffold a new AgentOS project from a template.

Creates a project directory with a working agent configuration, tool manifests, and an inline README explaining the security model.

Templates: hello-world     — Minimal agent that responds to a prompt secure-agent    — Agent with restricted CapabilityToken (recommended) mcp-server      — Agent exposed as an MCP server multi-agent     — Coordinator + 2 specialist agents

Examples: agentos init my-project agentos init my-project --template secure-agent

Usage: agentos init [OPTIONS] <NAME>

Arguments:
  <NAME>
          Project name (becomes the directory name)

Options:
  -t, --template <TEMPLATE>
          Template to use (default: secure-agent)

          Possible values:
          - hello-world:  Minimal "hello world" agent — simplest possible setup
          - secure-agent: Agent with restricted CapabilityToken (recommended starting point)
          - mcp-server:   Agent exposed as an MCP server for external clients
          - multi-agent:  Coordinator + 2 specialist agents with sub-agent spawning
          
          [default: secure-agent]

  -h, --help
          Print help (see a summary with '-h')
```

