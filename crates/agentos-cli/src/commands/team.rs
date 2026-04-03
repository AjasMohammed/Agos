use agentos_bus::client::BusClient;
use agentos_bus::message::{KernelCommand, KernelResponse};
use clap::Subcommand;

#[derive(Subcommand)]
pub enum TeamCommands {
    /// Run a team defined in a TOML config file against its declared goal.
    Run {
        /// Path to the team TOML config file.
        #[arg(short, long)]
        config: String,
    },
    /// List active team runs (by coordinator task).
    List,
}

pub async fn handle(client: &mut BusClient, cmd: TeamCommands) -> anyhow::Result<()> {
    match cmd {
        TeamCommands::Run { config } => {
            let config_str = std::fs::read_to_string(&config)
                .map_err(|e| anyhow::anyhow!("failed to read config file '{}': {}", config, e))?;
            let team_config: agentos_types::TeamConfig = toml::from_str(&config_str)
                .map_err(|e| anyhow::anyhow!("invalid team TOML: {}", e))?;
            let config_json = serde_json::to_string(&team_config)?;

            match client
                .send_command(KernelCommand::RunTeam {
                    config: config_json,
                })
                .await?
            {
                KernelResponse::TeamStarted {
                    coordinator_task_id,
                    ..
                } => {
                    println!("Team '{}' started.", team_config.name);
                    println!("Coordinator task: {}", coordinator_task_id);
                }
                KernelResponse::Error { message } => {
                    anyhow::bail!("Failed to start team: {}", message);
                }
                other => {
                    anyhow::bail!("Unexpected response: {:?}", other);
                }
            }
        }
        TeamCommands::List => match client.send_command(KernelCommand::ListTasks).await? {
            KernelResponse::TaskList(tasks) => {
                // Filter reliably on the `is_team_coordinator` flag — not fragile
                // prompt-prefix matching.
                let team_tasks: Vec<_> = tasks.iter().filter(|t| t.is_team_coordinator).collect();
                if team_tasks.is_empty() {
                    println!("No active team runs.");
                } else {
                    println!(
                        "{:<38} {:<12} {:<6} GOAL PREVIEW",
                        "COORDINATOR TASK ID", "STATE", "DEPTH"
                    );
                    println!("{}", "-".repeat(90));
                    for t in team_tasks {
                        let preview = &t.prompt_preview[..60.min(t.prompt_preview.len())];
                        println!(
                            "{:<38} {:<12} {:<6} {}",
                            t.id,
                            format!("{:?}", t.state),
                            t.spawn_depth,
                            preview
                        );
                    }
                }
            }
            KernelResponse::Error { message } => {
                anyhow::bail!("Failed to list tasks: {}", message);
            }
            other => {
                anyhow::bail!("Unexpected response: {:?}", other);
            }
        },
    }
    Ok(())
}
