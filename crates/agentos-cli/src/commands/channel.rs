use agentos_bus::{client::BusClient, KernelCommand, KernelResponse};
use agentos_types::ChannelKind;
use clap::Subcommand;

#[derive(Subcommand)]
pub enum ChannelCommands {
    /// Connect a bidirectional notification channel
    Connect {
        /// Channel kind: telegram, ntfy, email
        #[arg(long, short = 'k')]
        kind: String,

        /// Channel-specific external identifier (Telegram chat_id, ntfy topic, email address).
        /// Optional for Telegram — omit to auto-discover from the first /start message.
        #[arg(long, short = 'e')]
        external_id: Option<String>,

        /// Human-readable display name for this channel
        #[arg(long, short = 'd')]
        display_name: String,

        /// Vault key where the credential (bot token, password) is stored
        #[arg(long, short = 'c', default_value = "")]
        credential_key: String,

        /// ntfy reply-topic for inbound messages
        #[arg(long)]
        reply_topic: Option<String>,

        /// ntfy server URL (default: https://ntfy.sh)
        #[arg(long)]
        server_url: Option<String>,

        /// Public URL for Telegram webhook mode (e.g. "https://example.com").
        /// When set, Telegram pushes updates to this URL instead of long-polling.
        #[arg(long)]
        webhook_url: Option<String>,

        /// Default agent for inbound chat on this channel (Telegram `/agent` uses this).
        #[arg(long)]
        active_agent: Option<String>,
    },

    /// Set or clear the default agent for inbound channel chat
    SetAgent {
        /// Channel ID (from `channel list`)
        #[arg(long)]
        id: String,
        /// Agent name (omit or pass empty string to clear)
        #[arg(long)]
        agent: Option<String>,
    },

    /// Disconnect a registered channel
    Disconnect {
        /// Channel ID (from `channel list`)
        id: String,
    },

    /// List all registered channels
    List,

    /// Send a test notification to a channel
    Test {
        /// Channel ID (from `channel list`)
        id: String,
    },

    /// Manage the DM pairing allowlist (approve/revoke senders)
    Pair {
        #[command(subcommand)]
        command: PairCommands,
    },
}

#[derive(Subcommand)]
pub enum PairCommands {
    /// List approved senders and pending pairing requests
    List,
    /// Approve a pending 6-character pairing code
    Approve {
        /// The pairing code the sender received from the bot
        code: String,
    },
    /// Revoke an approved sender from a channel's allowlist
    Revoke {
        /// Channel ID the sender is approved on
        channel_id: String,
        /// External sender ID to remove
        sender_id: String,
    },
}

pub async fn handle(client: &mut BusClient, command: ChannelCommands) -> anyhow::Result<()> {
    match command {
        ChannelCommands::Connect {
            kind,
            external_id,
            display_name,
            credential_key,
            reply_topic,
            server_url,
            webhook_url,
            active_agent,
        } => {
            let channel_kind: ChannelKind = kind
                .parse()
                .map_err(|e| anyhow::anyhow!("Invalid channel kind '{kind}': {e}"))?;

            let resp = client
                .send_command(KernelCommand::ConnectChannel {
                    kind: channel_kind,
                    external_id,
                    display_name,
                    credential_key,
                    reply_topic,
                    server_url,
                    webhook_url,
                    active_agent_name: active_agent,
                })
                .await?;

            match resp {
                KernelResponse::Success { data } => {
                    if let Some(d) = data {
                        let id = d.get("channel_id").and_then(|v| v.as_str()).unwrap_or("?");
                        let name = d
                            .get("display_name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        let msg = d.get("message").and_then(|v| v.as_str()).unwrap_or("");
                        println!("Channel connected: {name} (id: {id})");
                        if !msg.is_empty() && msg != "Channel connected successfully" {
                            println!("{msg}");
                        }
                    } else {
                        println!("Channel connected.");
                    }
                }
                KernelResponse::Error { message } => anyhow::bail!("Error: {message}"),
                _ => anyhow::bail!("Unexpected response from kernel"),
            }
        }

        ChannelCommands::SetAgent { id, agent } => {
            let resp = client
                .send_command(KernelCommand::SetChannelActiveAgent {
                    channel_id: id.clone(),
                    agent_name: agent,
                })
                .await?;

            match resp {
                KernelResponse::Success { .. } => {
                    println!("Channel '{id}' default chat agent updated.");
                }
                KernelResponse::Error { message } => anyhow::bail!("Error: {message}"),
                _ => anyhow::bail!("Unexpected response from kernel"),
            }
        }

        ChannelCommands::Disconnect { id } => {
            let resp = client
                .send_command(KernelCommand::DisconnectChannel {
                    channel_id: id.clone(),
                })
                .await?;

            match resp {
                KernelResponse::Success { .. } => {
                    println!("Channel '{id}' disconnected.");
                }
                KernelResponse::Error { message } => anyhow::bail!("Error: {message}"),
                _ => anyhow::bail!("Unexpected response from kernel"),
            }
        }

        ChannelCommands::List => {
            let resp = client.send_command(KernelCommand::ListChannels).await?;

            match resp {
                KernelResponse::ChannelList(channels) => {
                    if channels.is_empty() {
                        println!("No channels connected.");
                        return Ok(());
                    }

                    println!(
                        "{:<36} {:<10} {:<20} {:<25} CONNECTED",
                        "CHANNEL ID", "KIND", "DISPLAY NAME", "EXTERNAL ID"
                    );
                    println!("{}", "-".repeat(100));

                    for ch in &channels {
                        let ext_display = if ch.external_id.is_empty() {
                            "(pending discovery)".to_string()
                        } else {
                            truncate(&ch.external_id, 25)
                        };
                        println!(
                            "{:<36} {:<10} {:<20} {:<25} {}",
                            ch.id,
                            ch.kind,
                            truncate(&ch.display_name, 20),
                            ext_display,
                            ch.connected_at.format("%Y-%m-%d %H:%M UTC"),
                        );
                    }
                }
                KernelResponse::Error { message } => anyhow::bail!("Error: {message}"),
                _ => anyhow::bail!("Unexpected response from kernel"),
            }
        }

        ChannelCommands::Test { id } => {
            let resp = client
                .send_command(KernelCommand::TestChannel {
                    channel_id: id.clone(),
                })
                .await?;

            match resp {
                KernelResponse::Success { .. } => {
                    println!("Test notification sent to channel '{id}'.");
                }
                KernelResponse::Error { message } => anyhow::bail!("Error: {message}"),
                _ => anyhow::bail!("Unexpected response from kernel"),
            }
        }

        ChannelCommands::Pair { command } => handle_pair(client, command).await?,
    }

    Ok(())
}

async fn handle_pair(client: &mut BusClient, command: PairCommands) -> anyhow::Result<()> {
    match command {
        PairCommands::List => {
            let resp = client.send_command(KernelCommand::ListPairings).await?;
            match resp {
                KernelResponse::PairingList { approved, pending } => {
                    if approved.is_empty() && pending.is_empty() {
                        println!("No approved senders or pending pairing requests.");
                        return Ok(());
                    }
                    if !approved.is_empty() {
                        println!("APPROVED SENDERS");
                        println!("{:<20} {:<28} APPROVED AT", "CHANNEL ID", "SENDER ID");
                        println!("{}", "-".repeat(70));
                        for s in &approved {
                            println!(
                                "{:<20} {:<28} {}",
                                truncate(&s.channel_id, 20),
                                truncate(&s.sender_id, 28),
                                s.approved_at,
                            );
                        }
                    }
                    if !pending.is_empty() {
                        if !approved.is_empty() {
                            println!();
                        }
                        println!("PENDING REQUESTS (approve with the code the sender received)");
                        println!("{:<20} {:<28} EXPIRES AT", "CHANNEL ID", "SENDER ID");
                        println!("{}", "-".repeat(70));
                        for p in &pending {
                            println!(
                                "{:<20} {:<28} {}",
                                truncate(&p.channel_id, 20),
                                truncate(&p.sender_id, 28),
                                p.expires_at,
                            );
                        }
                    }
                }
                KernelResponse::Error { message } => anyhow::bail!("Error: {message}"),
                _ => anyhow::bail!("Unexpected response from kernel"),
            }
        }

        PairCommands::Approve { code } => {
            let resp = client
                .send_command(KernelCommand::ApprovePairing { code })
                .await?;
            match resp {
                KernelResponse::Success { data } => {
                    let sender = data
                        .as_ref()
                        .and_then(|d| d.get("sender_id"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("sender");
                    println!("Pairing approved — '{sender}' is now allowlisted.");
                }
                KernelResponse::Error { message } => anyhow::bail!("Error: {message}"),
                _ => anyhow::bail!("Unexpected response from kernel"),
            }
        }

        PairCommands::Revoke {
            channel_id,
            sender_id,
        } => {
            let resp = client
                .send_command(KernelCommand::RevokePairing {
                    channel_id,
                    sender_id: sender_id.clone(),
                })
                .await?;
            match resp {
                KernelResponse::Success { .. } => {
                    println!("Pairing revoked for sender '{sender_id}'.");
                }
                KernelResponse::Error { message } => anyhow::bail!("Error: {message}"),
                _ => anyhow::bail!("Unexpected response from kernel"),
            }
        }
    }

    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    let mut chars = s.chars();
    let truncated: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}
