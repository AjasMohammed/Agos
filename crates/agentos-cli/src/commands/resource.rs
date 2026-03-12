use agentos_bus::client::BusClient;
use agentos_bus::message::{KernelCommand, KernelResponse};
use clap::Subcommand;

#[derive(Subcommand)]
pub enum ResourceCommands {
    /// List all currently held resource locks
    List,

    /// Forcibly release a specific resource lock held by an agent
    Release {
        /// Resource ID to release
        #[arg(long)]
        resource: String,
        /// Agent name that holds the lock
        #[arg(long)]
        agent: String,
    },

    /// Show resource contention statistics (waiters, blocked agents)
    Contention,

    /// Release all resource locks held by an agent
    ReleaseAll {
        /// Agent name whose locks should be released
        #[arg(long)]
        agent: String,
    },
}

pub async fn handle(client: &mut BusClient, cmd: ResourceCommands) -> anyhow::Result<()> {
    match cmd {
        ResourceCommands::List => {
            let response = client
                .send_command(KernelCommand::ListResourceLocks)
                .await?;

            match response {
                KernelResponse::ResourceLockList(locks) => {
                    if locks.is_empty() {
                        println!("No resources currently locked.");
                        return Ok(());
                    }

                    println!(
                        "{:<30} {:<10} {:<20} {:<10}",
                        "Resource", "Mode", "Held By", "TTL(s)"
                    );
                    println!("{}", "-".repeat(74));

                    for lock in &locks {
                        println!(
                            "{:<30} {:<10} {:<20} {:<10}",
                            lock.get("resource_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("-"),
                            lock.get("lock_mode")
                                .and_then(|v| v.as_str())
                                .unwrap_or("-"),
                            lock.get("held_by").and_then(|v| v.as_str()).unwrap_or("-"),
                            lock.get("ttl_seconds")
                                .and_then(|v| v.as_u64())
                                .map(|n| n.to_string())
                                .unwrap_or_else(|| "-".to_string()),
                        );
                    }
                }
                KernelResponse::Success { data } => {
                    if let Some(d) = data {
                        println!("{}", serde_json::to_string_pretty(&d)?);
                    } else {
                        println!("No resources currently locked.");
                    }
                }
                KernelResponse::Error { message } => {
                    eprintln!("Error: {}", message);
                }
                _ => {
                    eprintln!("Unexpected response");
                }
            }
        }

        ResourceCommands::Contention => {
            let response = client
                .send_command(KernelCommand::ResourceContention)
                .await?;
            match response {
                KernelResponse::ResourceContentionStats(stats) => {
                    let contended = stats
                        .get("contended_resources")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    if contended == 0 {
                        println!("No resource contention.");
                        return Ok(());
                    }
                    println!("Contended resources: {}", contended);
                    println!();
                    if let Some(resources) = stats.get("resources").and_then(|v| v.as_array()) {
                        println!(
                            "{:<30} {:<10} {:<10} Holders",
                            "Resource", "Mode", "Waiters"
                        );
                        println!("{}", "-".repeat(70));
                        for r in resources {
                            println!(
                                "{:<30} {:<10} {:<10} {}",
                                r.get("resource_id").and_then(|v| v.as_str()).unwrap_or("-"),
                                r.get("lock_mode").and_then(|v| v.as_str()).unwrap_or("-"),
                                r.get("waiter_count").and_then(|v| v.as_u64()).unwrap_or(0),
                                r.get("holders")
                                    .and_then(|v| v.as_array())
                                    .map(|a| a.len().to_string())
                                    .unwrap_or_else(|| "0".to_string()),
                            );
                        }
                    }
                }
                KernelResponse::Error { message } => eprintln!("Error: {}", message),
                _ => eprintln!("Unexpected response"),
            }
        }

        ResourceCommands::Release { resource, agent } => {
            let response = client
                .send_command(KernelCommand::ReleaseResourceLock {
                    resource_id: resource.clone(),
                    agent_name: agent.clone(),
                })
                .await?;

            match response {
                KernelResponse::Success { data } => {
                    if let Some(d) = data {
                        if let Some(err) = d.get("error").and_then(|v| v.as_str()) {
                            eprintln!("Error: {}", err);
                        } else {
                            println!("Released lock on '{}' held by '{}'.", resource, agent);
                        }
                    } else {
                        println!("Released lock on '{}' held by '{}'.", resource, agent);
                    }
                }
                KernelResponse::Error { message } => {
                    eprintln!("Error: {}", message);
                }
                _ => {
                    eprintln!("Unexpected response");
                }
            }
        }

        ResourceCommands::ReleaseAll { agent } => {
            let response = client
                .send_command(KernelCommand::ReleaseAllResourceLocks {
                    agent_name: agent.clone(),
                })
                .await?;

            match response {
                KernelResponse::Success { data } => {
                    if let Some(d) = data {
                        if let Some(err) = d.get("error").and_then(|v| v.as_str()) {
                            eprintln!("Error: {}", err);
                        } else {
                            println!("Released all locks held by '{}'.", agent);
                        }
                    } else {
                        println!("Released all locks held by '{}'.", agent);
                    }
                }
                KernelResponse::Error { message } => {
                    eprintln!("Error: {}", message);
                }
                _ => {
                    eprintln!("Unexpected response");
                }
            }
        }
    }

    Ok(())
}
