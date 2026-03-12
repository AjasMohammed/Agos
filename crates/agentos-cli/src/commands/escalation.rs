use agentos_bus::{client::BusClient, KernelCommand, KernelResponse};
use clap::Subcommand;

#[derive(Subcommand)]
pub enum EscalationCommands {
    /// List pending escalations awaiting human review
    List {
        /// Show all escalations including resolved ones
        #[arg(long)]
        all: bool,
    },

    /// Show details of a specific escalation
    Get {
        /// Escalation ID
        id: u64,
    },

    /// Resolve an escalation with a decision
    Resolve {
        /// Escalation ID to resolve
        id: u64,

        /// Decision string (e.g. "Approved", "Denied", "Acknowledged")
        #[arg(long, short)]
        decision: String,
    },
}

pub async fn handle(client: &mut BusClient, command: EscalationCommands) -> anyhow::Result<()> {
    match command {
        EscalationCommands::List { all } => {
            let resp = client
                .send_command(KernelCommand::ListEscalations { pending_only: !all })
                .await?;

            match resp {
                KernelResponse::EscalationList(list) => {
                    if list.is_empty() {
                        if all {
                            println!("No escalations recorded.");
                        } else {
                            println!("No pending escalations.");
                        }
                        return Ok(());
                    }

                    println!(
                        "{:<6} {:<12} {:<10} {:<10} {:<8} SUMMARY",
                        "ID", "TASK", "URGENCY", "BLOCKING", "STATUS"
                    );
                    println!("{}", "-".repeat(80));

                    for entry in &list {
                        let id = entry.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
                        let task_id = entry.get("task_id").and_then(|v| v.as_str()).unwrap_or("-");
                        let urgency = entry.get("urgency").and_then(|v| v.as_str()).unwrap_or("-");
                        let blocking = entry
                            .get("blocking")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let resolved = entry
                            .get("resolved")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let summary = entry
                            .get("context_summary")
                            .and_then(|v| v.as_str())
                            .unwrap_or("-");
                        let short_task = &task_id[..task_id.len().min(8)];
                        let status = if resolved { "resolved" } else { "pending" };
                        let summary_short = if summary.len() > 40 {
                            format!("{}...", &summary[..40])
                        } else {
                            summary.to_string()
                        };

                        println!(
                            "{:<6} {:<12} {:<10} {:<10} {:<8} {}",
                            id,
                            short_task,
                            urgency,
                            if blocking { "yes" } else { "no" },
                            status,
                            summary_short
                        );
                    }
                }
                KernelResponse::Error { message } => {
                    anyhow::bail!("Error: {}", message);
                }
                _ => anyhow::bail!("Unexpected response from kernel"),
            }
        }

        EscalationCommands::Get { id } => {
            let resp = client
                .send_command(KernelCommand::GetEscalation { id })
                .await?;

            match resp {
                KernelResponse::Success { data: Some(entry) } => {
                    println!("Escalation #{}", id);
                    println!("{}", "=".repeat(60));
                    println!(
                        "Task ID:      {}",
                        entry.get("task_id").and_then(|v| v.as_str()).unwrap_or("-")
                    );
                    println!(
                        "Agent ID:     {}",
                        entry
                            .get("agent_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("-")
                    );
                    println!(
                        "Reason:       {}",
                        entry.get("reason").and_then(|v| v.as_str()).unwrap_or("-")
                    );
                    println!(
                        "Urgency:      {}",
                        entry.get("urgency").and_then(|v| v.as_str()).unwrap_or("-")
                    );
                    println!(
                        "Blocking:     {}",
                        entry
                            .get("blocking")
                            .and_then(|v| v.as_bool())
                            .map(|b| if b { "yes" } else { "no" })
                            .unwrap_or("-")
                    );
                    println!(
                        "Status:       {}",
                        if entry
                            .get("resolved")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false)
                        {
                            "resolved"
                        } else {
                            "pending"
                        }
                    );
                    println!();
                    println!(
                        "Summary:\n  {}",
                        entry
                            .get("context_summary")
                            .and_then(|v| v.as_str())
                            .unwrap_or("-")
                    );
                    println!();
                    println!(
                        "Decision point:\n  {}",
                        entry
                            .get("decision_point")
                            .and_then(|v| v.as_str())
                            .unwrap_or("-")
                    );

                    if let Some(options) = entry.get("options").and_then(|v| v.as_array()) {
                        println!();
                        println!("Options:");
                        for opt in options {
                            if let Some(s) = opt.as_str() {
                                println!("  - {}", s);
                            }
                        }
                    }

                    if let Some(resolution) = entry.get("resolution").and_then(|v| v.as_str()) {
                        println!();
                        println!("Resolution: {}", resolution);
                    }
                }
                KernelResponse::Error { message } => {
                    anyhow::bail!("Error: {}", message);
                }
                _ => anyhow::bail!("Unexpected response from kernel"),
            }
        }

        EscalationCommands::Resolve { id, decision } => {
            let resp = client
                .send_command(KernelCommand::ResolveEscalation {
                    id,
                    decision: decision.clone(),
                })
                .await?;

            match resp {
                KernelResponse::Success { data } => {
                    println!("Escalation #{} resolved: {}", id, decision);
                    if let Some(data) = data {
                        if let Some(true) = data.get("task_resumed").and_then(|v| v.as_bool()) {
                            let task_id = data
                                .get("task_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown");
                            println!("Task {} resumed.", task_id);
                        }
                    }
                }
                KernelResponse::Error { message } => {
                    anyhow::bail!("Error: {}", message);
                }
                _ => anyhow::bail!("Unexpected response from kernel"),
            }
        }
    }

    Ok(())
}
