use agentos_bus::client::BusClient;
use agentos_bus::message::{KernelCommand, KernelResponse};
use agentos_tools::signing::{pubkey_hex_from_seed, sign_manifest, verify_manifest};
use agentos_types::ToolManifest;
use clap::Subcommand;
use rand::rngs::OsRng;
use rand::RngCore;

#[derive(Subcommand)]
pub enum ToolCommands {
    /// List installed tools
    List,
    /// Install a tool from its manifest file (verifies trust-tier signature)
    Install {
        /// Path to the tool manifest (.toml)
        path: String,
    },
    /// Remove an installed tool
    Remove {
        /// Tool name to remove
        name: String,
    },

    // ── Offline signing commands (no kernel connection needed) ──────────────
    /// Generate a new Ed25519 keypair for tool signing
    Keygen {
        /// Write keypair JSON to this file
        #[arg(long, default_value = "tool-keypair.json")]
        output: String,
    },

    /// Sign a tool manifest with an Ed25519 private key
    Sign {
        /// Path to the tool manifest (.toml) to sign
        #[arg(long)]
        manifest: String,
        /// Path to keypair JSON file produced by `tool keygen`
        #[arg(long)]
        key: String,
        /// Write the signed manifest to this path (defaults to overwriting the source)
        #[arg(long)]
        output: Option<String>,
    },

    /// Verify the Ed25519 signature on a tool manifest
    Verify {
        /// Path to the tool manifest (.toml) to verify
        manifest: String,
    },
}

/// Returns true if this subcommand can run without a kernel bus connection.
pub fn is_offline(cmd: &ToolCommands) -> bool {
    matches!(
        cmd,
        ToolCommands::Keygen { .. } | ToolCommands::Sign { .. } | ToolCommands::Verify { .. }
    )
}

/// Handle offline tool subcommands that don't require a kernel connection.
pub fn handle_offline(command: ToolCommands) -> anyhow::Result<()> {
    match command {
        ToolCommands::Keygen { output } => {
            let mut seed = [0u8; 32];
            OsRng.fill_bytes(&mut seed);
            let pubkey_hex = pubkey_hex_from_seed(&seed);
            let seed_hex = hex::encode(seed);

            let keypair_json = serde_json::json!({
                "pubkey": pubkey_hex,
                "seed": seed_hex,
                "algorithm": "Ed25519",
                "note": "Keep seed secret. Only distribute pubkey."
            });

            std::fs::write(&output, serde_json::to_string_pretty(&keypair_json)?)?;
            println!("Keypair written to {}", output);
            println!("Public key: {}", pubkey_hex);
            println!("Keep {} secret — it contains your signing seed.", output);
        }

        ToolCommands::Sign {
            manifest,
            key,
            output,
        } => {
            let keypair_json: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&key)?)
                    .map_err(|e| anyhow::anyhow!("Invalid keypair file: {}", e))?;

            let seed_hex = keypair_json["seed"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("keypair file missing 'seed' field"))?;
            let pubkey_hex = keypair_json["pubkey"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("keypair file missing 'pubkey' field"))?
                .to_string();

            let seed_bytes =
                hex::decode(seed_hex).map_err(|e| anyhow::anyhow!("Invalid seed hex: {}", e))?;
            let seed_array: [u8; 32] = seed_bytes
                .try_into()
                .map_err(|_| anyhow::anyhow!("Seed must be 32 bytes"))?;

            let content = std::fs::read_to_string(&manifest)?;
            let mut tool_manifest: ToolManifest =
                toml::from_str(&content).map_err(|e| anyhow::anyhow!("Invalid manifest: {}", e))?;

            tool_manifest.manifest.author_pubkey = Some(pubkey_hex);
            let sig_hex = sign_manifest(&tool_manifest, &seed_array);
            tool_manifest.manifest.signature = Some(sig_hex.clone());

            // Verify immediately to catch any regression
            verify_manifest(&tool_manifest)
                .map_err(|e| anyhow::anyhow!("Self-check failed after signing: {}", e))?;

            let signed_toml = toml::to_string_pretty(&tool_manifest)
                .map_err(|e| anyhow::anyhow!("Cannot serialize signed manifest: {}", e))?;

            let dest = output.as_deref().unwrap_or(&manifest);
            std::fs::write(dest, &signed_toml)?;

            println!("Signed manifest written to {}", dest);
            println!("Signature: {}", sig_hex);
        }

        ToolCommands::Verify { manifest } => {
            let content = std::fs::read_to_string(&manifest)?;
            let tool_manifest: ToolManifest =
                toml::from_str(&content).map_err(|e| anyhow::anyhow!("Invalid manifest: {}", e))?;

            let name = &tool_manifest.manifest.name;
            let tier = format!("{:?}", tool_manifest.manifest.trust_tier).to_lowercase();

            match verify_manifest(&tool_manifest) {
                Ok(()) => {
                    println!("OK  {} (trust_tier={})", name, tier);
                }
                Err(e) => {
                    eprintln!("FAIL  {} (trust_tier={}): {}", name, tier, e);
                    std::process::exit(1);
                }
            }
        }

        _ => unreachable!("handle_offline called with an online command"),
    }
    Ok(())
}

/// Handle online tool subcommands that require a kernel bus connection.
pub async fn handle(client: &mut BusClient, command: ToolCommands) -> anyhow::Result<()> {
    match command {
        ToolCommands::List => {
            let response = client.send_command(KernelCommand::ListTools).await?;
            match response {
                KernelResponse::ToolList(tools) => {
                    if tools.is_empty() {
                        println!("No tools installed.");
                    } else {
                        println!(
                            "{:<20} {:<12} {:<12} DESCRIPTION",
                            "NAME", "VERSION", "TRUST"
                        );
                        println!("{}", "-".repeat(70));
                        for t in tools {
                            let description = if t.manifest.description.len() > 25 {
                                format!("{}...", &t.manifest.description[..25])
                            } else {
                                t.manifest.description.clone()
                            };
                            println!(
                                "{:<20} {:<12} {:<12} {}",
                                t.manifest.name,
                                t.manifest.version,
                                format!("{:?}", t.manifest.trust_tier).to_lowercase(),
                                description
                            );
                        }
                    }
                }
                KernelResponse::Error { message } => eprintln!("Error: {}", message),
                _ => eprintln!("Unexpected response"),
            }
        }

        ToolCommands::Install { path } => {
            let response = client
                .send_command(KernelCommand::InstallTool {
                    manifest_path: path.clone(),
                })
                .await?;
            match response {
                KernelResponse::Success { .. } => println!("Tool from {} installed", path),
                KernelResponse::Error { message } => eprintln!("Error: {}", message),
                _ => eprintln!("Unexpected response"),
            }
        }

        ToolCommands::Remove { name } => {
            let response = client
                .send_command(KernelCommand::RemoveTool {
                    tool_name: name.clone(),
                })
                .await?;
            match response {
                KernelResponse::Success { .. } => println!("Tool '{}' removed", name),
                KernelResponse::Error { message } => eprintln!("Error: {}", message),
                _ => eprintln!("Unexpected response"),
            }
        }

        cmd if is_offline(&cmd) => {
            handle_offline(cmd)?;
        }

        _ => unreachable!(),
    }
    Ok(())
}
