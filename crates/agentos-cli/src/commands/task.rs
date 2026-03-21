use agentos_bus::client::BusClient;
use agentos_bus::message::{KernelCommand, KernelResponse};
use agentos_types::ids::TaskID;
use clap::Subcommand;
use uuid::Uuid;

#[derive(Subcommand)]
pub enum TaskCommands {
    /// Run a task
    Run {
        /// Agent to assign the task to (if left empty, auto-routing is used)
        #[arg(long)]
        agent: Option<String>,
        /// Run without iteration or timeout limits.
        /// Use for long-running autonomous workflows that must run to natural completion.
        #[arg(long, default_value_t = false)]
        autonomous: bool,
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

pub async fn handle(client: &mut BusClient, command: TaskCommands) -> anyhow::Result<()> {
    match command {
        TaskCommands::Run {
            agent,
            autonomous,
            prompt,
        } => {
            if let Some(ref a) = agent {
                println!("📝 Submitting task to agent '{}'...", a);
            } else {
                println!("🧠 Auto-routing task to best available agent...");
            }
            if autonomous {
                println!("   Mode: autonomous (no iteration/timeout limits)");
            }

            println!(
                "   Prompt: {}",
                if prompt.len() > 80 {
                    format!("{}...", &prompt[..80])
                } else {
                    prompt.clone()
                }
            );

            let response = client
                .send_command(KernelCommand::RunTask {
                    agent_name: agent,
                    prompt,
                    autonomous,
                })
                .await?;

            match response {
                KernelResponse::Success { data } => {
                    if let Some(data) = data {
                        let is_paused = data
                            .get("status")
                            .and_then(|v: &serde_json::Value| v.as_str())
                            == Some("paused");
                        if is_paused {
                            let task_id = data
                                .get("task_id")
                                .and_then(|v: &serde_json::Value| v.as_str())
                                .unwrap_or("<unknown-task>");
                            let reason = data
                                .get("reason")
                                .and_then(|v: &serde_json::Value| v.as_str())
                                .unwrap_or("No reason provided");
                            println!("\n⏸️ Task paused: {}", task_id);
                            println!("   Reason: {}", reason);
                            return Ok(());
                        }
                        println!("\n✅ Task completed:\n");
                        if let Some(result) = data
                            .get("result")
                            .and_then(|v: &serde_json::Value| v.as_str())
                        {
                            println!("{}", result);
                        } else {
                            println!("{}", serde_json::to_string_pretty(&data)?);
                        }
                    } else {
                        println!("\n✅ Task started successfully (async).");
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
                        println!("{:<38} {:<10} {:<15} PROMPT", "TASK ID", "STATE", "AGENT");
                        println!("{}", "-".repeat(90));
                        for t in tasks {
                            println!(
                                "{:<38} {:<10} {:<15} {}",
                                t.id,
                                format!("{:?}", t.state),
                                t.agent_id,
                                if t.prompt_preview.len() > 40 {
                                    format!("{}...", &t.prompt_preview[..40])
                                } else {
                                    t.prompt_preview
                                }
                            );
                        }
                    }
                }
                KernelResponse::Error { message } => eprintln!("❌ Error: {}", message),
                _ => eprintln!("❌ Unexpected response"),
            }
        }

        TaskCommands::Logs { task_id } => {
            let tid = TaskID::from_uuid(Uuid::parse_str(&task_id)?);
            let response = client
                .send_command(KernelCommand::GetTaskLogs { task_id: tid })
                .await?;
            match response {
                KernelResponse::TaskLogs(logs) => {
                    for line in logs {
                        println!("{}", line);
                    }
                }
                KernelResponse::Error { message } => eprintln!("❌ Error: {}", message),
                _ => eprintln!("❌ Unexpected response"),
            }
        }

        TaskCommands::Cancel { task_id } => {
            let tid = TaskID::from_uuid(Uuid::parse_str(&task_id)?);
            let response = client
                .send_command(KernelCommand::CancelTask { task_id: tid })
                .await?;
            match response {
                KernelResponse::Success { .. } => println!("✅ Task {} cancelled", task_id),
                KernelResponse::Error { message } => eprintln!("❌ Error: {}", message),
                _ => eprintln!("❌ Unexpected response"),
            }
        }
    }
    Ok(())
}
