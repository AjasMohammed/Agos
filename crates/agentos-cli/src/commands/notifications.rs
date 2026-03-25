use agentos_bus::{client::BusClient, KernelCommand, KernelResponse};
use agentos_types::{DeliveryChannel, NotificationID};
use clap::Subcommand;

#[derive(Subcommand)]
pub enum NotificationCommands {
    /// List notifications from the user inbox
    List {
        /// Show only unread notifications
        #[arg(long, short = 'u')]
        unread: bool,

        /// Maximum number of notifications to show
        #[arg(long, short = 'n', default_value_t = 50)]
        limit: u32,
    },

    /// Show a notification's full body and mark it as read
    Read {
        /// Notification ID
        id: String,
    },

    /// Respond to an interactive (Question) notification
    Respond {
        /// Notification ID
        id: String,

        /// Your response text
        #[arg(long, short = 'r')]
        response: String,
    },

    /// Poll for new notifications every 5 seconds (press Ctrl-C to stop)
    Watch,
}

pub async fn handle(client: &mut BusClient, command: NotificationCommands) -> anyhow::Result<()> {
    match command {
        NotificationCommands::List { unread, limit } => {
            let resp = client
                .send_command(KernelCommand::ListNotifications {
                    unread_only: unread,
                    limit,
                })
                .await?;

            match resp {
                KernelResponse::NotificationList(list) => {
                    if list.is_empty() {
                        if unread {
                            println!("No unread notifications.");
                        } else {
                            println!("No notifications.");
                        }
                        return Ok(());
                    }

                    println!(
                        "{:<12} {:<10} {:<8} {:<20} SUBJECT",
                        "ID", "PRIORITY", "READ", "FROM"
                    );
                    println!("{}", "-".repeat(80));

                    for msg in &list {
                        let id_short = {
                            let s = msg.id.to_string();
                            s[..s.len().min(8)].to_string()
                        };
                        let from = match &msg.from {
                            agentos_types::NotificationSource::Agent(id) => {
                                let s = id.to_string();
                                format!("agent:{}", &s[..s.len().min(8)])
                            }
                            agentos_types::NotificationSource::Kernel => "kernel".to_string(),
                            agentos_types::NotificationSource::System => "system".to_string(),
                        };
                        let read_str = if msg.read { "yes" } else { "no" };
                        let subject = {
                            let mut chars = msg.subject.chars();
                            let truncated: String = chars.by_ref().take(40).collect();
                            if chars.next().is_some() {
                                format!("{truncated}…")
                            } else {
                                truncated
                            }
                        };
                        println!(
                            "{:<12} {:<10} {:<8} {:<20} {}",
                            id_short,
                            msg.priority.to_string(),
                            read_str,
                            from,
                            subject
                        );
                    }
                }
                KernelResponse::Error { message } => anyhow::bail!("Error: {message}"),
                _ => anyhow::bail!("Unexpected response from kernel"),
            }
        }

        NotificationCommands::Read { id } => {
            let notification_id: NotificationID = id
                .parse()
                .map_err(|e| anyhow::anyhow!("Invalid notification ID: {e}"))?;

            let resp = client
                .send_command(KernelCommand::GetNotification { notification_id })
                .await?;

            match resp {
                KernelResponse::NotificationDetail(boxed) => match *boxed {
                    Some(msg) => {
                        let msg = &msg;

                        println!("ID:       {}", msg.id);
                        println!("Priority: {}", msg.priority);
                        println!("From:     {:?}", msg.from);
                        println!(
                            "Time:     {}",
                            msg.created_at.format("%Y-%m-%d %H:%M:%S UTC")
                        );
                        if let Some(task_id) = msg.task_id {
                            println!("Task:     {task_id}");
                        }
                        println!();
                        println!("Subject:  {}", msg.subject);
                        println!();
                        println!("{}", msg.body);

                        if let agentos_types::UserMessageKind::Question {
                            ref question,
                            ref options,
                            ..
                        } = msg.kind
                        {
                            println!();
                            println!("Question: {question}");
                            if let Some(opts) = options {
                                println!("Options:");
                                for opt in opts {
                                    println!("  - {opt}");
                                }
                            }
                            if let Some(ref resp) = msg.response {
                                println!();
                                println!("Response: {} (via {})", resp.text, resp.channel);
                            } else {
                                println!();
                                println!(
                                "Use `agentctl notifications respond {id} --response <text>` to reply."
                            );
                            }
                        }

                        // Mark as read.
                        let _ = client
                            .send_command(KernelCommand::MarkNotificationRead { notification_id })
                            .await?;
                    }
                    None => {
                        anyhow::bail!("Notification {id} not found")
                    }
                },
                KernelResponse::Error { message } => anyhow::bail!("Error: {message}"),
                _ => anyhow::bail!("Unexpected response from kernel"),
            }
        }

        NotificationCommands::Respond { id, response } => {
            let notification_id: NotificationID = id
                .parse()
                .map_err(|e| anyhow::anyhow!("Invalid notification ID: {e}"))?;

            let resp = client
                .send_command(KernelCommand::RespondToNotification {
                    notification_id,
                    response_text: response.clone(),
                    channel: DeliveryChannel::cli(),
                })
                .await?;

            match resp {
                KernelResponse::Success { .. } => {
                    println!("Response submitted for notification {id}.");
                }
                KernelResponse::Error { message } => anyhow::bail!("Error: {message}"),
                _ => anyhow::bail!("Unexpected response from kernel"),
            }
        }

        NotificationCommands::Watch => {
            println!("Watching for new notifications (Ctrl-C to stop)…");
            // Track IDs we've already shown so reads from another terminal
            // don't confuse the new-arrival detection.
            let mut seen_ids: std::collections::HashSet<NotificationID> =
                std::collections::HashSet::new();
            let mut first_poll = true;
            loop {
                let resp = client
                    .send_command(KernelCommand::ListNotifications {
                        unread_only: true,
                        limit: 100,
                    })
                    .await?;

                if let KernelResponse::NotificationList(list) = resp {
                    // Collect notifications not yet seen, oldest-first for display.
                    let mut new_msgs: Vec<&agentos_types::UserMessage> =
                        list.iter().filter(|m| !seen_ids.contains(&m.id)).collect();
                    new_msgs.reverse(); // list is newest-first; display oldest-first
                    for msg in new_msgs {
                        seen_ids.insert(msg.id);
                        if !first_poll {
                            println!(
                                "[{}] [{}] {} — {}",
                                msg.created_at.format("%H:%M:%S"),
                                msg.priority,
                                msg.subject,
                                msg.id
                            );
                        }
                    }
                    // On first poll, silently register all existing IDs so we
                    // don't flood the terminal with stale notifications.
                    if first_poll {
                        first_poll = false;
                        if seen_ids.is_empty() {
                            println!("No existing unread notifications. Waiting…");
                        } else {
                            println!(
                                "Skipping {} existing unread notification(s). Waiting for new ones…",
                                seen_ids.len()
                            );
                        }
                    }
                }

                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            }
        }
    }

    Ok(())
}
