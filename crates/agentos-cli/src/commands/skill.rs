use agentos_bus::client::BusClient;
use agentos_bus::message::{KernelCommand, KernelResponse};
use clap::Subcommand;
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum SkillCommands {
    /// Install a skill from a directory containing SKILL.toml
    Install {
        /// Path to the skill directory
        path: String,
    },

    /// Remove an installed skill
    Remove {
        /// Skill name to remove
        name: String,
    },

    /// List all installed skills
    List,

    /// Run a skill by name
    Run {
        /// Skill name to run
        name: String,
        /// Optional input text for the skill
        #[arg(long)]
        input: Option<String>,
    },

    /// Show detailed status of a skill
    Status {
        /// Skill name
        name: String,
    },

    // ── Offline skill development commands ────────────────────────────
    /// Create a new skill project from a template
    New {
        /// Skill name (becomes the directory name)
        name: String,
    },

    /// Validate a SKILL.toml manifest without installing it
    Validate {
        /// Path to the skill directory containing SKILL.toml
        #[arg(default_value = ".")]
        path: String,
    },

    /// Publish a skill to a local package index
    Publish {
        /// Path to the skill directory containing SKILL.toml
        #[arg(default_value = ".")]
        path: String,
        /// Path to the package index JSON file
        #[arg(long)]
        index: Option<PathBuf>,
    },

    /// Search the local package index for skills
    Search {
        /// Search query (matches name, description, tags, author)
        query: String,
        /// Path to the package index JSON file
        #[arg(long)]
        index: Option<PathBuf>,
    },
}

/// Returns true if this subcommand can run without a kernel bus connection.
pub fn is_offline(cmd: &SkillCommands) -> bool {
    matches!(
        cmd,
        SkillCommands::New { .. }
            | SkillCommands::Validate { .. }
            | SkillCommands::Publish { .. }
            | SkillCommands::Search { .. }
    )
}

/// Handle offline skill subcommands that don't require a kernel connection.
pub async fn handle_offline(command: SkillCommands) -> anyhow::Result<()> {
    match command {
        SkillCommands::New { name } => {
            cmd_skill_new(&name)?;
        }
        SkillCommands::Validate { path } => {
            cmd_skill_validate(&path)?;
        }
        SkillCommands::Publish { path, index } => {
            cmd_skill_publish(&path, index.as_deref())?;
        }
        SkillCommands::Search { query, index } => {
            cmd_skill_search(&query, index.as_deref())?;
        }
        _ => unreachable!("non-offline command dispatched to handle_offline"),
    }
    Ok(())
}

fn cmd_skill_new(name: &str) -> anyhow::Result<()> {
    // Validate name: alphanumeric, hyphens, underscores only — no path separators or injection
    if name.is_empty() {
        anyhow::bail!("Skill name must not be empty");
    }
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        anyhow::bail!(
            "Skill name must contain only alphanumeric characters, hyphens, or underscores"
        );
    }
    if name.contains("..") {
        anyhow::bail!("Skill name must not contain '..'");
    }

    let dir = std::path::Path::new(name);
    if dir.exists() {
        anyhow::bail!("Directory '{}' already exists", name);
    }
    std::fs::create_dir_all(dir)?;

    let skill_toml = format!(
        r#"[skill]
name        = "{name}"
version     = "0.1.0"
description = "A new AgentOS skill"
author      = "your-name"
license     = "MIT"
trust_tier  = "community"

[agent]
system_prompt = """
You are a helpful agent. Your job is to ...
"""
default_provider = "anthropic"
default_model    = "claude-opus-4-6"

[tools]
required = []
optional = []

[permissions]
required = []

[budget]
max_cost_per_run    = 0.10
max_tokens_per_run  = 50000

[triggers]
schedule = ""
events   = []
"#
    );
    std::fs::write(dir.join("SKILL.toml"), &skill_toml)?;
    std::fs::write(
        dir.join("README.md"),
        format!("# {name}\n\nAn AgentOS skill.\n"),
    )?;

    println!("Created skill project '{name}'");
    println!("  Edit {name}/SKILL.toml to configure your skill.");
    println!("  Run 'agentos skill validate {name}' to check the manifest.");
    println!("  Run 'agentos skill install {name}' to install.");
    Ok(())
}

fn cmd_skill_validate(path: &str) -> anyhow::Result<()> {
    let skill_toml_path = std::path::Path::new(path).join("SKILL.toml");
    if !skill_toml_path.exists() {
        anyhow::bail!("SKILL.toml not found at '{}'", skill_toml_path.display());
    }
    let content = std::fs::read_to_string(&skill_toml_path)?;
    // Parse as generic TOML to validate syntax
    let parsed: toml::Value =
        toml::from_str(&content).map_err(|e| anyhow::anyhow!("TOML parse error: {}", e))?;

    // Check required top-level fields
    let skill = parsed
        .get("skill")
        .ok_or_else(|| anyhow::anyhow!("Missing [skill] table"))?;

    let name = skill
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing skill.name"))?;
    let version = skill
        .get("version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing skill.version"))?;
    let description = skill
        .get("description")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing skill.description"))?;

    parsed
        .get("agent")
        .ok_or_else(|| anyhow::anyhow!("Missing [agent] table"))?;

    println!("SKILL.toml valid:");
    println!("  name:        {name}");
    println!("  version:     {version}");
    println!("  description: {description}");
    Ok(())
}

fn cmd_skill_publish(path: &str, index_path: Option<&std::path::Path>) -> anyhow::Result<()> {
    use crate::package_index::{default_index_path, PackageEntry, PackageIndex};

    let skill_toml_path = std::path::Path::new(path).join("SKILL.toml");
    if !skill_toml_path.exists() {
        anyhow::bail!("SKILL.toml not found at '{}'", skill_toml_path.display());
    }
    let content = std::fs::read_to_string(&skill_toml_path)?;
    let parsed: toml::Value =
        toml::from_str(&content).map_err(|e| anyhow::anyhow!("TOML parse error: {}", e))?;

    let skill = parsed
        .get("skill")
        .ok_or_else(|| anyhow::anyhow!("Missing [skill] table"))?;
    let name = skill
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing skill.name"))?
        .to_string();
    let version = skill
        .get("version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing skill.version"))?
        .to_string();
    let description = skill
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let author = skill
        .get("author")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let tags: Vec<String> = parsed
        .get("skill")
        .and_then(|s| s.get("tags"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let index_path = index_path
        .map(|p| p.to_path_buf())
        .unwrap_or_else(default_index_path);

    let mut index = PackageIndex::load(&index_path)?;
    index.upsert_skill(PackageEntry {
        name: name.clone(),
        version: version.clone(),
        description,
        author,
        trust_tier: agentos_types::TrustTier::Community,
        signature: None,
        download_url: None,
        tags,
        manifest_path: Some(
            skill_toml_path
                .canonicalize()
                .unwrap_or(skill_toml_path)
                .to_string_lossy()
                .into_owned(),
        ),
        published_at: chrono::Utc::now().to_rfc3339(),
    });
    index.save(&index_path)?;
    println!(
        "Published skill '{name}' v{version} to {}",
        index_path.display()
    );
    Ok(())
}

fn cmd_skill_search(query: &str, index_path: Option<&std::path::Path>) -> anyhow::Result<()> {
    use crate::package_index::{default_index_path, PackageIndex};

    let index_path = index_path
        .map(|p| p.to_path_buf())
        .unwrap_or_else(default_index_path);

    let index = PackageIndex::load(&index_path)?;
    let results = index.search_skills(query);

    if results.is_empty() {
        println!("No skills found matching '{query}'.");
    } else {
        println!(
            "{:<25} {:<10} {:<15} DESCRIPTION",
            "NAME", "VERSION", "AUTHOR"
        );
        println!("{}", "-".repeat(75));
        for entry in results {
            let short_desc: String = entry.description.chars().take(30).collect();
            println!(
                "{:<25} {:<10} {:<15} {}",
                entry.name, entry.version, entry.author, short_desc
            );
        }
    }
    Ok(())
}

pub async fn handle(client: &mut BusClient, command: SkillCommands) -> anyhow::Result<()> {
    match command {
        SkillCommands::Install { path } => {
            let response = client
                .send_command(KernelCommand::SkillInstall { path: path.clone() })
                .await?;
            match response {
                KernelResponse::Success { data } => {
                    let name = data
                        .as_ref()
                        .and_then(|d| d["skill_name"].as_str())
                        .unwrap_or("unknown");
                    let version = data
                        .as_ref()
                        .and_then(|d| d["version"].as_str())
                        .unwrap_or("unknown");
                    println!("Installed skill '{}' v{}", name, version);
                }
                KernelResponse::Error { message } => {
                    eprintln!("Error: {}", message);
                }
                _ => eprintln!("Unexpected response"),
            }
        }

        SkillCommands::Remove { name } => {
            let response = client
                .send_command(KernelCommand::SkillRemove { name: name.clone() })
                .await?;
            match response {
                KernelResponse::Success { .. } => {
                    println!("Removed skill '{}'", name);
                }
                KernelResponse::Error { message } => {
                    eprintln!("Error: {}", message);
                }
                _ => eprintln!("Unexpected response"),
            }
        }

        SkillCommands::List => {
            let response = client.send_command(KernelCommand::SkillList).await?;
            match response {
                KernelResponse::SkillList(skills) => {
                    if skills.is_empty() {
                        println!("No skills installed.");
                    } else {
                        println!(
                            "{:<25} {:<10} {:<12} {:<12} DESCRIPTION",
                            "NAME", "VERSION", "AUTHOR", "TRUST"
                        );
                        println!("{}", "-".repeat(80));
                        for s in &skills {
                            let name = s["name"].as_str().unwrap_or("?");
                            let version = s["version"].as_str().unwrap_or("?");
                            let author = s["author"].as_str().unwrap_or("?");
                            let trust = s["trust_tier"].as_str().unwrap_or("?");
                            let desc = s["description"].as_str().unwrap_or("");
                            let short_desc = truncate_str(desc, 25);
                            println!(
                                "{:<25} {:<10} {:<12} {:<12} {}",
                                name, version, author, trust, short_desc
                            );
                        }
                    }
                }
                KernelResponse::Error { message } => {
                    eprintln!("Error: {}", message);
                }
                _ => eprintln!("Unexpected response"),
            }
        }

        SkillCommands::Run { name, input } => {
            let response = client
                .send_command(KernelCommand::SkillRun {
                    name: name.clone(),
                    input,
                })
                .await?;
            match response {
                KernelResponse::SkillRunResult { task_id } => {
                    println!("Skill '{}' started, task ID: {}", name, task_id);
                }
                KernelResponse::Success { data } => {
                    if let Some(d) = data {
                        println!("{}", serde_json::to_string_pretty(&d)?);
                    } else {
                        println!("Skill '{}' executed successfully", name);
                    }
                }
                KernelResponse::Error { message } => {
                    eprintln!("Error: {}", message);
                }
                _ => eprintln!("Unexpected response"),
            }
        }

        SkillCommands::Status { name } => {
            let response = client
                .send_command(KernelCommand::SkillStatus { name: name.clone() })
                .await?;
            match response {
                KernelResponse::SkillStatusInfo(info) => {
                    print_skill_status(&info);
                }
                KernelResponse::Error { message } => {
                    eprintln!("Error: {}", message);
                }
                _ => eprintln!("Unexpected response"),
            }
        }

        // Offline commands — should have been handled before reaching here.
        SkillCommands::New { .. }
        | SkillCommands::Validate { .. }
        | SkillCommands::Publish { .. }
        | SkillCommands::Search { .. } => {
            eprintln!("Internal error: offline skill command reached online handler");
        }
    }
    Ok(())
}

fn truncate_str(s: &str, max_chars: usize) -> String {
    let truncated: String = s.chars().take(max_chars).collect();
    if truncated.len() < s.len() {
        format!("{}...", truncated)
    } else {
        truncated
    }
}

fn print_skill_status(info: &serde_json::Value) {
    let name = info["name"].as_str().unwrap_or("?");
    let version = info["version"].as_str().unwrap_or("?");
    let description = info["description"].as_str().unwrap_or("");
    let author = info["author"].as_str().unwrap_or("?");
    let trust = info["trust_tier"].as_str().unwrap_or("?");
    let status = info["status"].as_str().unwrap_or("?");
    let prompt_len = info["system_prompt_length"].as_u64().unwrap_or(0);

    println!("Skill:       {} v{}", name, version);
    println!("Description: {}", description);
    println!("Author:      {}", author);
    println!("Trust tier:  {}", trust);
    println!("Status:      {}", status);
    println!("Prompt size: {} bytes", prompt_len);

    if let Some(license) = info["license"].as_str() {
        println!("License:     {}", license);
    }

    // Triggers
    if let Some(schedule) = info["triggers"]["schedule"].as_str() {
        println!("Schedule:    {}", schedule);
    }
    if let Some(events) = info["triggers"]["events"].as_array() {
        if !events.is_empty() {
            let event_list: Vec<&str> = events.iter().filter_map(|e| e.as_str()).collect();
            println!("Events:      {}", event_list.join(", "));
        }
    }

    // Agent config
    if let Some(provider) = info["agent"]["default_provider"].as_str() {
        println!("Provider:    {}", provider);
    }
    if let Some(model) = info["agent"]["default_model"].as_str() {
        println!("Model:       {}", model);
    }
    if let Some(roles) = info["agent"]["roles"].as_array() {
        if !roles.is_empty() {
            let role_list: Vec<&str> = roles.iter().filter_map(|r| r.as_str()).collect();
            println!("Roles:       {}", role_list.join(", "));
        }
    }

    // Tools
    if let Some(required) = info["tools"]["required"].as_array() {
        if !required.is_empty() {
            let tool_list: Vec<&str> = required.iter().filter_map(|t| t.as_str()).collect();
            println!("Tools (req): {}", tool_list.join(", "));
        }
    }
    if let Some(optional) = info["tools"]["optional"].as_array() {
        if !optional.is_empty() {
            let tool_list: Vec<&str> = optional.iter().filter_map(|t| t.as_str()).collect();
            println!("Tools (opt): {}", tool_list.join(", "));
        }
    }

    // Permissions
    if let Some(perms) = info["permissions"]["required"].as_array() {
        if !perms.is_empty() {
            let perm_list: Vec<&str> = perms.iter().filter_map(|p| p.as_str()).collect();
            println!("Permissions: {}", perm_list.join(", "));
        }
    }

    // Budget
    if let Some(cost) = info["budget"]["max_cost_per_run"].as_f64() {
        println!("Max cost:    ${:.2}/run", cost);
    }
    if let Some(tokens) = info["budget"]["max_tokens_per_run"].as_u64() {
        println!("Max tokens:  {}/run", tokens);
    }
}
