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

        /// Channel-specific external identifier (Telegram chat_id, ntfy topic, email address)
        #[arg(long, short = 'e')]
        external_id: String,

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
                })
                .await?;

            match resp {
                KernelResponse::Success { data } => {
                    if let Some(d) = data {
                        if let (Some(id), Some(name)) = (
                            d.get("channel_id").and_then(|v| v.as_str()),
                            d.get("display_name").and_then(|v| v.as_str()),
                        ) {
                            println!("Channel connected: {name} (id: {id})");
                        } else {
                            println!("Channel connected.");
                        }
                    } else {
                        println!("Channel connected.");
                    }
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
                        println!(
                            "{:<36} {:<10} {:<20} {:<25} {}",
                            ch.id,
                            ch.kind,
                            truncate(&ch.display_name, 20),
                            truncate(&ch.external_id, 25),
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
