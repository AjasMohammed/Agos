use agentos_bus::client::BusClient;
use agentos_bus::message::{KernelCommand, KernelResponse};
use agentos_types::schedule::TimerAction;
use clap::Subcommand;

#[derive(Subcommand)]
pub enum TimerCommands {
    /// Create a one-shot timer (fires once after delay)
    Create {
        /// Name of the timer (must be unique among active timers)
        #[arg(long)]
        name: String,

        /// Seconds until the timer fires (1–86400)
        #[arg(long)]
        delay: u64,

        /// Agent to run the task (required for 'run_task' action)
        #[arg(long)]
        agent: String,

        /// Action: notify, run_task, or run_task_and_notify
        #[arg(long, default_value = "notify")]
        action: String,

        /// Notification subject
        #[arg(long, default_value = "Timer fired")]
        subject: String,

        /// Notification body
        #[arg(long, default_value = "")]
        body: String,

        /// Task prompt (required for run_task/run_task_and_notify)
        #[arg(long, default_value = "")]
        prompt: String,
    },

    /// List pending timers
    List,

    /// Cancel a pending timer
    Cancel {
        /// Name of the timer to cancel
        name: String,
    },
}

pub async fn handle(client: &mut BusClient, command: TimerCommands) -> anyhow::Result<()> {
    match command {
        TimerCommands::Create {
            name,
            delay,
            agent,
            action,
            subject,
            body,
            prompt,
        } => {
            let timer_action: TimerAction = match action.as_str() {
                "notify" => TimerAction::NotifyUser {
                    subject,
                    body,
                    priority: "info".into(),
                },
                "run_task" => {
                    if prompt.is_empty() {
                        anyhow::bail!("--prompt is required for action 'run_task'");
                    }
                    TimerAction::RunTask { prompt }
                }
                "run_task_and_notify" => {
                    if prompt.is_empty() {
                        anyhow::bail!("--prompt is required for action 'run_task_and_notify'");
                    }
                    TimerAction::RunTaskAndNotify {
                        prompt,
                        subject,
                        body,
                        priority: "info".into(),
                    }
                }
                other => anyhow::bail!(
                    "Unknown action '{}'. Valid: notify, run_task, run_task_and_notify",
                    other
                ),
            };

            let cmd = KernelCommand::CreateTimer {
                name: name.clone(),
                delay_secs: delay,
                agent_name: agent,
                action: serde_json::to_string(&timer_action)?,
            };

            let response = client.send_command(cmd).await?;
            match response {
                KernelResponse::TimerId(id) => {
                    println!(
                        "Timer '{}' created (id: {}). Fires in {} seconds.",
                        name, id, delay
                    );
                }
                KernelResponse::Error { message } => {
                    anyhow::bail!("Failed to create timer: {}", message);
                }
                _ => anyhow::bail!("Unexpected response: {:?}", response),
            }
        }
        TimerCommands::List => {
            let response = client.send_command(KernelCommand::ListTimers).await?;
            match response {
                KernelResponse::TimerList(timers) => {
                    if timers.is_empty() {
                        println!("No pending timers.");
                        return Ok(());
                    }

                    println!(
                        "{:<20} {:<20} {:<15} {:<25}",
                        "NAME", "AGENT", "ACTION", "FIRES AT"
                    );
                    for timer in timers {
                        let action_type = match &timer.action {
                            agentos_types::schedule::TimerAction::NotifyUser { .. } => "notify",
                            agentos_types::schedule::TimerAction::RunTask { .. } => "run_task",
                            agentos_types::schedule::TimerAction::RunTaskAndNotify { .. } => {
                                "run+notify"
                            }
                        };
                        println!(
                            "{:<20} {:<20} {:<15} {:<25}",
                            timer.name,
                            timer.agent_name,
                            action_type,
                            timer.fire_at.format("%Y-%m-%d %H:%M:%S UTC"),
                        );
                    }
                }
                KernelResponse::Error { message } => {
                    anyhow::bail!("Failed to list timers: {}", message);
                }
                _ => anyhow::bail!("Unexpected response: {:?}", response),
            }
        }
        TimerCommands::Cancel { name } => {
            let response = client
                .send_command(KernelCommand::CancelTimer { name: name.clone() })
                .await?;
            match response {
                KernelResponse::Success { .. } => {
                    println!("Timer '{}' cancelled.", name);
                }
                KernelResponse::Error { message } => {
                    anyhow::bail!("Failed to cancel timer: {}", message);
                }
                _ => anyhow::bail!("Unexpected response: {:?}", response),
            }
        }
    }
    Ok(())
}
