use agentos_bus::client::BusClient;
use agentos_bus::message::{KernelCommand, KernelResponse};
use clap::Subcommand;

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
