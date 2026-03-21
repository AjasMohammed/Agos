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
                })
                .await?;

            match response {
                KernelResponse::Success { data } => {
                    println!("✅ Agent '{}' connected", name);
                    if let Some(tid) = data
                        .as_ref()
                        .and_then(|d| d.get("test_task_id"))
                        .and_then(|v| v.as_str())
                    {
                        println!();
                        println!("  Test mode: ecosystem evaluation task queued.");
                        println!("  Task ID : {}", tid);
                        println!("  Monitor : agentctl task logs {}", tid);
                        println!("  Results will appear once the agent completes its evaluation.");
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
