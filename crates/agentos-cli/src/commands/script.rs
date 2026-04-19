use agentos_bus::client::BusClient;
use agentos_bus::message::{KernelCommand, KernelResponse};
use clap::Subcommand;

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[derive(Subcommand)]
pub enum ScriptCommands {
    /// List all script tools currently loaded from the scripts directory
    List,

    /// Force re-parse and re-register a script tool by name
    Reload {
        /// Script tool name (as declared in @agentos tool: annotation)
        name: String,
    },
}

pub async fn handle(client: &mut BusClient, command: ScriptCommands) -> anyhow::Result<()> {
    match command {
        ScriptCommands::List => {
            let response = client.send_command(KernelCommand::ListScripts).await?;
            match response {
                KernelResponse::ScriptList(scripts) => {
                    if scripts.is_empty() {
                        println!("No script tools loaded.");
                        println!(
                            "Drop annotated scripts into the scripts directory to register tools."
                        );
                    } else {
                        println!("{:<30} {:<12} PATH", "NAME", "VERSION");
                        println!("{}", "-".repeat(75));
                        for s in &scripts {
                            println!(
                                "{:<30} {:<12} {}",
                                truncate(&s.name, 30),
                                truncate(&s.version, 12),
                                s.path
                            );
                        }
                        println!("\n{} script tool(s) loaded.", scripts.len());
                    }
                }
                KernelResponse::Error { message } => {
                    eprintln!("Error: {}", message);
                }
                _ => eprintln!("Unexpected response"),
            }
        }

        ScriptCommands::Reload { name } => {
            let response = client
                .send_command(KernelCommand::ReloadScript { name: name.clone() })
                .await?;
            match response {
                KernelResponse::Success { .. } => {
                    println!("Script tool '{}' reloaded.", name);
                }
                KernelResponse::Error { message } => {
                    eprintln!("Error: {}", message);
                }
                _ => eprintln!("Unexpected response"),
            }
        }
    }
    Ok(())
}
