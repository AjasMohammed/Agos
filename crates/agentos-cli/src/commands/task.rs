use agentos_bus::client::BusClient;
use agentos_bus::message::{KernelCommand, KernelResponse};
use agentos_types::ids::{AgentID, TaskID};
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
    /// Show execution trace for a completed task
    Trace {
        /// Task ID
        task_id: String,
        /// Output as raw JSON instead of formatted text
        #[arg(long)]
        json: bool,
        /// Show only a specific iteration number (1-based)
        #[arg(long)]
        iter: Option<u32>,
    },
    /// List recent task execution traces
    Traces {
        /// Maximum number of traces to show
        #[arg(long, default_value_t = 20)]
        limit: u32,
        /// Filter traces by agent ID
        #[arg(long)]
        agent: Option<String>,
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
                if prompt.chars().count() > 80 {
                    format!("{}...", prompt.chars().take(80).collect::<String>())
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
                                if t.prompt_preview.chars().count() > 40 {
                                    format!(
                                        "{}...",
                                        t.prompt_preview.chars().take(40).collect::<String>()
                                    )
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

        TaskCommands::Trace {
            task_id,
            json,
            iter,
        } => {
            let tid = TaskID::from_uuid(Uuid::parse_str(&task_id)?);
            let response = client
                .send_command(KernelCommand::TaskGetTrace { task_id: tid })
                .await?;
            match response {
                KernelResponse::TaskTrace(trace) => {
                    if json {
                        println!("{}", serde_json::to_string_pretty(&trace)?);
                    } else {
                        print_trace(&trace, iter);
                    }
                }
                KernelResponse::Error { message } => eprintln!("❌ Error: {}", message),
                _ => eprintln!("❌ Unexpected response"),
            }
        }

        TaskCommands::Traces { limit, agent } => {
            let agent_id: Option<AgentID> = match agent {
                Some(ref s) => match s.parse() {
                    Ok(id) => Some(id),
                    Err(_) => {
                        eprintln!("Error: invalid agent ID '{s}'");
                        return Ok(());
                    }
                },
                None => None,
            };
            let response = client
                .send_command(KernelCommand::TaskListTraces { agent_id, limit })
                .await?;
            match response {
                KernelResponse::TaskTraces(summaries) => {
                    if summaries.is_empty() {
                        println!("No task traces found.");
                    } else {
                        println!(
                            "{:<38} {:<10} {:<6} {:<8} {:<10} PROMPT",
                            "TASK ID", "STATUS", "ITERS", "TOOLS", "TOKENS"
                        );
                        println!("{}", "-".repeat(95));
                        for s in summaries {
                            println!(
                                "{:<38} {:<10} {:<6} {:<8} {:<10} {}",
                                s.task_id,
                                s.status,
                                s.iteration_count,
                                s.tool_call_count,
                                s.total_tokens,
                                if s.prompt_preview.chars().count() > 35 {
                                    format!(
                                        "{}...",
                                        s.prompt_preview.chars().take(35).collect::<String>()
                                    )
                                } else {
                                    s.prompt_preview
                                }
                            );
                        }
                    }
                }
                KernelResponse::Error { message } => eprintln!("❌ Error: {}", message),
                _ => eprintln!("❌ Unexpected response"),
            }
        }
    }
    Ok(())
}

fn print_trace(trace: &agentos_types::TaskTrace, only_iter: Option<u32>) {
    println!("Task:    {}", trace.task_id);
    println!("Agent:   {}", trace.agent_id);
    println!("Status:  {}", trace.status);
    println!(
        "Started: {}",
        trace.started_at.format("%Y-%m-%d %H:%M:%S UTC")
    );
    if let Some(fin) = trace.finished_at {
        let elapsed = fin - trace.started_at;
        println!(
            "Elapsed: {:.1}s",
            elapsed.num_milliseconds() as f64 / 1000.0
        );
    }
    println!(
        "Tokens:  {} in + {} out = {}",
        trace.total_input_tokens,
        trace.total_output_tokens,
        trace.total_input_tokens + trace.total_output_tokens
    );
    if trace.total_cost_usd > 0.0 {
        println!("Cost:    ${:.6}", trace.total_cost_usd);
    }
    println!("Prompt:  {}", trace.prompt_preview);
    println!();

    let iters_to_show: Vec<_> = trace
        .iterations
        .iter()
        .filter(|it| only_iter.is_none_or(|n| it.iteration == n))
        .collect();

    if iters_to_show.is_empty() {
        if let Some(n) = only_iter {
            println!(
                "No iteration {} found (task has {} iterations).",
                n,
                trace.iterations.len()
            );
        } else {
            println!("No iterations recorded.");
        }
        return;
    }

    for it in iters_to_show {
        println!(
            "┌─ Iteration {} ─ {} ─ stop: {} ─ {}in/{}out",
            it.iteration, it.model, it.stop_reason, it.input_tokens, it.output_tokens
        );
        if it.tool_calls.is_empty() {
            println!("│  (no tool calls)");
        }
        for (i, tc) in it.tool_calls.iter().enumerate() {
            let last = i + 1 == it.tool_calls.len();
            let prefix = if last { "└──" } else { "├──" };
            let status = if !tc.permission_check.granted {
                format!(
                    "DENIED: {}",
                    tc.permission_check
                        .deny_reason
                        .as_deref()
                        .unwrap_or("unknown")
                )
            } else if tc.error.is_some() {
                format!("ERROR: {}", tc.error.as_deref().unwrap_or(""))
            } else {
                format!("ok ({}ms)", tc.duration_ms)
            };
            let inj = tc
                .injection_score
                .map(|s| format!(" [inj:{:.2}]", s))
                .unwrap_or_default();
            println!("│ {} {} — {}{}", prefix, tc.tool_name, status, inj);
        }
        println!("└{}", "─".repeat(70));
    }
}
