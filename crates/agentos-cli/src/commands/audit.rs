use agentos_bus::client::BusClient;
use agentos_bus::message::{KernelCommand, KernelResponse};
use clap::Subcommand;

#[derive(Subcommand)]
pub enum AuditCommands {
    /// View recent audit log entries
    Logs {
        /// Number of recent entries to show
        #[arg(long, default_value = "50")]
        last: u32,
    },
    /// Verify the Merkle hash chain integrity
    Verify {
        /// Start verification from this sequence number (default: beginning)
        #[arg(long)]
        from: Option<i64>,
    },
    /// List context snapshots for a task
    Snapshots {
        /// Task ID to list snapshots for
        #[arg(long)]
        task: String,
    },
    /// Export the full audit chain as JSONL
    Export {
        /// Maximum number of entries to export
        #[arg(long)]
        limit: Option<u32>,
        /// Write to file instead of stdout
        #[arg(long)]
        output: Option<String>,
    },
    /// Roll back a task's context to a saved snapshot
    Rollback {
        /// Task ID to roll back
        #[arg(long)]
        task: String,
        /// Snapshot reference (e.g. snap_0001). Defaults to most recent.
        #[arg(long)]
        snapshot: Option<String>,
    },
}

pub async fn handle(client: &mut BusClient, command: AuditCommands) -> anyhow::Result<()> {
    match command {
        AuditCommands::Logs { last } => {
            let response = client
                .send_command(KernelCommand::GetAuditLogs { limit: last })
                .await?;
            match response {
                KernelResponse::AuditLogs(entries) => {
                    if entries.is_empty() {
                        println!("No audit entries.");
                    } else {
                        println!(
                            "{:<30} {:<25} {:<10} DETAILS",
                            "TIMESTAMP", "EVENT TYPE", "SEVERITY"
                        );
                        println!("{}", "-".repeat(100));
                        for entry in entries {
                            let details_str =
                                serde_json::to_string(&entry.details).unwrap_or_default();
                            println!(
                                "{:<30} {:<25} {:<10} {}",
                                entry.timestamp.to_rfc3339(),
                                format!("{:?}", entry.event_type),
                                format!("{:?}", entry.severity),
                                if details_str.chars().count() > 30 {
                                    format!(
                                        "{}...",
                                        details_str.chars().take(30).collect::<String>()
                                    )
                                } else {
                                    details_str
                                }
                            );
                        }
                    }
                }
                KernelResponse::Error { message } => eprintln!("❌ Error: {}", message),
                _ => eprintln!("❌ Unexpected response"),
            }
        }
        AuditCommands::Verify { from } => {
            let response = client
                .send_command(KernelCommand::VerifyAuditChain { from_seq: from })
                .await?;
            match response {
                KernelResponse::Success { data: Some(val) } => {
                    let valid = val.get("valid").and_then(|v| v.as_bool()).unwrap_or(false);
                    let checked = val
                        .get("entries_checked")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);

                    if valid {
                        println!("Audit chain VALID ({} entries verified)", checked);
                    } else {
                        let seq = val
                            .get("first_invalid_seq")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0);
                        let err = val
                            .get("error")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        eprintln!(
                            "Audit chain INVALID at seq {} ({} entries checked): {}",
                            seq, checked, err
                        );
                    }
                }
                KernelResponse::Error { message } => eprintln!("Error: {}", message),
                _ => eprintln!("Unexpected response"),
            }
        }
        AuditCommands::Export { limit, output } => {
            let response = client
                .send_command(KernelCommand::ExportAuditChain { limit })
                .await?;
            match response {
                KernelResponse::AuditChainExport(jsonl) => {
                    if let Some(path) = output {
                        std::fs::write(&path, &jsonl)?;
                        println!("Audit chain exported to '{}'.", path);
                    } else {
                        print!("{}", jsonl);
                    }
                }
                KernelResponse::Error { message } => eprintln!("Error: {}", message),
                _ => eprintln!("Unexpected response"),
            }
        }

        AuditCommands::Snapshots { task } => {
            use agentos_types::TaskID;
            let task_id: TaskID = task
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid task ID: {}", task))?;
            let response = client
                .send_command(KernelCommand::ListSnapshots { task_id })
                .await?;
            match response {
                KernelResponse::SnapshotList(snaps) => {
                    if snaps.is_empty() {
                        println!("No snapshots for task {}.", task);
                    } else {
                        println!("{:<15} {:<12} TAKEN", "SNAPSHOT", "SIZE");
                        println!("{}", "-".repeat(50));
                        for s in snaps {
                            let snap_ref = s
                                .get("snapshot_ref")
                                .and_then(|v| v.as_str())
                                .unwrap_or("-");
                            let size = s.get("size_bytes").and_then(|v| v.as_u64()).unwrap_or(0);
                            let ts = s
                                .get("created_at_unix")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);
                            println!("{:<15} {:<12} {}", snap_ref, size, ts);
                        }
                    }
                }
                KernelResponse::Error { message } => eprintln!("Error: {}", message),
                _ => eprintln!("Unexpected response"),
            }
        }
        AuditCommands::Rollback { task, snapshot } => {
            use agentos_types::TaskID;
            let task_id: TaskID = task
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid task ID: {}", task))?;
            let response = client
                .send_command(KernelCommand::RollbackTask {
                    task_id,
                    snapshot_ref: snapshot,
                })
                .await?;
            match response {
                KernelResponse::Success { data } => {
                    let snap = data
                        .as_ref()
                        .and_then(|d| d.get("snapshot_ref"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("latest");
                    println!("Task {} rolled back to snapshot '{}'.", task, snap);
                }
                KernelResponse::Error { message } => eprintln!("Error: {}", message),
                _ => eprintln!("Unexpected response"),
            }
        }
    }
    Ok(())
}
