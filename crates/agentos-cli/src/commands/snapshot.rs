use agentos_bus::client::BusClient;
use agentos_bus::message::{KernelCommand, KernelResponse};
use clap::Subcommand;

#[derive(Subcommand, Debug)]
pub enum SnapshotCommands {
    /// List snapshots for a task
    List {
        /// Task ID
        #[arg(long)]
        task: String,
    },

    /// Roll back a task to a specific snapshot (or the latest)
    Rollback {
        /// Task ID
        #[arg(long)]
        task: String,

        /// Snapshot reference (e.g. snap_0001). Defaults to the latest
        #[arg(long)]
        snapshot: Option<String>,
    },
}

pub async fn handle(client: &mut BusClient, cmd: SnapshotCommands) -> anyhow::Result<()> {
    match cmd {
        SnapshotCommands::List { task } => {
            let task_id: agentos_types::TaskID = task
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid task ID: {}", task))?;

            let resp = client
                .send_command(KernelCommand::ListSnapshots { task_id })
                .await?;

            match resp {
                KernelResponse::SnapshotList(entries) => {
                    if entries.is_empty() {
                        println!("No snapshots found for task {}", task);
                    } else {
                        println!(
                            "{:<40} {:<20} {:<12} {:<20}",
                            "SNAPSHOT_REF", "ACTION", "SIZE", "CREATED"
                        );
                        for entry in &entries {
                            let snap_ref = entry
                                .get("snapshot_ref")
                                .and_then(|v| v.as_str())
                                .unwrap_or("?");
                            let action = entry
                                .get("action_type")
                                .and_then(|v| v.as_str())
                                .unwrap_or("?");
                            let size = entry
                                .get("size_bytes")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);
                            let created = entry
                                .get("created_at_unix")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);
                            println!(
                                "{:<40} {:<20} {:<12} {:<20}",
                                snap_ref, action, size, created
                            );
                        }
                        println!("\nTotal: {} snapshot(s)", entries.len());
                    }
                }
                KernelResponse::Error { message } => {
                    eprintln!("Error: {}", message);
                }
                _ => {
                    eprintln!("Unexpected response");
                }
            }
            Ok(())
        }

        SnapshotCommands::Rollback { task, snapshot } => {
            let task_id: agentos_types::TaskID = task
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid task ID: {}", task))?;

            let resp = client
                .send_command(KernelCommand::RollbackTask {
                    task_id,
                    snapshot_ref: snapshot.clone(),
                })
                .await?;

            match resp {
                KernelResponse::Success { data } => {
                    let snap_ref = data
                        .as_ref()
                        .and_then(|d| d.get("snapshot_ref"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("latest");
                    println!("✅ Task {} rolled back to snapshot '{}'", task, snap_ref);
                }
                KernelResponse::Error { message } => {
                    eprintln!("Error: {}", message);
                }
                _ => {
                    eprintln!("Unexpected response");
                }
            }
            Ok(())
        }
    }
}
