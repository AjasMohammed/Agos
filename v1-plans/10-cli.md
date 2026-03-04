# Plan 10 — CLI (`agentos-cli` crate)

## Goal

Build `agentctl` — the user-facing CLI binary. It connects to the kernel over the Unix domain socket bus and sends `KernelCommand` messages, displaying the responses. Uses `clap` for argument parsing.

## Dependencies

- `agentos-types`
- `agentos-kernel` (for embedded kernel mode)
- `agentos-bus` (for `BusClient`)
- `clap` (with `derive` feature)
- `tokio`
- `serde_json`
- `anyhow`
- `tracing`, `tracing-subscriber`
- `rpassword` — for hidden password/key input

**Add to workspace Cargo.toml:**

```toml
rpassword = "7"
```

## Architecture

Phase 1 CLI operates in **embedded mode**: the CLI binary starts the kernel in-process, then handles commands. This avoids the complexity of a separate daemon process.

```
agentctl start              → boots kernel, starts bus server, waits
agentctl agent connect ...  → sends ConnectAgent command to kernel
agentctl task run ...       → sends RunTask command, waits for result
agentctl secret set ...     → interactive: prompts for value, sends SetSecret
```

## Command Structure

```
agentctl
├── start                         # Boot the kernel (foreground)
├── agent
│   ├── connect                   # Connect an LLM agent
│   │   --provider <ollama|openai|anthropic|gemini>
│   │   --model <model-name>
│   │   --name <agent-name>
│   ├── list                      # List connected agents
│   └── disconnect <agent-name>   # Disconnect an agent
├── task
│   ├── run                       # Run a task
│   │   --agent <agent-name>
│   │   <prompt>                  # The task as a string
│   ├── list                      # List all tasks
│   ├── logs <task-id>            # Get logs for a task
│   └── cancel <task-id>          # Cancel a running task
├── tool
│   ├── list                      # List installed tools
│   ├── install <path>            # Install a tool from manifest
│   └── remove <tool-name>        # Remove a tool
├── secret
│   ├── set <name>                # Set a secret (interactive input)
│   │   [--scope agent:<name>]
│   │   [--scope tool:<name>]
│   ├── list                      # List secrets (metadata only)
│   ├── revoke <name>             # Delete a secret
│   └── rotate <name>             # Rotate a secret (interactive input)
├── perm
│   ├── grant <agent> <perm>      # Grant permission (e.g. "fs.user_data:rw")
│   ├── revoke <agent> <perm>     # Revoke permission
│   └── show <agent>              # Show all permissions for an agent
├── status                        # System status overview
└── audit
    └── logs                      # View audit log
        [--last <N>]
```

## Clap Derive Structs

```rust
// src/main.rs
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "agentctl")]
#[command(about = "AgentOS — Control CLI for the LLM-native operating system")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Path to kernel config file
    #[arg(long, default_value = "config/default.toml")]
    config: String,
}

#[derive(Subcommand)]
enum Commands {
    /// Boot the AgentOS kernel
    Start {
        /// Vault passphrase (will prompt interactively if not provided)
        #[arg(long)]
        vault_passphrase: Option<String>,
    },

    /// Manage LLM agents
    Agent {
        #[command(subcommand)]
        command: AgentCommands,
    },

    /// Manage tasks
    Task {
        #[command(subcommand)]
        command: TaskCommands,
    },

    /// Manage tools
    Tool {
        #[command(subcommand)]
        command: ToolCommands,
    },

    /// Manage secrets
    Secret {
        #[command(subcommand)]
        command: SecretCommands,
    },

    /// Manage agent permissions
    Perm {
        #[command(subcommand)]
        command: PermCommands,
    },

    /// Show system status
    Status,

    /// View audit logs
    Audit {
        #[command(subcommand)]
        command: AuditCommands,
    },
}

#[derive(Subcommand)]
enum AgentCommands {
    /// Connect a new LLM agent
    Connect {
        /// LLM provider
        #[arg(long)]
        provider: String,
        /// Model name
        #[arg(long)]
        model: String,
        /// Agent display name
        #[arg(long)]
        name: String,
    },
    /// List connected agents
    List,
    /// Disconnect an agent
    Disconnect {
        /// Agent name to disconnect
        name: String,
    },
}

#[derive(Subcommand)]
enum TaskCommands {
    /// Run a task
    Run {
        /// Agent to assign the task to
        #[arg(long)]
        agent: String,
        /// The task prompt
        prompt: String,
    },
    /// List all tasks
    List,
    /// View task logs
    Logs {
        /// Task ID
        task_id: String,
    },
    /// Cancel a running task
    Cancel {
        /// Task ID
        task_id: String,
    },
}

#[derive(Subcommand)]
enum ToolCommands {
    /// List installed tools
    List,
    /// Install a tool from its manifest file
    Install {
        /// Path to the tool manifest (.toml)
        path: String,
    },
    /// Remove an installed tool
    Remove {
        /// Tool name to remove
        name: String,
    },
}

#[derive(Subcommand)]
enum SecretCommands {
    /// Set a secret (value entered interactively — never in shell args)
    Set {
        /// Secret name (e.g. OPENAI_API_KEY)
        name: String,
        /// Scope: "global", "agent:<name>", or "tool:<name>"
        #[arg(long, default_value = "global")]
        scope: String,
    },
    /// List all secrets (metadata only — values never shown)
    List,
    /// Revoke (delete) a secret
    Revoke {
        /// Secret name
        name: String,
    },
    /// Rotate a secret (new value entered interactively)
    Rotate {
        /// Secret name
        name: String,
    },
}

#[derive(Subcommand)]
enum PermCommands {
    /// Grant a permission to an agent
    Grant {
        /// Agent name
        agent: String,
        /// Permission string (e.g. "fs.user_data:rw")
        permission: String,
    },
    /// Revoke a permission from an agent
    Revoke {
        /// Agent name
        agent: String,
        /// Permission string
        permission: String,
    },
    /// Show all permissions for an agent
    Show {
        /// Agent name
        agent: String,
    },
}

#[derive(Subcommand)]
enum AuditCommands {
    /// View recent audit log entries
    Logs {
        /// Number of recent entries to show
        #[arg(long, default_value = "50")]
        last: u32,
    },
}
```

## Main Entry Point

```rust
// src/main.rs

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter("agentos=info")
        .with_target(false)
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Start { vault_passphrase } => {
            cmd_start(&cli.config, vault_passphrase).await?;
        }
        other => {
            // All other commands connect to a running kernel
            let config = load_config(&cli.config)?;
            let mut client = BusClient::connect(Path::new(&config.bus.socket_path)).await?;
            handle_command(&mut client, other).await?;
        }
    }

    Ok(())
}

async fn cmd_start(config_path: &str, vault_passphrase: Option<String>) -> anyhow::Result<()> {
    // Prompt for vault passphrase if not provided
    let passphrase = match vault_passphrase {
        Some(p) => p,
        None => {
            eprint!("Enter vault passphrase: ");
            rpassword::read_password()?
        }
    };

    println!("🚀 Booting AgentOS kernel...");

    let kernel = Arc::new(
        Kernel::boot(Path::new(config_path), &passphrase).await?
    );

    println!("✅ Kernel started");
    println!("   Bus: {}", kernel.config.bus.socket_path);
    println!("   Tools: {} loaded", kernel.tool_registry.read().await.list_all().len());
    println!();
    println!("AgentOS is running. Use another terminal to run agentctl commands.");
    println!("Press Ctrl+C to shutdown.");

    // Run the kernel (blocks until shutdown)
    kernel.run().await?;

    Ok(())
}
```

## Command Handler

```rust
// src/commands/mod.rs
pub mod agent;
pub mod task;
pub mod tool;
pub mod secret;
pub mod perm;
pub mod status;

// Dispatch from main.rs
async fn handle_command(client: &mut BusClient, command: Commands) -> anyhow::Result<()> {
    match command {
        Commands::Agent { command } => agent::handle(client, command).await,
        Commands::Task { command } => task::handle(client, command).await,
        Commands::Tool { command } => tool::handle(client, command).await,
        Commands::Secret { command } => secret::handle(client, command).await,
        Commands::Perm { command } => perm::handle(client, command).await,
        Commands::Status => status::handle(client).await,
        Commands::Audit { command } => audit::handle(client, command).await,
        _ => unreachable!(),
    }
}
```

## Example Command Implementation: `secret set`

```rust
// src/commands/secret.rs

pub async fn handle(client: &mut BusClient, command: SecretCommands) -> anyhow::Result<()> {
    match command {
        SecretCommands::Set { name, scope } => {
            // Interactive input — value never appears in shell history
            eprint!("Enter value for '{}' (input hidden): ", name);
            let value = rpassword::read_password()?;

            if value.is_empty() {
                anyhow::bail!("Secret value cannot be empty");
            }

            let scope = parse_scope(&scope)?;

            let response = client.send_command(KernelCommand::SetSecret {
                name: name.clone(),
                value,
                scope,
            }).await?;

            match response {
                KernelResponse::Success { .. } => {
                    println!("✅ Secret '{}' stored securely", name);
                }
                KernelResponse::Error { message } => {
                    eprintln!("❌ Error: {}", message);
                }
                _ => eprintln!("❌ Unexpected response"),
            }
        }

        SecretCommands::List => {
            let response = client.send_command(KernelCommand::ListSecrets).await?;
            match response {
                KernelResponse::SecretList(secrets) => {
                    if secrets.is_empty() {
                        println!("No secrets stored.");
                    } else {
                        println!("{:<25} {:<20} {}", "NAME", "SCOPE", "LAST USED");
                        println!("{}", "-".repeat(65));
                        for s in secrets {
                            let scope_str = format!("{:?}", s.scope);
                            let last_used = s.last_used_at
                                .map(|t| t.to_string())
                                .unwrap_or_else(|| "never".into());
                            println!("{:<25} {:<20} {}", s.name, scope_str, last_used);
                        }
                    }
                }
                _ => eprintln!("❌ Unexpected response"),
            }
        }

        SecretCommands::Revoke { name } => {
            let response = client.send_command(KernelCommand::RevokeSecret { name: name.clone() }).await?;
            match response {
                KernelResponse::Success { .. } => println!("✅ Secret '{}' revoked", name),
                KernelResponse::Error { message } => eprintln!("❌ Error: {}", message),
                _ => eprintln!("❌ Unexpected response"),
            }
        }

        SecretCommands::Rotate { name } => {
            eprint!("Enter new value for '{}' (input hidden): ", name);
            let new_value = rpassword::read_password()?;

            let response = client.send_command(KernelCommand::RotateSecret {
                name: name.clone(),
                new_value,
            }).await?;

            match response {
                KernelResponse::Success { .. } => println!("✅ Secret '{}' rotated", name),
                KernelResponse::Error { message } => eprintln!("❌ Error: {}", message),
                _ => eprintln!("❌ Unexpected response"),
            }
        }
    }
    Ok(())
}

fn parse_scope(s: &str) -> anyhow::Result<SecretScope> {
    match s {
        "global" => Ok(SecretScope::Global),
        s if s.starts_with("agent:") => {
            let name = s.strip_prefix("agent:").unwrap();
            // Note: in real impl, need to look up AgentID by name via kernel
            // For now, store as a string scope that kernel resolves
            Ok(SecretScope::Global) // simplified for Phase 1
        }
        s if s.starts_with("tool:") => {
            Ok(SecretScope::Global) // simplified for Phase 1
        }
        _ => anyhow::bail!("Invalid scope: '{}'. Use 'global', 'agent:<name>', or 'tool:<name>'"),
    }
}
```

## Example Command: `task run`

```rust
// src/commands/task.rs

pub async fn handle(client: &mut BusClient, command: TaskCommands) -> anyhow::Result<()> {
    match command {
        TaskCommands::Run { agent, prompt } => {
            println!("📝 Submitting task to agent '{}'...", agent);
            println!("   Prompt: {}", if prompt.len() > 80 {
                format!("{}...", &prompt[..80])
            } else {
                prompt.clone()
            });

            let response = client.send_command(KernelCommand::RunTask {
                agent_name: agent,
                prompt,
            }).await?;

            match response {
                KernelResponse::Success { data } => {
                    if let Some(data) = data {
                        println!("\n✅ Task completed:\n");
                        if let Some(result) = data.get("result").and_then(|v| v.as_str()) {
                            println!("{}", result);
                        } else {
                            println!("{}", serde_json::to_string_pretty(&data)?);
                        }
                    }
                }
                KernelResponse::Error { message } => {
                    eprintln!("❌ Task failed: {}", message);
                }
                _ => eprintln!("❌ Unexpected response"),
            }
        }

        TaskCommands::List => {
            let response = client.send_command(KernelCommand::ListTasks).await?;
            match response {
                KernelResponse::TaskList(tasks) => {
                    if tasks.is_empty() {
                        println!("No tasks.");
                    } else {
                        println!("{:<38} {:<10} {:<15} {}", "TASK ID", "STATE", "AGENT", "PROMPT");
                        println!("{}", "-".repeat(90));
                        for t in tasks {
                            println!("{:<38} {:<10} {:<15} {}",
                                t.id, format!("{:?}", t.state), t.agent_id,
                                if t.prompt_preview.len() > 40 {
                                    format!("{}...", &t.prompt_preview[..40])
                                } else {
                                    t.prompt_preview
                                }
                            );
                        }
                    }
                }
                _ => eprintln!("❌ Unexpected response"),
            }
        }

        TaskCommands::Logs { task_id } => {
            let tid = TaskID::from_uuid(uuid::Uuid::parse_str(&task_id)?);
            let response = client.send_command(KernelCommand::GetTaskLogs { task_id: tid }).await?;
            match response {
                KernelResponse::TaskLogs(logs) => {
                    for line in logs {
                        println!("{}", line);
                    }
                }
                _ => eprintln!("❌ Unexpected response"),
            }
        }

        TaskCommands::Cancel { task_id } => {
            let tid = TaskID::from_uuid(uuid::Uuid::parse_str(&task_id)?);
            let response = client.send_command(KernelCommand::CancelTask { task_id: tid }).await?;
            match response {
                KernelResponse::Success { .. } => println!("✅ Task cancelled"),
                KernelResponse::Error { message } => eprintln!("❌ Error: {}", message),
                _ => eprintln!("❌ Unexpected response"),
            }
        }
    }
    Ok(())
}
```

## Example: `status`

```rust
// src/commands/status.rs

pub async fn handle(client: &mut BusClient) -> anyhow::Result<()> {
    let response = client.send_command(KernelCommand::GetStatus).await?;
    match response {
        KernelResponse::Status(status) => {
            println!("🟢 AgentOS Status");
            println!("   Uptime:           {}s", status.uptime_secs);
            println!("   Connected agents: {}", status.connected_agents);
            println!("   Active tasks:     {}", status.active_tasks);
            println!("   Installed tools:  {}", status.installed_tools);
            println!("   Audit entries:    {}", status.total_audit_entries);
        }
        _ => eprintln!("❌ Cannot reach kernel. Is it running?"),
    }
    Ok(())
}
```

## Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_parses_start_command() {
        let cli = Cli::try_parse_from(["agentctl", "start"]).unwrap();
        assert!(matches!(cli.command, Commands::Start { .. }));
    }

    #[test]
    fn test_cli_parses_agent_connect() {
        let cli = Cli::try_parse_from([
            "agentctl", "agent", "connect",
            "--provider", "ollama",
            "--model", "llama3.2",
            "--name", "analyst",
        ]).unwrap();

        match cli.command {
            Commands::Agent { command: AgentCommands::Connect { provider, model, name } } => {
                assert_eq!(provider, "ollama");
                assert_eq!(model, "llama3.2");
                assert_eq!(name, "analyst");
            }
            _ => panic!("Wrong command parsed"),
        }
    }

    #[test]
    fn test_cli_parses_task_run() {
        let cli = Cli::try_parse_from([
            "agentctl", "task", "run",
            "--agent", "analyst",
            "Summarize all log files",
        ]).unwrap();

        match cli.command {
            Commands::Task { command: TaskCommands::Run { agent, prompt } } => {
                assert_eq!(agent, "analyst");
                assert_eq!(prompt, "Summarize all log files");
            }
            _ => panic!("Wrong command parsed"),
        }
    }

    #[test]
    fn test_cli_parses_secret_set_with_scope() {
        let cli = Cli::try_parse_from([
            "agentctl", "secret", "set", "SLACK_TOKEN",
            "--scope", "agent:notifier",
        ]).unwrap();

        match cli.command {
            Commands::Secret { command: SecretCommands::Set { name, scope } } => {
                assert_eq!(name, "SLACK_TOKEN");
                assert_eq!(scope, "agent:notifier");
            }
            _ => panic!("Wrong command parsed"),
        }
    }

    #[test]
    fn test_cli_parses_perm_grant() {
        let cli = Cli::try_parse_from([
            "agentctl", "perm", "grant", "analyst", "fs.user_data:rw",
        ]).unwrap();

        match cli.command {
            Commands::Perm { command: PermCommands::Grant { agent, permission } } => {
                assert_eq!(agent, "analyst");
                assert_eq!(permission, "fs.user_data:rw");
            }
            _ => panic!("Wrong command parsed"),
        }
    }
}
```

## Verification

```bash
# Unit tests (CLI parsing)
cargo test -p agentos-cli

# Build the binary
cargo build -p agentos-cli

# Verify help output
./target/debug/agentctl --help
./target/debug/agentctl agent --help
./target/debug/agentctl secret --help
./target/debug/agentctl perm --help
```
