use agentos_bus::client::BusClient;
use agentos_bus::message::{KernelCommand, KernelResponse};
use agentos_types::SecretScope;
use clap::Subcommand;

#[derive(Subcommand)]
pub enum SecretCommands {
    /// Set a secret. Value is prompted interactively, or supplied via `--value`
    /// / piped on stdin for non-interactive (CI / scripted) use.
    Set {
        /// Secret name (e.g. OPENAI_API_KEY)
        name: String,
        /// Scope: "agent:<name>", "tool:<name>", or "global". REQUIRED — there
        /// is deliberately no default, because "global" makes the secret
        /// readable by EVERY agent and tool on this host. Prefer the narrowest
        /// scope that works (e.g. --scope agent:worker).
        #[arg(long)]
        scope: String,
        /// Secret value. If omitted: prompts on a TTY, else reads one line from
        /// stdin. NOTE: passing on the command line exposes it in the process
        /// list — prefer the stdin form
        /// (`… | agentos secret set NAME --scope agent:<name>`) in CI.
        #[arg(long)]
        value: Option<String>,
    },
    /// List all secrets (metadata only — values never shown)
    List,
    /// Revoke (delete) a secret
    Revoke {
        /// Secret name
        name: String,
    },
    /// Rotate a secret (new value prompted, or via `--value` / stdin)
    Rotate {
        /// Secret name
        name: String,
        /// New value. If omitted: prompts on a TTY, else reads one line from
        /// stdin. Prefer the stdin form in CI (command-line args are visible).
        #[arg(long)]
        value: Option<String>,
    },
    /// Emergency vault lockdown: revoke all proxy tokens and block new issuance
    Lockdown,
}

pub async fn handle(client: &mut BusClient, command: SecretCommands) -> anyhow::Result<()> {
    match command {
        SecretCommands::Set { name, scope, value } => {
            // Validate (and warn about) the scope before prompting, so a typo
            // doesn't cost the operator a re-typed secret.
            let parsed_scope = parse_scope(&scope)?;
            if scope == "global" {
                eprintln!(
                    "⚠ scope 'global' makes '{name}' readable by EVERY agent and tool. \
                     Prefer --scope agent:<name> or --scope tool:<name>."
                );
            }

            let value = resolve_secret_value(
                &name,
                value,
                &format!("Enter value for '{name}' (input hidden): "),
            )?;

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
                KernelResponse::Error { message } => return Err(anyhow::anyhow!("{message}")),
                _ => return Err(anyhow::anyhow!("unexpected kernel response")),
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
                KernelResponse::Error { message } => return Err(anyhow::anyhow!("{message}")),
                _ => return Err(anyhow::anyhow!("unexpected kernel response")),
            }
        }

        SecretCommands::Revoke { name } => {
            let response = client
                .send_command(KernelCommand::RevokeSecret { name: name.clone() })
                .await?;
            match response {
                KernelResponse::Success { .. } => println!("✅ Secret '{}' revoked", name),
                KernelResponse::Error { message } => return Err(anyhow::anyhow!("{message}")),
                _ => return Err(anyhow::anyhow!("unexpected kernel response")),
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
                KernelResponse::Error { message } => return Err(anyhow::anyhow!("{message}")),
                _ => return Err(anyhow::anyhow!("unexpected kernel response")),
            }
        }

        SecretCommands::Rotate { name, value } => {
            let new_value = resolve_secret_value(
                &name,
                value,
                &format!("Enter new value for '{name}' (input hidden): "),
            )?;

            let response = client
                .send_command(KernelCommand::RotateSecret {
                    name: name.clone(),
                    new_value,
                })
                .await?;

            match response {
                KernelResponse::Success { .. } => println!("✅ Secret '{}' rotated", name),
                KernelResponse::Error { message } => return Err(anyhow::anyhow!("{message}")),
                _ => return Err(anyhow::anyhow!("unexpected kernel response")),
            }
        }
    }
    Ok(())
}

/// Resolve a secret value from (in priority order): an explicit `--value` flag,
/// a TTY hidden prompt, or one line piped on stdin (non-interactive / CI).
fn resolve_secret_value(
    name: &str,
    provided: Option<String>,
    prompt: &str,
) -> anyhow::Result<zeroize::Zeroizing<String>> {
    use std::io::{IsTerminal, Read};

    let value = zeroize::Zeroizing::new(match provided {
        Some(v) => {
            eprintln!(
                "⚠ --value exposes the secret in the process list and shell history; \
                 prefer piping it on stdin instead (printf %s \"$TOKEN\" | agentos secret …)"
            );
            v
        }
        None if std::io::stdin().is_terminal() => {
            eprint!("{prompt}");
            rpassword::read_password()?
        }
        None => {
            // Non-interactive: consume stdin (allows `printf %s "$TOKEN" | … set NAME`).
            let mut buf = zeroize::Zeroizing::new(String::new());
            std::io::stdin().read_to_string(&mut buf)?;
            buf.trim_end_matches(['\n', '\r']).to_string()
        }
    });

    if value.is_empty() {
        anyhow::bail!(
            "Secret value for '{name}' cannot be empty (pass --value, pipe it on stdin, or type it at the prompt)"
        );
    }
    Ok(value)
}

fn parse_scope(s: &str) -> anyhow::Result<SecretScope> {
    // NOTE: agent: and tool: scopes are resolved server-side by the kernel using
    // the scope_raw field — which always accompanies the scope placeholder sent here.
    // This function validates the format and returns a client-side placeholder only.
    // The placeholder is `Kernel`, not `Global`: if the scope_raw wiring ever
    // regresses, the secret ends up kernel-only (unreadable by agents) instead of
    // world-readable. Same fail-closed choice as the REST twin in
    // `agentos-api/src/kernel_impl.rs::parse_scope`.
    match s {
        "global" => Ok(SecretScope::Global),
        s if s.starts_with("agent:") => {
            let name = &s[6..];
            if name.is_empty() {
                anyhow::bail!("agent scope requires a name, e.g. 'agent:worker'");
            }
            Ok(SecretScope::Kernel) // fail-closed placeholder; kernel resolves via scope_raw
        }
        s if s.starts_with("tool:") => {
            let name = &s[5..];
            if name.is_empty() {
                anyhow::bail!("tool scope requires a name, e.g. 'tool:file-reader'");
            }
            Ok(SecretScope::Kernel) // fail-closed placeholder; kernel resolves via scope_raw
        }
        _ => anyhow::bail!(
            "Invalid scope: '{}'. Use 'global', 'agent:<name>', or 'tool:<name>'",
            s
        ),
    }
}
