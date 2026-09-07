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

        /// Cron expression (5-field: 'min hr dom mon dow', or 6-field with seconds: 'sec min hr dom mon dow')
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
        /// Name or ID (UUID) of the schedule
        name: String,
    },

    /// Resume a paused scheduled job
    Resume {
        /// Name or ID (UUID) of the schedule
        name: String,
    },

    /// Delete a scheduled job
    Delete {
        /// Name or ID (UUID) of the schedule
        name: String,
    },
}

pub async fn handle(client: &mut BusClient, command: ScheduleCommands) -> anyhow::Result<()> {
    match command {
        ScheduleCommands::Create {
            name,
            cron,
            agent,
            task,
            permissions,
        } => {
            let perms = if permissions.is_empty() {
                Vec::new()
            } else {
                permissions
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .collect()
            };

            let cmd = KernelCommand::CreateSchedule {
                name: name.clone(),
                cron,
                agent_name: agent,
                task,
                permissions: perms,
            };

            let response = client.send_command(cmd).await?;
            if let KernelResponse::ScheduleId(id) = response {
                println!(
                    "✅ Schedule '{}' created (id: {}). Use 'agentos schedule list' to view.",
                    name, id
                );
            } else if let KernelResponse::Error { message } = response {
                anyhow::bail!("Failed to create schedule: {}", message);
            } else {
                anyhow::bail!("Unexpected response: {:?}", response);
            }
        }
        ScheduleCommands::List => {
            let r1 = client.send_command(KernelCommand::ListSchedules).await;
            let r2 = client.send_command(KernelCommand::ListOnceJobs).await;
            let r3 = client.send_command(KernelCommand::ListTimers).await;

            struct Row {
                name: String,
                kind: &'static str,
                schedule: String,
                agent: String,
                state: String,
                next_run: String,
            }

            let mut rows: Vec<Row> = Vec::new();

            match r1? {
                KernelResponse::ScheduleList(jobs) => {
                    for job in jobs {
                        rows.push(Row {
                            name: job.name,
                            kind: "cron",
                            schedule: job.cron_expression,
                            agent: job.agent_name,
                            state: format!("{:?}", job.state).to_lowercase(),
                            next_run: job
                                .next_run_at
                                .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                                .unwrap_or_else(|| "N/A".to_string()),
                        });
                    }
                }
                KernelResponse::Error { message } => {
                    anyhow::bail!("Failed to list schedules: {}", message)
                }
                other => anyhow::bail!("Unexpected response: {:?}", other),
            }

            match r2? {
                KernelResponse::OnceJobList(jobs) => {
                    for job in jobs {
                        rows.push(Row {
                            name: job.name,
                            kind: "once",
                            schedule: job.fire_at.format("%Y-%m-%d %H:%M").to_string(),
                            agent: job.agent_name,
                            state: format!("{:?}", job.state).to_lowercase(),
                            next_run: job.fire_at.format("%Y-%m-%d %H:%M").to_string(),
                        });
                    }
                }
                KernelResponse::Error { message } => {
                    anyhow::bail!("Failed to list once-jobs: {}", message)
                }
                other => anyhow::bail!("Unexpected response: {:?}", other),
            }

            match r3? {
                KernelResponse::TimerList(timers) => {
                    for timer in timers {
                        rows.push(Row {
                            name: timer.name,
                            kind: "timer",
                            schedule: timer.fire_at.format("%Y-%m-%d %H:%M").to_string(),
                            agent: timer.agent_name,
                            state: "pending".to_string(),
                            next_run: timer.fire_at.format("%Y-%m-%d %H:%M").to_string(),
                        });
                    }
                }
                KernelResponse::Error { message } => {
                    anyhow::bail!("Failed to list timers: {}", message)
                }
                other => anyhow::bail!("Unexpected response: {:?}", other),
            }

            if rows.is_empty() {
                println!("No scheduled jobs found.");
                return Ok(());
            }

            println!(
                "{:<20} {:<6} {:<17} {:<15} {:<10} {:<16}",
                "NAME", "KIND", "SCHEDULE/FIRE-AT", "AGENT", "STATE", "NEXT RUN"
            );
            for row in rows {
                println!(
                    "{:<20} {:<6} {:<17} {:<15} {:<10} {:<16}",
                    row.name, row.kind, row.schedule, row.agent, row.state, row.next_run
                );
            }
        }
        ScheduleCommands::Pause { name } => {
            let response = client
                .send_command(KernelCommand::PauseSchedule { name: name.clone() })
                .await?;
            if let KernelResponse::Success { .. } = response {
                println!("⏸️  Schedule '{}' paused.", name);
            } else if let KernelResponse::Error { message } = response {
                anyhow::bail!("Failed to pause schedule: {}", message);
            } else {
                anyhow::bail!("Unexpected response: {:?}", response);
            }
        }
        ScheduleCommands::Resume { name } => {
            let response = client
                .send_command(KernelCommand::ResumeSchedule { name: name.clone() })
                .await?;
            if let KernelResponse::Success { .. } = response {
                println!("▶️  Schedule '{}' resumed.", name);
            } else if let KernelResponse::Error { message } = response {
                anyhow::bail!("Failed to resume schedule: {}", message);
            } else {
                anyhow::bail!("Unexpected response: {:?}", response);
            }
        }
        ScheduleCommands::Delete { name } => {
            let response = client
                .send_command(KernelCommand::DeleteSchedule { name: name.clone() })
                .await?;
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
