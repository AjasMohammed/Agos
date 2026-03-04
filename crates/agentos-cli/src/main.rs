use clap::{Parser, Subcommand};
use std::path::Path;
use std::sync::Arc;
use agentos_bus::client::BusClient;
use agentos_kernel::Kernel;

mod commands;
use commands::{
    agent::AgentCommands, task::TaskCommands, tool::ToolCommands,
    secret::SecretCommands, perm::PermCommands, audit::AuditCommands,
    role::RoleCommands, schedule::ScheduleCommands, bg::BgCommands,
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

    let passphrase = match vault_passphrase {
        Some(p) => p,
        None => {
            eprint!("Enter vault passphrase: ");
            rpassword::read_password()?
        }
    };

    println!("🚀 Booting AgentOS kernel...");

    let kernel = Arc::new(Kernel::boot(config_path, &passphrase).await?);

    println!("✅ Kernel started");
    println!("   Bus: {}", kernel.config.bus.socket_path);
    println!("   Tools: {} loaded", kernel.tool_registry.read().await.list_all().len());
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
            "agentctl", "agent", "connect",
            "--provider", "openai",
            "--model", "gpt-4",
            "--name", "analyst-1",
        ]).unwrap();

        match cli.command {
            Commands::Agent { command: AgentCommands::Connect { provider, model, name, base_url: _ } } => {
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
            "agentctl", "task", "run",
            "--agent", "analyst",
            "Summarize the report",
        ]).unwrap();

        match cli.command {
            Commands::Task { command: TaskCommands::Run { agent, prompt } } => {
                assert_eq!(agent, Some("analyst".to_string()));
                assert_eq!(prompt, "Summarize the report");
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
            Commands::Perm { command: PermCommands::Grant { agent, permission, expires: _ } } => {
                assert_eq!(agent, "analyst");
                assert_eq!(permission, "fs.user_data:rw");
            }
            _ => panic!("Wrong command parsed"),
        }
    }

    #[test]
    fn test_cli_parses_schedule_create() {
        let cli = Cli::try_parse_from([
            "agentctl", "schedule", "create",
            "--name", "daily-report",
            "--cron", "0 0 * * * *",
            "--agent", "analyst",
            "--task", "Summarize logs",
            "--permissions", "fs.logs:r"
        ]).unwrap();

        match cli.command {
            Commands::Schedule { command: ScheduleCommands::Create { name, cron, agent, task, permissions } } => {
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
            "agentctl", "bg", "run",
            "--name", "process-data",
            "--agent", "worker",
            "--task", "process all files",
            "--detach"
        ]).unwrap();

        match cli.command {
            Commands::Bg { command: BgCommands::Run { name, agent, task, detach } } => {
                assert_eq!(name, "process-data");
                assert_eq!(agent, "worker");
                assert!(detach);
            }
            _ => panic!("Wrong command parsed"),
        }
    }
}
