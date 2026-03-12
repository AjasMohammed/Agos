use agentos_bus::{client::BusClient, KernelCommand, KernelResponse};
use clap::Subcommand;

#[derive(Subcommand)]
pub enum EventCommands {
    /// Subscribe an agent to an event type
    Subscribe {
        /// Name of the agent to subscribe
        #[arg(long)]
        agent: String,

        /// Event filter: "all", "category:<name>", or exact event type like "AgentAdded"
        #[arg(long)]
        event: String,

        /// Throttle policy: "none", "once_per:<dur>", "max:<count>/<dur>" (e.g. "once_per:30s")
        #[arg(long)]
        throttle: Option<String>,

        /// Subscription priority: critical, high, normal, low
        #[arg(long, default_value = "normal")]
        priority: String,
    },

    /// Remove an event subscription
    Unsubscribe {
        /// Subscription ID to remove
        id: String,
    },

    /// Manage event subscriptions
    Subscriptions {
        #[command(subcommand)]
        command: SubscriptionSubcommands,
    },

    /// View recent event history
    History {
        /// Number of recent events to show
        #[arg(long, default_value = "20")]
        last: u32,
    },
}

#[derive(Subcommand)]
pub enum SubscriptionSubcommands {
    /// List all subscriptions (optionally filtered by agent)
    List {
        /// Filter by agent name
        #[arg(long)]
        agent: Option<String>,
    },

    /// Show details of a subscription
    Show {
        /// Subscription ID
        #[arg(long)]
        id: String,
    },

    /// Enable a subscription
    Enable {
        /// Subscription ID
        #[arg(long)]
        id: String,
    },

    /// Disable a subscription
    Disable {
        /// Subscription ID
        #[arg(long)]
        id: String,
    },
}

pub async fn handle(client: &mut BusClient, command: EventCommands) -> anyhow::Result<()> {
    match command {
        EventCommands::Subscribe {
            agent,
            event,
            throttle,
            priority,
        } => {
            let resp = client
                .send_command(KernelCommand::EventSubscribe {
                    agent_name: agent,
                    event_filter: event,
                    throttle,
                    priority: Some(priority),
                })
                .await?;

            match resp {
                KernelResponse::EventSubscriptionId(id) => {
                    println!("Subscription created: {}", id);
                }
                KernelResponse::Error { message } => {
                    anyhow::bail!("Error: {}", message);
                }
                _ => anyhow::bail!("Unexpected response from kernel"),
            }
        }

        EventCommands::Unsubscribe { id } => {
            let resp = client
                .send_command(KernelCommand::EventUnsubscribe {
                    subscription_id: id.clone(),
                })
                .await?;

            match resp {
                KernelResponse::Success { .. } => {
                    println!("Subscription {} removed.", id);
                }
                KernelResponse::Error { message } => {
                    anyhow::bail!("Error: {}", message);
                }
                _ => anyhow::bail!("Unexpected response from kernel"),
            }
        }

        EventCommands::Subscriptions { command: sub_cmd } => match sub_cmd {
            SubscriptionSubcommands::List { agent } => {
                let resp = client
                    .send_command(KernelCommand::EventListSubscriptions { agent_name: agent })
                    .await?;

                match resp {
                    KernelResponse::EventSubscriptionList(list) => {
                        if list.is_empty() {
                            println!("No event subscriptions.");
                            return Ok(());
                        }

                        println!(
                            "{:<38} {:<38} {:<20} {:<10} {:<8}",
                            "ID", "AGENT_ID", "FILTER", "PRIORITY", "ENABLED"
                        );
                        println!("{}", "-".repeat(114));

                        for entry in &list {
                            let id = entry.get("id").and_then(|v| v.as_str()).unwrap_or("-");
                            let agent_id = entry
                                .get("agent_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("-");
                            let filter = entry
                                .get("event_type_filter")
                                .and_then(|v| v.as_str())
                                .unwrap_or("-");
                            let priority = entry
                                .get("priority")
                                .and_then(|v| v.as_str())
                                .unwrap_or("-");
                            let enabled = entry
                                .get("enabled")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);

                            let short_id = &id[..id.len().min(36)];
                            let short_agent = &agent_id[..agent_id.len().min(36)];
                            let short_filter = if filter.len() > 18 {
                                format!("{}...", &filter[..18])
                            } else {
                                filter.to_string()
                            };

                            println!(
                                "{:<38} {:<38} {:<20} {:<10} {:<8}",
                                short_id,
                                short_agent,
                                short_filter,
                                priority,
                                if enabled { "yes" } else { "no" }
                            );
                        }
                    }
                    KernelResponse::Error { message } => {
                        anyhow::bail!("Error: {}", message);
                    }
                    _ => anyhow::bail!("Unexpected response from kernel"),
                }
            }

            SubscriptionSubcommands::Show { id } => {
                let resp = client
                    .send_command(KernelCommand::EventGetSubscription {
                        subscription_id: id.clone(),
                    })
                    .await?;

                match resp {
                    KernelResponse::Success { data: Some(sub) } => {
                        println!("Subscription {}", id);
                        println!("{}", "=".repeat(60));
                        println!(
                            "Agent ID:     {}",
                            sub.get("agent_id").and_then(|v| v.as_str()).unwrap_or("-")
                        );
                        println!(
                            "Filter:       {}",
                            sub.get("event_type_filter")
                                .and_then(|v| v.as_str())
                                .unwrap_or("-")
                        );
                        println!(
                            "Priority:     {}",
                            sub.get("priority").and_then(|v| v.as_str()).unwrap_or("-")
                        );
                        println!(
                            "Throttle:     {}",
                            sub.get("throttle").and_then(|v| v.as_str()).unwrap_or("-")
                        );
                        println!(
                            "Enabled:      {}",
                            sub.get("enabled")
                                .and_then(|v| v.as_bool())
                                .map(|b| if b { "yes" } else { "no" })
                                .unwrap_or("-")
                        );
                        println!(
                            "Created at:   {}",
                            sub.get("created_at")
                                .and_then(|v| v.as_str())
                                .unwrap_or("-")
                        );
                    }
                    KernelResponse::Error { message } => {
                        anyhow::bail!("Error: {}", message);
                    }
                    _ => anyhow::bail!("Unexpected response from kernel"),
                }
            }

            SubscriptionSubcommands::Enable { id } => {
                let resp = client
                    .send_command(KernelCommand::EventEnableSubscription {
                        subscription_id: id.clone(),
                    })
                    .await?;

                match resp {
                    KernelResponse::Success { .. } => {
                        println!("Subscription {} enabled.", id);
                    }
                    KernelResponse::Error { message } => {
                        anyhow::bail!("Error: {}", message);
                    }
                    _ => anyhow::bail!("Unexpected response from kernel"),
                }
            }

            SubscriptionSubcommands::Disable { id } => {
                let resp = client
                    .send_command(KernelCommand::EventDisableSubscription {
                        subscription_id: id.clone(),
                    })
                    .await?;

                match resp {
                    KernelResponse::Success { .. } => {
                        println!("Subscription {} disabled.", id);
                    }
                    KernelResponse::Error { message } => {
                        anyhow::bail!("Error: {}", message);
                    }
                    _ => anyhow::bail!("Unexpected response from kernel"),
                }
            }
        },

        EventCommands::History { last } => {
            let resp = client
                .send_command(KernelCommand::EventHistory { last })
                .await?;

            match resp {
                KernelResponse::EventHistoryList(events) => {
                    if events.is_empty() {
                        println!("No events recorded.");
                        return Ok(());
                    }

                    println!(
                        "{:<26} {:<30} {:<10} {:<6}",
                        "TIMESTAMP", "EVENT TYPE", "SEVERITY", "DEPTH"
                    );
                    println!("{}", "-".repeat(80));

                    for entry in &events {
                        let ts = entry
                            .get("timestamp")
                            .and_then(|v| v.as_str())
                            .unwrap_or("-");
                        let event_type = entry
                            .get("event_type")
                            .and_then(|v| v.as_str())
                            .unwrap_or("-");
                        let severity = entry
                            .get("severity")
                            .and_then(|v| v.as_str())
                            .unwrap_or("-");
                        let depth = entry
                            .get("chain_depth")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);

                        println!(
                            "{:<26} {:<30} {:<10} {:<6}",
                            ts, event_type, severity, depth
                        );
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
