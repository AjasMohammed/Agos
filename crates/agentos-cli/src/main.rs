use agentos_bus::client::BusClient;
use agentos_kernel::Kernel;
use agentos_vault::ZeroizingString;
use clap::{Parser, Subcommand};
use std::path::Path;
use std::sync::Arc;

mod commands;
use commands::{
    agent::AgentCommands, audit::AuditCommands, bg::BgCommands, cost::CostCommands,
    escalation::EscalationCommands, event::EventCommands, hal::HalCommands,
    identity::IdentityCommands, perm::PermCommands, pipeline::PipelineCommands,
    resource::ResourceCommands, role::RoleCommands, schedule::ScheduleCommands,
    secret::SecretCommands, snapshot::SnapshotCommands, task::TaskCommands, tool::ToolCommands,
};

#[derive(Parser)]
#[command(name = "agentctl")]
#[command(about = "AgentOS — Control CLI for the LLM-native operating system")]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Path to kernel config file
    #[arg(long, default_value = "config/default.toml")]
    pub config: String,
}

#[derive(Subcommand)]
pub enum Commands {
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

    /// Manage OS roles
    Role {
        #[command(subcommand)]
        command: RoleCommands,
    },

    /// Show system status
    Status,

    /// View audit logs
    Audit {
        #[command(subcommand)]
        command: AuditCommands,
    },

    /// Manage scheduled background jobs
    Schedule {
        #[command(subcommand)]
        command: ScheduleCommands,
    },

    /// Manage background tasks
    Bg {
        #[command(subcommand)]
        command: BgCommands,
    },

    /// Manage multi-agent pipelines
    Pipeline {
        #[command(subcommand)]
        command: PipelineCommands,
    },

    /// View agent cost and budget reports
    Cost {
        #[command(subcommand)]
        command: CostCommands,
    },

    /// Manage resource locks (arbitration)
    Resource {
        #[command(subcommand)]
        command: ResourceCommands,
    },

    /// View and resolve human approval requests from agents
    Escalation {
        #[command(subcommand)]
        command: EscalationCommands,
    },

    /// Manage task snapshots and rollback
    Snapshot {
        #[command(subcommand)]
        command: SnapshotCommands,
    },

    /// Manage event subscriptions and view event history
    Event {
        #[command(subcommand)]
        command: EventCommands,
    },

    /// Manage agent cryptographic identities
    Identity {
        #[command(subcommand)]
        command: IdentityCommands,
    },

    /// Manage hardware device access (HAL)
    Hal {
        #[command(subcommand)]
        command: HalCommands,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("agentos=info")
        .with_target(false)
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Start { vault_passphrase } => {
            cmd_start(&cli.config, vault_passphrase).await?;
        }

        // Offline tool subcommands run without a kernel connection
        Commands::Tool { command } if commands::tool::is_offline(&command) => {
            commands::tool::handle_offline(command)?;
        }

        other => {
            let config_path = Path::new(&cli.config);
            if !config_path.exists() {
                anyhow::bail!("Config file not found: {}", cli.config);
            }
            let config = agentos_kernel::config::load_config(config_path)?;
            let mut client = BusClient::connect(Path::new(&config.bus.socket_path)).await?;
            commands::handle_command(&mut client, other).await?;
        }
    }

    Ok(())
}

async fn cmd_start(config_str: &str, vault_passphrase: Option<String>) -> anyhow::Result<()> {
    let config_path = Path::new(config_str);
    if !config_path.exists() {
        anyhow::bail!("Config file not found: {}", config_str);
    }

    let passphrase = ZeroizingString::new(match vault_passphrase {
        Some(p) => p,
        None => match std::env::var("AGENTOS_VAULT_PASSPHRASE") {
            Ok(env_pass) if !env_pass.is_empty() => env_pass,
            _ => {
                eprint!("Enter vault passphrase: ");
                rpassword::read_password()?
            }
        },
    });

    println!("🚀 Booting AgentOS kernel...");

    let kernel = Arc::new(Kernel::boot(config_path, &passphrase).await?);

    println!("✅ Kernel started");
    println!("   Bus: {}", kernel.config.bus.socket_path);
    println!(
        "   Tools: {} loaded",
        kernel.tool_registry.read().await.list_all().len()
    );
    println!();
    println!("AgentOS is running. Use another terminal to run agentctl commands.");
    println!("Press Ctrl+C to shutdown.");

    kernel.run().await?;

    Ok(())
}

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
            "agentctl",
            "agent",
            "connect",
            "--provider",
            "openai",
            "--model",
            "gpt-4",
            "--name",
            "analyst-1",
        ])
        .unwrap();

        match cli.command {
            Commands::Agent {
                command:
                    AgentCommands::Connect {
                        provider,
                        model,
                        name,
                        base_url: _,
                        roles: _,
                    },
            } => {
                assert_eq!(provider, "openai");
                assert_eq!(model, "gpt-4");
                assert_eq!(name, "analyst-1");
            }
            _ => panic!("Wrong command parsed"),
        }
    }

    #[test]
    fn test_cli_parses_task_run() {
        let cli = Cli::try_parse_from([
            "agentctl",
            "task",
            "run",
            "--agent",
            "analyst",
            "Summarize the report",
        ])
        .unwrap();

        match cli.command {
            Commands::Task {
                command: TaskCommands::Run { agent, prompt },
            } => {
                assert_eq!(agent, Some("analyst".to_string()));
                assert_eq!(prompt, "Summarize the report");
            }
            _ => panic!("Wrong command parsed"),
        }
    }

    #[test]
    fn test_cli_parses_secret_set_with_scope() {
        let cli = Cli::try_parse_from([
            "agentctl",
            "secret",
            "set",
            "SLACK_TOKEN",
            "--scope",
            "agent:notifier",
        ])
        .unwrap();

        match cli.command {
            Commands::Secret {
                command: SecretCommands::Set { name, scope },
            } => {
                assert_eq!(name, "SLACK_TOKEN");
                assert_eq!(scope, "agent:notifier");
            }
            _ => panic!("Wrong command parsed"),
        }
    }

    #[test]
    fn test_cli_parses_perm_grant() {
        let cli = Cli::try_parse_from(["agentctl", "perm", "grant", "analyst", "fs.user_data:rw"])
            .unwrap();

        match cli.command {
            Commands::Perm {
                command:
                    PermCommands::Grant {
                        agent,
                        permission,
                        expires: _,
                    },
            } => {
                assert_eq!(agent, "analyst");
                assert_eq!(permission, "fs.user_data:rw");
            }
            _ => panic!("Wrong command parsed"),
        }
    }

    #[test]
    fn test_cli_parses_schedule_create() {
        let cli = Cli::try_parse_from([
            "agentctl",
            "schedule",
            "create",
            "--name",
            "daily-report",
            "--cron",
            "0 0 * * * *",
            "--agent",
            "analyst",
            "--task",
            "Summarize logs",
            "--permissions",
            "fs.logs:r",
        ])
        .unwrap();

        match cli.command {
            Commands::Schedule {
                command:
                    ScheduleCommands::Create {
                        name,
                        cron,
                        agent,
                        task: _,
                        permissions,
                    },
            } => {
                assert_eq!(name, "daily-report");
                assert_eq!(cron, "0 0 * * * *");
                assert_eq!(agent, "analyst");
                assert_eq!(permissions, "fs.logs:r");
            }
            _ => panic!("Wrong command parsed"),
        }
    }

    #[test]
    fn test_cli_parses_bg_run() {
        let cli = Cli::try_parse_from([
            "agentctl",
            "bg",
            "run",
            "--name",
            "process-data",
            "--agent",
            "worker",
            "--task",
            "process all files",
            "--detach",
        ])
        .unwrap();

        match cli.command {
            Commands::Bg {
                command:
                    BgCommands::Run {
                        name,
                        agent,
                        task: _,
                        detach,
                    },
            } => {
                assert_eq!(name, "process-data");
                assert_eq!(agent, "worker");
                assert!(detach);
            }
            _ => panic!("Wrong command parsed"),
        }
    }

    #[test]
    fn test_cli_parses_event_subscribe() {
        let cli = Cli::try_parse_from([
            "agentctl",
            "event",
            "subscribe",
            "--agent",
            "analyst",
            "--event",
            "AgentAdded",
            "--throttle",
            "once_per:30s",
            "--priority",
            "high",
        ])
        .unwrap();

        match cli.command {
            Commands::Event {
                command:
                    EventCommands::Subscribe {
                        agent,
                        event,
                        filter,
                        throttle,
                        priority,
                    },
            } => {
                assert_eq!(agent, "analyst");
                assert_eq!(event, "AgentAdded");
                assert_eq!(filter, None);
                assert_eq!(throttle, Some("once_per:30s".to_string()));
                assert_eq!(priority, "high");
            }
            _ => panic!("Wrong command parsed"),
        }
    }

    #[test]
    fn test_cli_parses_event_subscribe_with_payload_filter() {
        let cli = Cli::try_parse_from([
            "agentctl",
            "event",
            "subscribe",
            "--agent",
            "analyst",
            "--event",
            "CPUSpikeDetected",
            "--filter",
            "cpu_percent > 90 AND severity == Critical",
        ])
        .unwrap();

        match cli.command {
            Commands::Event {
                command:
                    EventCommands::Subscribe {
                        agent,
                        event,
                        filter,
                        throttle,
                        priority,
                    },
            } => {
                assert_eq!(agent, "analyst");
                assert_eq!(event, "CPUSpikeDetected");
                assert_eq!(
                    filter,
                    Some("cpu_percent > 90 AND severity == Critical".to_string())
                );
                assert_eq!(throttle, None);
                assert_eq!(priority, "normal");
            }
            _ => panic!("Wrong command parsed"),
        }
    }

    #[test]
    fn test_cli_parses_event_subscriptions_list() {
        let cli = Cli::try_parse_from(["agentctl", "event", "subscriptions", "list"]).unwrap();

        match cli.command {
            Commands::Event {
                command: EventCommands::Subscriptions { command: _ },
            } => {}
            _ => panic!("Wrong command parsed"),
        }
    }

    #[test]
    fn test_cli_parses_event_history() {
        let cli = Cli::try_parse_from(["agentctl", "event", "history", "--last", "50"]).unwrap();

        match cli.command {
            Commands::Event {
                command: EventCommands::History { last },
            } => {
                assert_eq!(last, 50);
            }
            _ => panic!("Wrong command parsed"),
        }
    }

    #[test]
    fn test_cli_parses_hal_register() {
        let cli = Cli::try_parse_from([
            "agentctl",
            "hal",
            "register",
            "--id",
            "gpu:0",
            "--type",
            "nvidia-rtx-4090",
        ])
        .unwrap();

        match cli.command {
            Commands::Hal {
                command: HalCommands::Register { id, device_type },
            } => {
                assert_eq!(id, "gpu:0");
                assert_eq!(device_type, "nvidia-rtx-4090");
            }
            _ => panic!("Wrong command parsed"),
        }
    }

    #[test]
    fn test_cli_parses_hal_approve() {
        let cli = Cli::try_parse_from(["agentctl", "hal", "approve", "gpu:0", "--agent", "worker"])
            .unwrap();

        match cli.command {
            Commands::Hal {
                command: HalCommands::Approve { device, agent },
            } => {
                assert_eq!(device, "gpu:0");
                assert_eq!(agent, "worker");
            }
            _ => panic!("Wrong command parsed"),
        }
    }
}
