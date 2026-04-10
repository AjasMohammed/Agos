use agentos_bus::client::BusClient;
use agentos_bus::message::{KernelCommand, KernelResponse};
use clap::Subcommand;

#[derive(Subcommand)]
pub enum PluginCommands {
    /// List all discovered plugins and their status
    List,
    /// Activate a plugin by ID
    Enable {
        /// Plugin ID (e.g. "discord", "memory-embeddings")
        plugin_id: String,
    },
    /// Deactivate a plugin by ID
    Disable {
        /// Plugin ID
        plugin_id: String,
    },
    /// Show full details for a specific plugin
    Info {
        /// Plugin ID
        plugin_id: String,
    },
}

pub async fn handle(client: &mut BusClient, command: PluginCommands) -> anyhow::Result<()> {
    match command {
        PluginCommands::List => {
            let response = client.send_command(KernelCommand::ListPlugins).await?;
            match response {
                KernelResponse::Success { data: Some(data) } => {
                    let plugins = data["plugins"].as_array().cloned().unwrap_or_default();

                    if plugins.is_empty() {
                        println!("No plugins discovered.");
                        println!("Place plugin.toml files in plugins/core/ or plugins/user/");
                        return Ok(());
                    }

                    println!(
                        "{:<20} {:<10} {:<12} DESCRIPTION",
                        "ID", "VERSION", "STATUS"
                    );
                    println!("{}", "-".repeat(70));
                    for p in &plugins {
                        println!(
                            "{:<20} {:<10} {:<12} {}",
                            p["id"].as_str().unwrap_or("-"),
                            p["version"].as_str().unwrap_or("-"),
                            p["status"].as_str().unwrap_or("-"),
                            p["description"].as_str().unwrap_or("-"),
                        );
                    }
                }
                KernelResponse::Error { message } => {
                    anyhow::bail!("Failed to list plugins: {}", message);
                }
                _ => anyhow::bail!("Unexpected response"),
            }
        }

        PluginCommands::Enable { plugin_id } => {
            let response = client
                .send_command(KernelCommand::EnablePlugin {
                    plugin_id: plugin_id.clone(),
                })
                .await?;
            match response {
                KernelResponse::Success { .. } => {
                    println!("Plugin '{}' enabled.", plugin_id);
                }
                KernelResponse::Error { message } => {
                    anyhow::bail!("{}", message);
                }
                _ => anyhow::bail!("Unexpected response"),
            }
        }

        PluginCommands::Disable { plugin_id } => {
            let response = client
                .send_command(KernelCommand::DisablePlugin {
                    plugin_id: plugin_id.clone(),
                })
                .await?;
            match response {
                KernelResponse::Success { .. } => {
                    println!("Plugin '{}' disabled.", plugin_id);
                }
                KernelResponse::Error { message } => {
                    anyhow::bail!("{}", message);
                }
                _ => anyhow::bail!("Unexpected response"),
            }
        }

        PluginCommands::Info { plugin_id } => {
            let response = client.send_command(KernelCommand::ListPlugins).await?;
            match response {
                KernelResponse::Success { data: Some(data) } => {
                    let plugin = data["plugins"]
                        .as_array()
                        .and_then(|arr| arr.iter().find(|p| p["id"].as_str() == Some(&plugin_id)))
                        .cloned();

                    match plugin {
                        Some(p) => {
                            println!("Plugin: {}", p["id"].as_str().unwrap_or("-"));
                            println!("  Name    : {}", p["display_name"].as_str().unwrap_or("-"));
                            println!("  Version : {}", p["version"].as_str().unwrap_or("-"));
                            println!("  Status  : {}", p["status"].as_str().unwrap_or("-"));
                            println!("  Trust   : {}", p["trust_tier"].as_str().unwrap_or("-"));
                            println!("  Desc    : {}", p["description"].as_str().unwrap_or("-"));
                            println!("  Path    : {}", p["path"].as_str().unwrap_or("-"));
                            if let Some(reason) = p["block_reason"].as_str() {
                                println!("  Blocked : {}", reason);
                            }
                        }
                        None => {
                            anyhow::bail!("Plugin '{}' not found", plugin_id);
                        }
                    }
                }
                KernelResponse::Error { message } => {
                    anyhow::bail!("{}", message);
                }
                _ => anyhow::bail!("Unexpected response"),
            }
        }
    }
    Ok(())
}
