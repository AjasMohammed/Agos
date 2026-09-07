//! CLI commands for the proactive personalization governance surface (Phase 6).
//!
//! Exposes `agentos personalization status`, `export`, and `forget`.
//! `forget` requires explicit confirmation (dialoguer `Confirm`) unless `--yes`
//! is passed — the operation is irreversible.

use agentos_bus::{KernelCommand, KernelResponse, PersonalizationAction};
use clap::Subcommand;
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum PersonalizationCommands {
    /// Show personalization subsystem status (enabled flags, row counts, retention windows).
    Status,

    /// Export all personalization data (profile, interests, recommendations) as JSON.
    ///
    /// Writes to stdout by default; use --out to save to a file.
    Export {
        /// Optional output file path (default: stdout)
        #[arg(long, short)]
        out: Option<PathBuf>,
    },

    /// Permanently wipe all personalization data (right-to-forget).
    ///
    /// Clears the profile store, interests store, recommendations store, and
    /// accepted-preference context-memory entries. This operation is irreversible.
    Forget {
        /// Skip the confirmation prompt
        #[arg(long)]
        yes: bool,
    },
}

pub async fn handle(
    client: &mut agentos_bus::client::BusClient,
    command: PersonalizationCommands,
) -> anyhow::Result<()> {
    match command {
        PersonalizationCommands::Status => {
            let resp = client
                .send_command(KernelCommand::PersonalizationGovernance {
                    action: PersonalizationAction::Status,
                })
                .await?;
            print_response(resp)
        }

        PersonalizationCommands::Export { out } => {
            let resp = client
                .send_command(KernelCommand::PersonalizationGovernance {
                    action: PersonalizationAction::Export,
                })
                .await?;

            match resp {
                KernelResponse::Success { data: Some(ref v) } => {
                    // The kernel returns `{ "json": "<serialized doc>" }`.
                    let json_str = v
                        .get("json")
                        .and_then(|j| j.as_str())
                        .ok_or_else(|| anyhow::anyhow!("unexpected export response shape"))?;

                    match out {
                        Some(path) => {
                            std::fs::write(&path, json_str).map_err(|e| {
                                anyhow::anyhow!("failed to write export to {}: {e}", path.display())
                            })?;
                            println!("Export saved to {}", path.display());
                        }
                        None => {
                            println!("{json_str}");
                        }
                    }
                    Ok(())
                }
                KernelResponse::Success { data: None } => {
                    anyhow::bail!("export returned no data")
                }
                KernelResponse::Error { message } => anyhow::bail!(message),
                other => anyhow::bail!("unexpected response: {:?}", other),
            }
        }

        PersonalizationCommands::Forget { yes } => {
            // Irreversible — require confirmation unless --yes is passed.
            if !yes {
                let confirmed = dialoguer::Confirm::new()
                    .with_prompt(
                        "This will permanently delete ALL personalization data \
                         (profile, interests, recommendations, accepted preferences). \
                         This cannot be undone. Continue?",
                    )
                    .default(false)
                    .interact()
                    .unwrap_or(false);

                if !confirmed {
                    println!("Aborted.");
                    return Ok(());
                }
            }

            let resp = client
                .send_command(KernelCommand::PersonalizationGovernance {
                    action: PersonalizationAction::Forget,
                })
                .await?;

            match resp {
                KernelResponse::Success { data } => {
                    if let Some(v) = data {
                        println!("{}", serde_json::to_string_pretty(&v)?);
                    }
                    println!("All personalization data has been forgotten.");
                    Ok(())
                }
                KernelResponse::Error { message } => {
                    // Partial forget — print the message but do not bail hard,
                    // so the operator can see what was cleared.
                    eprintln!("Warning: {message}");
                    Ok(())
                }
                other => anyhow::bail!("unexpected response: {:?}", other),
            }
        }
    }
}

fn print_response(resp: KernelResponse) -> anyhow::Result<()> {
    match resp {
        KernelResponse::Success { data } => {
            if let Some(v) = data {
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                println!("ok");
            }
            Ok(())
        }
        KernelResponse::Error { message } => anyhow::bail!(message),
        other => anyhow::bail!("unexpected response: {:?}", other),
    }
}
