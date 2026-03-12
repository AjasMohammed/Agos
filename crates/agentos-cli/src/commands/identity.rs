use agentos_bus::client::BusClient;
use agentos_bus::message::{KernelCommand, KernelResponse};
use clap::Subcommand;

#[derive(Subcommand)]
pub enum IdentityCommands {
    /// Show an agent's Ed25519 cryptographic identity
    Show {
        /// Agent name
        #[arg(long)]
        agent: String,
    },
    /// Revoke an agent's cryptographic identity and permissions
    Revoke {
        /// Agent name
        #[arg(long)]
        agent: String,
    },
}

pub async fn handle(client: &mut BusClient, cmd: IdentityCommands) -> anyhow::Result<()> {
    match cmd {
        IdentityCommands::Show { agent } => {
            let response = client
                .send_command(KernelCommand::IdentityShow { agent_name: agent })
                .await?;

            match response {
                KernelResponse::Success { data: Some(val) } => {
                    let agent_name = val
                        .get("agent_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("-");
                    let agent_id = val.get("agent_id").and_then(|v| v.as_str()).unwrap_or("-");
                    let pubkey = val
                        .get("public_key")
                        .and_then(|v| v.as_str())
                        .unwrap_or("none");
                    let has_key = val
                        .get("has_signing_key")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);

                    println!("Agent:       {}", agent_name);
                    println!("ID:          {}", agent_id);
                    println!("Public Key:  {}", pubkey);
                    println!(
                        "Signing Key: {}",
                        if has_key { "present" } else { "absent" }
                    );
                }
                KernelResponse::Error { message } => eprintln!("Error: {}", message),
                _ => eprintln!("Unexpected response"),
            }
        }

        IdentityCommands::Revoke { agent } => {
            let response = client
                .send_command(KernelCommand::IdentityRevoke {
                    agent_name: agent.clone(),
                })
                .await?;

            match response {
                KernelResponse::Success { .. } => {
                    println!("Identity and permissions revoked for agent '{}'.", agent);
                }
                KernelResponse::Error { message } => eprintln!("Error: {}", message),
                _ => eprintln!("Unexpected response"),
            }
        }
    }
    Ok(())
}
