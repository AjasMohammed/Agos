use agentos_bus::client::BusClient;
use agentos_bus::message::{KernelCommand, KernelResponse};
use clap::Subcommand;

#[derive(Subcommand)]
pub enum ProviderCommands {
    /// List all available LLM providers (built-in + catalog)
    List,
    /// Override the base URL for a catalog provider (persisted to providers.toml)
    SetUrl {
        /// Provider name (e.g. lmstudio, groq)
        name: String,
        /// New base URL (e.g. http://localhost:5678/v1)
        url: String,
    },
}

pub async fn handle(client: &mut BusClient, command: ProviderCommands) -> anyhow::Result<()> {
    match command {
        ProviderCommands::SetUrl { name, url } => {
            let response = client
                .send_command(KernelCommand::SetProviderUrl {
                    name: name.clone(),
                    url: url.clone(),
                })
                .await?;
            match response {
                KernelResponse::Success { .. } => {
                    println!("Provider '{}' base URL updated to '{}'", name, url);
                }
                KernelResponse::Error { message } => eprintln!("Error: {}", message),
                _ => eprintln!("Unexpected response"),
            }
        }
        ProviderCommands::List => {
            let response = client.send_command(KernelCommand::ListProviders).await?;
            match response {
                KernelResponse::ProviderList(providers) => {
                    if providers.is_empty() {
                        println!("No providers available.");
                        return Ok(());
                    }
                    println!(
                        "{:<15} {:<20} {:<10} {:<30} {:<5}",
                        "NAME", "DISPLAY NAME", "SOURCE", "DEFAULT MODEL", "KEY"
                    );
                    println!("{}", "-".repeat(80));
                    for p in &providers {
                        let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("-");
                        let display_name = p
                            .get("display_name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("-");
                        let source = p.get("source").and_then(|v| v.as_str()).unwrap_or("-");
                        let default_model = p
                            .get("default_model")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let api_key_env =
                            p.get("api_key_env").and_then(|v| v.as_str()).unwrap_or("");
                        let key_set = p
                            .get("api_key_set")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);

                        let key_indicator = if api_key_env.is_empty() {
                            "-".to_string() // No key needed
                        } else if key_set {
                            "yes".to_string()
                        } else {
                            "no".to_string()
                        };

                        println!(
                            "{:<15} {:<20} {:<10} {:<30} {:<5}",
                            name, display_name, source, default_model, key_indicator
                        );
                    }
                }
                KernelResponse::Error { message } => eprintln!("Error: {}", message),
                _ => eprintln!("Unexpected response"),
            }
        }
    }
    Ok(())
}
