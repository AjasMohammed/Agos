use agentos_bus::client::BusClient;
use agentos_bus::message::{KernelCommand, KernelResponse};
use agentos_types::agent::LLMProvider;
use clap::Subcommand;

#[derive(Subcommand)]
pub enum AgentCommands {
    /// Connect a new LLM agent
    Connect {
        /// LLM provider
        #[arg(long)]
        provider: String,
        /// Model name
        #[arg(long)]
        model: String,
        /// Agent display name
        #[arg(long)]
        name: String,
        /// Optional base URL for custom providers
        #[arg(long)]
        base_url: Option<String>,
        /// Role(s) for the agent — may be repeated (e.g. --role orchestrator).
        /// Supported: orchestrator, security-monitor, sysops, memory-manager, tool-manager, general.
        /// Defaults to "general" if omitted.
        #[arg(long = "role")]
        roles: Vec<String>,
        /// Connect in test mode: the agent receives an ecosystem-evaluation prompt
        /// instead of starting idle, and is asked to provide usability feedback.
        #[arg(long, default_value_t = false)]
        test: bool,
        /// Extra permissions to grant on connect (format: resource:flags, e.g. process.exec:x).
        /// May be repeated: --grant process.exec:x --grant fs.data:rw
        #[arg(long = "grant")]
        grants: Vec<String>,
    },
    /// List connected agents
    List,
    /// Disconnect an agent
    Disconnect {
        /// Agent name to disconnect
        name: String,
    },
    /// Send a message to an agent
    Message {
        /// Sender agent name
        #[arg(long)]
        from: String,
        /// Target agent name
        to: String,
        /// Message content
        content: String,
    },
    /// List messages for an agent
    Messages {
        /// Agent name
        agent: String,
        /// Number of recent messages to show
        #[arg(long, default_value = "10")]
        last: u32,
    },
    /// Manage agent groups
    Group {
        #[command(subcommand)]
        command: AgentGroupCommands,
    },
    /// Manage agent context memory
    Memory {
        #[command(subcommand)]
        command: AgentMemoryCommands,
    },
    /// Broadcast a message to a group
    Broadcast {
        /// Sender agent name
        #[arg(long)]
        from: String,
        /// Target group name
        group: String,
        /// Message content
        content: String,
    },
}

#[derive(Subcommand)]
pub enum AgentMemoryCommands {
    /// Show the current context memory for an agent
    Show {
        /// Agent name or ID
        agent: String,
    },
    /// Show context memory version history
    History {
        /// Agent name or ID
        agent: String,
        /// Number of versions to show
        #[arg(long, default_value = "10")]
        limit: u32,
    },
    /// Rollback to a specific version
    Rollback {
        /// Agent name or ID
        agent: String,
        /// Version number to restore
        version: u32,
    },
    /// Clear the agent's context memory
    Clear {
        /// Agent name or ID
        agent: String,
    },
    /// Set context memory from a file
    Set {
        /// Agent name or ID
        agent: String,
        /// Path to markdown file
        #[arg(long)]
        file: String,
    },
}

#[derive(Subcommand)]
pub enum AgentGroupCommands {
    /// Create a new agent group
    Create {
        /// Group name
        name: String,
        /// Comma-separated list of agent names
        #[arg(long)]
        members: String,
    },
}

pub async fn handle(client: &mut BusClient, command: AgentCommands) -> anyhow::Result<()> {
    match command {
        AgentCommands::Connect {
            provider,
            model,
            name,
            base_url,
            roles,
            test,
            grants,
        } => {
            let provider = parse_provider(&provider)?;
            let response = client
                .send_command(KernelCommand::ConnectAgent {
                    provider,
                    model,
                    name: name.clone(),
                    base_url,
                    roles,
                    test_mode: test,
                    extra_permissions: grants,
                })
                .await?;

            match response {
                KernelResponse::Success { data } => {
                    println!("✅ Agent '{}' connected", name);
                    if let Some(tid) = data
                        .as_ref()
                        .and_then(|d| d.get("onboarding_task_id"))
                        .and_then(|v| v.as_str())
                    {
                        println!();
                        if test {
                            println!("  Test mode: ecosystem evaluation task queued.");
                        } else {
                            println!(
                                "  Onboarding task queued — agent is exploring the ecosystem."
                            );
                        }
                        println!("  Task ID : {}", tid);
                        println!("  Monitor : agentctl task logs {}", tid);
                    }
                }
                KernelResponse::Error { message } => eprintln!("❌ Error: {}", message),
                _ => eprintln!("❌ Unexpected response"),
            }
        }
        AgentCommands::List => {
            let response = client.send_command(KernelCommand::ListAgents).await?;
            match response {
                KernelResponse::AgentList(agents) => {
                    if agents.is_empty() {
                        println!("No connected agents.");
                    } else {
                        println!("{:<20} {:<15} MODEL", "NAME", "PROVIDER");
                        println!("{}", "-".repeat(50));
                        for a in agents {
                            println!(
                                "{:<20} {:<15} {}",
                                a.name,
                                format!("{:?}", a.provider),
                                a.model
                            );
                        }
                    }
                }
                KernelResponse::Error { message } => eprintln!("❌ Error: {}", message),
                _ => eprintln!("❌ Unexpected response"),
            }
        }
        AgentCommands::Disconnect { name } => {
            let list_resp = client.send_command(KernelCommand::ListAgents).await?;
            let agent_id = match list_resp {
                KernelResponse::AgentList(agents) => {
                    agents.into_iter().find(|a| a.name == name).map(|a| a.id)
                }
                _ => anyhow::bail!("Failed to list agents"),
            };
            let Some(agent_id) = agent_id else {
                anyhow::bail!("Agent '{}' not found", name);
            };

            let response = client
                .send_command(KernelCommand::DisconnectAgent { agent_id })
                .await?;
            match response {
                KernelResponse::Success { .. } => println!("✅ Agent '{}' disconnected", name),
                KernelResponse::Error { message } => eprintln!("❌ Error: {}", message),
                _ => eprintln!("❌ Unexpected response"),
            }
        }
        AgentCommands::Message { from, to, content } => {
            let response = client
                .send_command(KernelCommand::SendAgentMessage {
                    from_name: from,
                    to_name: to.clone(),
                    content,
                })
                .await?;
            match response {
                KernelResponse::Success { .. } => println!("✅ Message sent to '{}'", to),
                KernelResponse::Error { message } => eprintln!("❌ Error: {}", message),
                _ => eprintln!("❌ Unexpected response"),
            }
        }
        AgentCommands::Messages { agent, last } => {
            let response = client
                .send_command(KernelCommand::ListAgentMessages {
                    agent_name: agent.clone(),
                    limit: last,
                })
                .await?;
            match response {
                KernelResponse::AgentMessageList(messages) => {
                    if messages.is_empty() {
                        println!("No messages for '{}'.", agent);
                    } else {
                        println!("Messages for '{}':", agent);
                        for m in messages {
                            let content_str = match m.content {
                                agentos_types::MessageContent::Text(ref text) => text.clone(),
                                agentos_types::MessageContent::Structured(ref v) => v.to_string(),
                                agentos_types::MessageContent::TaskDelegation {
                                    ref prompt,
                                    ..
                                } => format!("Delegation: {}", prompt),
                                agentos_types::MessageContent::TaskResult {
                                    ref result, ..
                                } => format!("Result: {}", result),
                            };
                            println!(
                                "[{}] From: {} -> {}",
                                m.timestamp.format("%H:%M:%S"),
                                m.from,
                                content_str
                            );
                        }
                    }
                }
                KernelResponse::Error { message } => eprintln!("❌ Error: {}", message),
                _ => eprintln!("❌ Unexpected response"),
            }
        }
        AgentCommands::Group {
            command: AgentGroupCommands::Create { name, members },
        } => {
            let member_vec: Vec<String> =
                members.split(',').map(|s| s.trim().to_string()).collect();
            let response = client
                .send_command(KernelCommand::CreateAgentGroup {
                    group_name: name.clone(),
                    members: member_vec,
                })
                .await?;
            match response {
                KernelResponse::Success { .. } => println!("✅ Group '{}' created", name),
                KernelResponse::Error { message } => eprintln!("❌ Error: {}", message),
                _ => eprintln!("❌ Unexpected response"),
            }
        }
        AgentCommands::Memory { command } => {
            handle_memory(client, command).await?;
        }
        AgentCommands::Broadcast {
            from,
            group,
            content,
        } => {
            let response = client
                .send_command(KernelCommand::BroadcastToGroup {
                    from_name: from,
                    group_name: group.clone(),
                    content,
                })
                .await?;
            match response {
                KernelResponse::Success { data } => {
                    let sent_to = data
                        .and_then(|d| d.get("sent_to").and_then(|v| v.as_u64()))
                        .unwrap_or(0);
                    println!(
                        "✅ Broadcast sent to {} agents in group '{}'",
                        sent_to, group
                    );
                }
                KernelResponse::Error { message } => eprintln!("❌ Error: {}", message),
                _ => eprintln!("❌ Unexpected response"),
            }
        }
    }
    Ok(())
}

async fn handle_memory(client: &mut BusClient, command: AgentMemoryCommands) -> anyhow::Result<()> {
    match command {
        AgentMemoryCommands::Show { agent } => {
            let response = client
                .send_command(KernelCommand::ContextMemoryRead {
                    agent_id: agent.clone(),
                })
                .await?;
            match response {
                KernelResponse::Success { data } => {
                    if let Some(data) = data {
                        let version = data.get("version").and_then(|v| v.as_u64()).unwrap_or(0);
                        let token_count = data
                            .get("token_count")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        let content = data.get("content").and_then(|v| v.as_str()).unwrap_or("");
                        if version == 0 || content.is_empty() {
                            println!("Agent '{}' has no context memory set.", agent);
                        } else {
                            println!(
                                "Agent '{}' context memory (v{}, {} tokens):\n",
                                agent, version, token_count
                            );
                            println!("{}", content);
                        }
                    }
                }
                KernelResponse::Error { message } => eprintln!("Error: {}", message),
                _ => eprintln!("Unexpected response"),
            }
        }
        AgentMemoryCommands::History { agent, limit } => {
            let response = client
                .send_command(KernelCommand::ContextMemoryHistory {
                    agent_id: agent.clone(),
                    limit,
                })
                .await?;
            match response {
                KernelResponse::Success { data } => {
                    if let Some(data) = data {
                        let versions = data
                            .get("versions")
                            .and_then(|v| v.as_array())
                            .cloned()
                            .unwrap_or_default();
                        if versions.is_empty() {
                            println!("No version history for agent '{}'.", agent);
                        } else {
                            println!("{:<8} {:<10} {:<25} REASON", "VERSION", "TOKENS", "UPDATED");
                            println!("{}", "-".repeat(60));
                            for v in versions {
                                let ver = v.get("version").and_then(|x| x.as_u64()).unwrap_or(0);
                                let tokens =
                                    v.get("token_count").and_then(|x| x.as_u64()).unwrap_or(0);
                                let updated =
                                    v.get("updated_at").and_then(|x| x.as_str()).unwrap_or("-");
                                let reason =
                                    v.get("reason").and_then(|x| x.as_str()).unwrap_or("-");
                                println!("{:<8} {:<10} {:<25} {}", ver, tokens, updated, reason);
                            }
                        }
                    }
                }
                KernelResponse::Error { message } => eprintln!("Error: {}", message),
                _ => eprintln!("Unexpected response"),
            }
        }
        AgentMemoryCommands::Rollback { agent, version } => {
            let response = client
                .send_command(KernelCommand::ContextMemoryRollback {
                    agent_id: agent.clone(),
                    version,
                })
                .await?;
            match response {
                KernelResponse::Success { data } => {
                    let new_ver = data
                        .as_ref()
                        .and_then(|d| d.get("new_version"))
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    println!(
                        "Rolled back agent '{}' to version {} (new version: {})",
                        agent, version, new_ver
                    );
                }
                KernelResponse::Error { message } => eprintln!("Error: {}", message),
                _ => eprintln!("Unexpected response"),
            }
        }
        AgentMemoryCommands::Clear { agent } => {
            let response = client
                .send_command(KernelCommand::ContextMemoryClear {
                    agent_id: agent.clone(),
                })
                .await?;
            match response {
                KernelResponse::Success { .. } => {
                    println!("Context memory cleared for agent '{}'.", agent);
                }
                KernelResponse::Error { message } => eprintln!("Error: {}", message),
                _ => eprintln!("Unexpected response"),
            }
        }
        AgentMemoryCommands::Set { agent, file } => {
            let content = std::fs::read_to_string(&file)
                .map_err(|e| anyhow::anyhow!("Failed to read file '{}': {}", file, e))?;
            let response = client
                .send_command(KernelCommand::ContextMemorySet {
                    agent_id: agent.clone(),
                    content,
                })
                .await?;
            match response {
                KernelResponse::Success { data } => {
                    let tokens = data
                        .as_ref()
                        .and_then(|d| d.get("token_count"))
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    println!(
                        "Context memory set for agent '{}' ({} tokens).",
                        agent, tokens
                    );
                }
                KernelResponse::Error { message } => eprintln!("Error: {}", message),
                _ => eprintln!("Unexpected response"),
            }
        }
    }
    Ok(())
}

fn parse_provider(s: &str) -> anyhow::Result<LLMProvider> {
    match s.to_lowercase().as_str() {
        "ollama" => Ok(LLMProvider::Ollama),
        "openai" => Ok(LLMProvider::OpenAI),
        "anthropic" => Ok(LLMProvider::Anthropic),
        "gemini" => Ok(LLMProvider::Gemini),
        p if p.starts_with("custom:") => {
            let parts: Vec<&str> = p.splitn(2, ':').collect();
            Ok(LLMProvider::Custom(parts[1].to_string()))
        }
        "custom" => Ok(LLMProvider::Custom("custom".to_string())),
        _ => anyhow::bail!("Unknown provider '{}'", s),
    }
}
