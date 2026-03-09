use agentos_bus::client::BusClient;
use agentos_bus::message::{KernelCommand, KernelResponse};
use clap::Subcommand;

#[derive(Subcommand)]
pub enum ToolCommands {
    /// List installed tools
    List,
    /// Install a tool from its manifest file
    Install {
        /// Path to the tool manifest (.toml)
        path: String,
    },
    /// Remove an installed tool
    Remove {
        /// Tool name to remove
        name: String,
    },
}

pub async fn handle(client: &mut BusClient, command: ToolCommands) -> anyhow::Result<()> {
    match command {
        ToolCommands::List => {
            let response = client.send_command(KernelCommand::ListTools).await?;
            match response {
                KernelResponse::ToolList(tools) => {
                    if tools.is_empty() {
                        println!("No tools installed.");
                    } else {
                        println!("{:<20} {:<15} {}", "NAME", "VERSION", "DESCRIPTION");
                        println!("{}", "-".repeat(60));
                        for t in tools {
                            let description = if t.manifest.description.len() > 25 {
                                format!("{}...", &t.manifest.description[..25])
                            } else {
                                t.manifest.description.clone()
                            };
                            println!(
                                "{:<20} {:<15} {}",
                                t.manifest.name, t.manifest.version, description
                            );
                        }
                    }
                }
                KernelResponse::Error { message } => eprintln!("❌ Error: {}", message),
                _ => eprintln!("❌ Unexpected response"),
            }
        }
        ToolCommands::Install { path } => {
            let response = client
                .send_command(KernelCommand::InstallTool {
                    manifest_path: path.clone(),
                })
                .await?;
            match response {
                KernelResponse::Success { .. } => println!("✅ Tool from {} installed", path),
                KernelResponse::Error { message } => eprintln!("❌ Error: {}", message),
                _ => eprintln!("❌ Unexpected response"),
            }
        }
        ToolCommands::Remove { name } => {
            let response = client
                .send_command(KernelCommand::RemoveTool {
                    tool_name: name.clone(),
                })
                .await?;
            match response {
                KernelResponse::Success { .. } => println!("✅ Tool '{}' removed", name),
                KernelResponse::Error { message } => eprintln!("❌ Error: {}", message),
                _ => eprintln!("❌ Unexpected response"),
            }
        }
    }
    Ok(())
}
