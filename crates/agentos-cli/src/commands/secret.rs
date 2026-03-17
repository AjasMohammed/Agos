use agentos_bus::client::BusClient;
use agentos_bus::message::{KernelCommand, KernelResponse};
use agentos_types::SecretScope;
use clap::Subcommand;

#[derive(Subcommand)]
pub enum SecretCommands {
    /// Set a secret (value entered interactively — never in shell args)
    Set {
        /// Secret name (e.g. OPENAI_API_KEY)
        name: String,
        /// Scope: "global", "agent:<name>", or "tool:<name>"
        #[arg(long, default_value = "global")]
        scope: String,
    },
    /// List all secrets (metadata only — values never shown)
    List,
    /// Revoke (delete) a secret
    Revoke {
        /// Secret name
        name: String,
    },
    /// Rotate a secret (new value entered interactively)
    Rotate {
        /// Secret name
        name: String,
    },
    /// Emergency vault lockdown: revoke all proxy tokens and block new issuance
    Lockdown,
}

pub async fn handle(client: &mut BusClient, command: SecretCommands) -> anyhow::Result<()> {
    match command {
        SecretCommands::Set { name, scope } => {
            eprint!("Enter value for '{}' (input hidden): ", name);
            let value = rpassword::read_password()?;

            if value.is_empty() {
                anyhow::bail!("Secret value cannot be empty");
            }

            let parsed_scope = parse_scope(&scope)?;

            let response = client
                .send_command(KernelCommand::SetSecret {
                    name: name.clone(),
                    value,
                    scope: parsed_scope,
                    scope_raw: Some(scope),
                })
                .await?;

            match response {
                KernelResponse::Success { .. } => println!("✅ Secret '{}' stored securely", name),
                KernelResponse::Error { message } => eprintln!("❌ Error: {}", message),
                _ => eprintln!("❌ Unexpected response"),
            }
        }

        SecretCommands::List => {
            let response = client.send_command(KernelCommand::ListSecrets).await?;
            match response {
                KernelResponse::SecretList(secrets) => {
                    if secrets.is_empty() {
                        println!("No secrets stored.");
                    } else {
                        println!("{:<25} {:<20} LAST USED", "NAME", "SCOPE");
                        println!("{}", "-".repeat(65));
                        for s in secrets {
                            let scope_str = format!("{:?}", s.scope);
                            let last_used = s
                                .last_used_at
                                .map(|t: chrono::DateTime<chrono::Utc>| t.to_string())
                                .unwrap_or_else(|| "never".into());
                            println!("{:<25} {:<20} {}", s.name, scope_str, last_used);
                        }
                    }
                }
                KernelResponse::Error { message } => eprintln!("❌ Error: {}", message),
                _ => eprintln!("❌ Unexpected response"),
            }
        }

        SecretCommands::Revoke { name } => {
            let response = client
                .send_command(KernelCommand::RevokeSecret { name: name.clone() })
                .await?;
            match response {
                KernelResponse::Success { .. } => println!("✅ Secret '{}' revoked", name),
                KernelResponse::Error { message } => eprintln!("❌ Error: {}", message),
                _ => eprintln!("❌ Unexpected response"),
            }
        }

        SecretCommands::Lockdown => {
            let response = client.send_command(KernelCommand::VaultLockdown).await?;
            match response {
                KernelResponse::Success { data } => {
                    let msg = data
                        .as_ref()
                        .and_then(|d| d.get("message"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("Vault locked down");
                    println!("{}", msg);
                }
                KernelResponse::Error { message } => eprintln!("Error: {}", message),
                _ => eprintln!("Unexpected response"),
            }
        }

        SecretCommands::Rotate { name } => {
            eprint!("Enter new value for '{}' (input hidden): ", name);
            let new_value = rpassword::read_password()?;

            if new_value.is_empty() {
                anyhow::bail!("Secret value cannot be empty");
            }

            let response = client
                .send_command(KernelCommand::RotateSecret {
                    name: name.clone(),
                    new_value,
                })
                .await?;

            match response {
                KernelResponse::Success { .. } => println!("✅ Secret '{}' rotated", name),
                KernelResponse::Error { message } => eprintln!("❌ Error: {}", message),
                _ => eprintln!("❌ Unexpected response"),
            }
        }
    }
    Ok(())
}

fn parse_scope(s: &str) -> anyhow::Result<SecretScope> {
    // NOTE: agent: and tool: scopes are resolved server-side by the kernel using
    // the scope_raw field — which always accompanies the scope placeholder sent here.
    // This function validates the format and returns a client-side placeholder only.
    match s {
        "global" => Ok(SecretScope::Global),
        s if s.starts_with("agent:") => {
            let name = &s[6..];
            if name.is_empty() {
                anyhow::bail!("agent scope requires a name, e.g. 'agent:worker'");
            }
            Ok(SecretScope::Global) // placeholder; kernel resolves via scope_raw
        }
        s if s.starts_with("tool:") => {
            let name = &s[5..];
            if name.is_empty() {
                anyhow::bail!("tool scope requires a name, e.g. 'tool:file-reader'");
            }
            Ok(SecretScope::Global) // placeholder; kernel resolves via scope_raw
        }
        _ => anyhow::bail!(
            "Invalid scope: '{}'. Use 'global', 'agent:<name>', or 'tool:<name>'",
            s
        ),
    }
}
