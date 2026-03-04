use agentos_bus::client::BusClient;
use agentos_bus::message::{KernelCommand, KernelResponse};
use clap::Subcommand;

#[derive(Subcommand)]
pub enum ScheduleCommands {
    /// Create a recurring job
    Create {
        /// Name of the schedule
        #[arg(long)]
        name: String,

        /// Cron expression for the schedule
        #[arg(long)]
        cron: String,

        /// Name of the agent to run the task
        #[arg(long)]
        agent: String,

        /// Prompt/task description
        #[arg(long)]
        task: String,

        /// Permissions required for the task (comma-separated, e.g., 'fs.user_data:rw')
        #[arg(long, default_value = "")]
        permissions: String,
    },

    /// List scheduled jobs
    List,

    /// Pause a scheduled job
    Pause {
        /// Name of the schedule
        name: String,
    },

    /// Resume a paused scheduled job
    Resume {
        /// Name of the schedule
        name: String,
    },

    /// Delete a scheduled job
    Delete {
        /// Name of the schedule
        name: String,
    },
}

pub async fn handle(client: &mut BusClient, command: ScheduleCommands) -> anyhow::Result<()> {
    match command {
        ScheduleCommands::Create { name, cron, agent, task, permissions } => {
            let perms = if permissions.is_empty() {
                Vec::new()
            } else {
                permissions.split(',').map(|s| s.trim().to_string()).collect()
            };

            let cmd = KernelCommand::CreateSchedule {
                name,
                cron,
                agent_name: agent,
                task,
                permissions: perms,
            };

            let response = client.send_command(cmd).await?;
            if let KernelResponse::ScheduleId(id) = response {
                println!("✅ Schedule created. ID: {}", id);
            } else if let KernelResponse::Error { message } = response {
                anyhow::bail!("Failed to create schedule: {}", message);
            } else {
                anyhow::bail!("Unexpected response: {:?}", response);
            }
        }
        ScheduleCommands::List => {
            let response = client.send_command(KernelCommand::ListSchedules).await?;
            if let KernelResponse::ScheduleList(jobs) = response {
                if jobs.is_empty() {
                    println!("No scheduled jobs found.");
                    return Ok(());
                }

                println!("{:<20} {:<15} {:<15} {:<10} {:<20} {:<10}", "NAME", "CRON", "AGENT", "STATE", "NEXT RUN", "RUNS");
                for job in jobs {
                    let next_run = job.next_run_at.map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()).unwrap_or_else(|| "N/A".to_string());
                    let state = format!("{:?}", job.state).to_lowercase();
                    println!("{:<20} {:<15} {:<15} {:<10} {:<20} {:<10}",
                             job.name, job.cron_expression, job.agent_name, state, next_run, job.run_count);
                }
            } else if let KernelResponse::Error { message } = response {
                anyhow::bail!("Failed to list schedules: {}", message);
            } else {
                anyhow::bail!("Unexpected response: {:?}", response);
            }
        }
        ScheduleCommands::Pause { name } => {
            let response = client.send_command(KernelCommand::PauseSchedule { name: name.clone() }).await?;
            if let KernelResponse::Success { .. } = response {
                println!("⏸️  Schedule '{}' paused.", name);
            } else if let KernelResponse::Error { message } = response {
                anyhow::bail!("Failed to pause schedule: {}", message);
            } else {
                anyhow::bail!("Unexpected response: {:?}", response);
            }
        }
        ScheduleCommands::Resume { name } => {
            let response = client.send_command(KernelCommand::ResumeSchedule { name: name.clone() }).await?;
            if let KernelResponse::Success { .. } = response {
                println!("▶️  Schedule '{}' resumed.", name);
            } else if let KernelResponse::Error { message } = response {
                anyhow::bail!("Failed to resume schedule: {}", message);
            } else {
                anyhow::bail!("Unexpected response: {:?}", response);
            }
        }
        ScheduleCommands::Delete { name } => {
            let response = client.send_command(KernelCommand::DeleteSchedule { name: name.clone() }).await?;
            if let KernelResponse::Success { .. } = response {
                println!("🗑️  Schedule '{}' deleted.", name);
            } else if let KernelResponse::Error { message } = response {
                anyhow::bail!("Failed to delete schedule: {}", message);
            } else {
                anyhow::bail!("Unexpected response: {:?}", response);
            }
        }
    }
    Ok(())
}
