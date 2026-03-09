use agentos_bus::client::BusClient;
use agentos_bus::message::{KernelCommand, KernelResponse};
use clap::Subcommand;

#[derive(Subcommand)]
pub enum BgCommands {
    /// Run a one-shot background task (detached)
    Run {
        /// Name of the background task
        #[arg(long)]
        name: String,

        /// Name of the agent to run the task
        #[arg(long)]
        agent: String,

        /// Prompt/task description
        #[arg(long)]
        task: String,

        /// Detach the task immediately
        #[arg(long, default_value_t = false)]
        detach: bool,
    },

    /// List background tasks
    List,

    /// Follow logs for a background task
    Logs {
        /// Name of the background task
        name: String,

        /// Follow the logs continuously
        #[arg(long, default_value_t = false)]
        follow: bool,
    },

    /// Kill a running background task
    Kill {
        /// Name of the background task
        name: String,
    },
}

pub async fn handle(client: &mut BusClient, command: BgCommands) -> anyhow::Result<()> {
    match command {
        BgCommands::Run {
            name,
            agent,
            task,
            detach,
        } => {
            let cmd = KernelCommand::RunBackground {
                name: name.clone(),
                agent_name: agent,
                task,
                detach,
            };

            let response = client.send_command(cmd).await?;
            if let KernelResponse::Success { data } = response {
                let data = data.ok_or_else(|| anyhow::anyhow!("Missing response data"))?;
                let task_id = data
                    .get("task_id")
                    .ok_or_else(|| anyhow::anyhow!("Missing 'task_id' in response data"))?
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("'task_id' is not a string"))?
                    .to_string();
                println!("🚀 Background task '{}' started. ID: {}", name, task_id);
            } else if let KernelResponse::Error { message } = response {
                anyhow::bail!("Failed to start background task: {}", message);
            } else {
                anyhow::bail!("Unexpected response: {:?}", response);
            }
        }
        BgCommands::List => {
            let response = client.send_command(KernelCommand::ListBackground).await?;
            if let KernelResponse::BackgroundPoolList(tasks) = response {
                if tasks.is_empty() {
                    println!("No background tasks found.");
                    return Ok(());
                }

                println!(
                    "{:<20} {:<15} {:<15} {:<20} {:<20}",
                    "NAME", "AGENT", "STATE", "STARTED", "COMPLETED"
                );
                for task in tasks {
                    let started = task
                        .started_at
                        .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
                        .unwrap_or_else(|| "N/A".to_string());
                    let completed = task
                        .completed_at
                        .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
                        .unwrap_or_else(|| "N/A".to_string());
                    let state = format!("{:?}", task.state).to_lowercase();
                    println!(
                        "{:<20} {:<15} {:<15} {:<20} {:<20}",
                        task.name, task.agent_name, state, started, completed
                    );
                }
            } else if let KernelResponse::Error { message } = response {
                anyhow::bail!("Failed to list background tasks: {}", message);
            } else {
                anyhow::bail!("Unexpected response: {:?}", response);
            }
        }
        BgCommands::Logs { name, follow } => {
            let response = client
                .send_command(KernelCommand::GetBackgroundLogs {
                    name: name.clone(),
                    follow,
                })
                .await?;
            if let KernelResponse::TaskLogs(logs) = response {
                for log in logs {
                    println!("{}", log);
                }
            } else if let KernelResponse::Error { message } = response {
                anyhow::bail!("Failed to get background logs: {}", message);
            } else {
                anyhow::bail!("Unexpected response: {:?}", response);
            }
        }
        BgCommands::Kill { name } => {
            let response = client
                .send_command(KernelCommand::KillBackground { name: name.clone() })
                .await?;
            if let KernelResponse::Success { .. } = response {
                println!("🛑 Background task '{}' killed.", name);
            } else if let KernelResponse::Error { message } = response {
                anyhow::bail!("Failed to kill background task: {}", message);
            } else {
                anyhow::bail!("Unexpected response: {:?}", response);
            }
        }
    }
    Ok(())
}
