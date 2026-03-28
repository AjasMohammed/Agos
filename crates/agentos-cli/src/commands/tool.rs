use agentos_bus::client::BusClient;
use agentos_bus::message::{KernelCommand, KernelResponse};
use agentos_tools::signing::{pubkey_hex_from_seed, sign_manifest, verify_manifest};
use agentos_types::ToolManifest;
use clap::Subcommand;
use rand::rngs::OsRng;
use rand::RngCore;
use std::path::PathBuf;

/// HTTP timeout for all registry requests.
const REGISTRY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Subcommand)]
pub enum ToolCommands {
    /// List installed tools
    List,
    /// Install a tool from a local manifest file (verifies trust-tier signature)
    Install {
        /// Path to the tool manifest (.toml)
        path: String,
    },
    /// Remove an installed tool
    Remove {
        /// Tool name to remove
        name: String,
    },

    // ── Registry commands (require network, not kernel) ─────────────────
    /// Search the tool registry for available tools
    Search {
        /// Search query (matches name, description, tags, author)
        query: String,
        /// Maximum results to return
        #[arg(long, default_value_t = 20)]
        limit: u32,
        /// Registry URL override (default: from config or AGENTOS_REGISTRY env)
        #[arg(long)]
        registry: Option<String>,
    },

    /// Install a tool from the registry by name (downloads, verifies, hot-loads)
    Add {
        /// Tool name in the registry
        name: String,
        /// Specific version to install (default: latest)
        #[arg(long)]
        version: Option<String>,
        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
        /// Registry URL override
        #[arg(long)]
        registry: Option<String>,
    },

    /// Publish a signed tool manifest to the registry
    Publish {
        /// Path to the tool manifest (.toml)
        manifest: PathBuf,
        /// Path to keypair JSON file (default: sign with existing signature in manifest)
        #[arg(long)]
        key: Option<PathBuf>,
        /// Registry URL override
        #[arg(long)]
        registry: Option<String>,
    },

    // ── Offline signing commands (no kernel connection needed) ──────────
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
        ToolCommands::Keygen { .. }
            | ToolCommands::Sign { .. }
            | ToolCommands::Verify { .. }
            | ToolCommands::Search { .. }
            | ToolCommands::Publish { .. }
    )
}

/// Resolve the registry URL from explicit flag, env var, or config default.
fn resolve_registry_url(explicit: Option<&str>) -> String {
    if let Some(url) = explicit {
        return url.to_string();
    }
    if let Ok(url) = std::env::var("AGENTOS_REGISTRY") {
        if !url.trim().is_empty() {
            return url;
        }
    }
    "https://registry.agentos.dev".to_string()
}

/// Build a reqwest client with a standard timeout.
fn http_client() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(REGISTRY_TIMEOUT)
        .build()
        .map_err(|e| anyhow::anyhow!("Failed to build HTTP client: {}", e))
}

/// Validate tool name to prevent path traversal and filesystem issues.
fn validate_tool_name(name: &str) -> anyhow::Result<()> {
    if name.is_empty() {
        anyhow::bail!("Tool name cannot be empty");
    }
    if name.contains("..") || name.contains('/') || name.contains('\\') {
        anyhow::bail!("Tool name contains path traversal characters: '{}'", name);
    }
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        anyhow::bail!(
            "Tool name must contain only alphanumeric, '-', or '_': '{}'",
            name
        );
    }
    Ok(())
}

/// Truncate a string to at most `max_chars` characters (UTF-8 safe).
fn truncate_str(s: &str, max_chars: usize) -> String {
    let truncated: String = s.chars().take(max_chars).collect();
    if truncated.len() < s.len() {
        format!("{}...", truncated)
    } else {
        truncated
    }
}

/// Handle offline tool subcommands that don't require a kernel connection.
/// This is async because Search and Publish need network access.
pub async fn handle_offline(command: ToolCommands) -> anyhow::Result<()> {
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

        ToolCommands::Search {
            query,
            limit,
            registry,
        } => {
            cmd_search(&query, limit, registry.as_deref()).await?;
        }

        ToolCommands::Publish {
            manifest,
            key,
            registry,
        } => {
            cmd_publish(&manifest, key.as_deref(), registry.as_deref()).await?;
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
                            let description = truncate_str(&t.manifest.description, 25);
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

        ToolCommands::Add {
            name,
            version,
            yes,
            registry,
        } => {
            cmd_add(client, &name, version.as_deref(), yes, registry.as_deref()).await?;
        }

        cmd if is_offline(&cmd) => {
            handle_offline(cmd).await?;
        }

        _ => unreachable!(),
    }
    Ok(())
}

// ── Registry subcommand implementations ────────────────────────────────

/// `agentctl tool search <query>` — search the registry.
async fn cmd_search(query: &str, limit: u32, registry: Option<&str>) -> anyhow::Result<()> {
    let base = resolve_registry_url(registry);
    let client = http_client()?;

    let resp = client
        .get(format!("{}/v1/tools", base))
        .query(&[("q", query), ("limit", &limit.to_string())])
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Registry request failed: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Registry returned {}: {}", status, body);
    }

    let tools: Vec<serde_json::Value> = resp.json().await?;

    if tools.is_empty() {
        println!("No tools found for '{}'.", query);
        return Ok(());
    }

    println!(
        "{:<25} {:<10} {:<12} {:<10} DESCRIPTION",
        "NAME", "VERSION", "AUTHOR", "DOWNLOADS"
    );
    println!("{}", "-".repeat(80));
    for t in &tools {
        let desc = t["description"].as_str().unwrap_or("");
        let short_desc = truncate_str(desc, 30);
        println!(
            "{:<25} {:<10} {:<12} {:<10} {}",
            t["name"].as_str().unwrap_or("?"),
            t["version"].as_str().unwrap_or("?"),
            t["author"].as_str().unwrap_or("?"),
            t["downloads"].as_i64().unwrap_or(0),
            short_desc,
        );
    }
    Ok(())
}

/// `agentctl tool add <name>` — fetch from registry, verify, write to tools/user/, hot-load.
async fn cmd_add(
    client: &mut BusClient,
    name: &str,
    version: Option<&str>,
    yes: bool,
    registry: Option<&str>,
) -> anyhow::Result<()> {
    // Validate tool name before doing anything.
    validate_tool_name(name)?;

    let base = resolve_registry_url(registry);
    let http = http_client()?;

    // Fetch the manifest from the registry.
    let url = if let Some(v) = version {
        format!("{}/v1/tools/{}/{}", base, name, v)
    } else {
        format!("{}/v1/tools/{}", base, name)
    };

    let resp = http
        .get(&url)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Registry request failed: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!(
            "Tool '{}' not found in registry ({}): {}",
            name,
            status,
            body
        );
    }

    let entry: serde_json::Value = resp.json().await?;
    let manifest_toml = entry["manifest_toml"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Registry response missing manifest_toml field"))?;

    // Parse and verify the manifest locally BEFORE showing info to the user.
    // This prevents displaying a "looks legitimate" prompt for an unsigned tool.
    let manifest: ToolManifest = toml::from_str(manifest_toml)
        .map_err(|e| anyhow::anyhow!("Invalid manifest from registry: {}", e))?;

    verify_manifest(&manifest)
        .map_err(|e| anyhow::anyhow!("Signature verification failed: {}", e))?;

    let tool_version = &manifest.manifest.version;
    let trust_tier = format!("{:?}", manifest.manifest.trust_tier).to_lowercase();

    println!("Tool:        {} v{}", manifest.manifest.name, tool_version);
    println!("Trust tier:  {}", trust_tier);
    println!("Author:      {}", manifest.manifest.author);
    if let Some(ref pk) = manifest.manifest.author_pubkey {
        let display_len = pk.len().min(16);
        let safe_prefix: String = pk.chars().take(display_len).collect();
        println!("Pubkey:      {}...", safe_prefix);
    }
    println!("Signature:   verified");

    if !yes {
        print!("Install this tool? [y/N] ");
        use std::io::Write;
        std::io::stdout().flush()?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    // Write manifest to tools/user/<name>.toml.
    let tools_dir = std::env::var("AGENTOS_USER_TOOLS_DIR")
        .unwrap_or_else(|_| "/tmp/agentos/tools/user".to_string());
    std::fs::create_dir_all(&tools_dir)?;
    let manifest_path = format!("{}/{}.toml", tools_dir, name);
    std::fs::write(&manifest_path, manifest_toml)?;

    // Hot-load into the running kernel via ToolLoad command.
    let response = client
        .send_command(KernelCommand::ToolLoad {
            manifest_path: manifest_path.clone(),
        })
        .await?;

    match response {
        KernelResponse::Success { data } => {
            let tool_id = data
                .as_ref()
                .and_then(|d| d["tool_id"].as_str())
                .unwrap_or("unknown");
            println!(
                "Installed {} v{} (id: {})",
                manifest.manifest.name, tool_version, tool_id
            );
        }
        KernelResponse::Error { message } => {
            anyhow::bail!("Kernel rejected tool: {}", message);
        }
        _ => anyhow::bail!("Unexpected kernel response"),
    }
    Ok(())
}

/// `agentctl tool publish <manifest>` — publish a tool to the registry.
async fn cmd_publish(
    manifest_path: &std::path::Path,
    key_path: Option<&std::path::Path>,
    registry: Option<&str>,
) -> anyhow::Result<()> {
    let content = std::fs::read_to_string(manifest_path)?;
    let mut manifest: ToolManifest =
        toml::from_str(&content).map_err(|e| anyhow::anyhow!("Invalid manifest: {}", e))?;

    // If a key file is provided, sign the manifest before publishing.
    if let Some(key) = key_path {
        let keypair_json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(key)?)
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

        manifest.manifest.author_pubkey = Some(pubkey_hex);
        let sig = sign_manifest(&manifest, &seed_array);
        manifest.manifest.signature = Some(sig);
    }

    // Verify before sending (catch issues early).
    verify_manifest(&manifest)
        .map_err(|e| anyhow::anyhow!("Manifest signature verification failed: {}", e))?;

    let signed_toml = toml::to_string_pretty(&manifest)
        .map_err(|e| anyhow::anyhow!("Cannot serialize manifest: {}", e))?;

    let base = resolve_registry_url(registry);
    let resp = http_client()?
        .post(format!("{}/v1/tools", base))
        .json(&serde_json::json!({ "manifest_toml": signed_toml }))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Registry request failed: {}", e))?;

    if resp.status().is_success() {
        println!(
            "Published {} v{}",
            manifest.manifest.name, manifest.manifest.version
        );
    } else {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Publish failed ({}): {}", status, body);
    }
    Ok(())
}
