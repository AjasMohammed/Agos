use clap::Subcommand;

#[derive(Subcommand)]
pub enum A2ACommands {
    /// Display this agent's A2A identity card
    Card {
        /// A2A server URL (default: http://localhost:3001)
        #[arg(long, default_value = "http://localhost:3001")]
        url: String,
    },

    /// Discover an external A2A agent's capabilities
    Discover {
        /// Base URL of the external agent
        agent_url: String,
    },

    /// Delegate a task to an external A2A agent
    Delegate {
        /// Base URL of the external agent
        #[arg(long)]
        agent: String,

        /// Capability name to invoke
        #[arg(long)]
        capability: String,

        /// Input JSON for the capability
        #[arg(long, default_value = "{}")]
        input: String,

        /// Bearer token for authenticating with the external agent
        #[arg(long)]
        token: Option<String>,

        /// Poll until task completes and print result
        #[arg(long)]
        wait: bool,
    },

    /// List active A2A task delegations
    Tasks {
        /// A2A server URL (default: http://localhost:3001)
        #[arg(long, default_value = "http://localhost:3001")]
        url: String,
    },
}

pub async fn handle(command: A2ACommands) -> anyhow::Result<()> {
    match command {
        A2ACommands::Card { url } => {
            let card_url = format!("{}/.well-known/agent.json", url.trim_end_matches('/'));
            let resp = reqwest::get(card_url).await?.error_for_status()?;
            let card: serde_json::Value = resp.json().await?;
            println!("{}", serde_json::to_string_pretty(&card)?);
        }

        A2ACommands::Discover { agent_url } => {
            let client = agentos_mcp::a2a::A2AClient::new(&agent_url);
            let card = client.discover().await?;
            println!("Agent: {} v{}", card.name, card.version);
            println!("Provider: {}", card.provider);
            println!("Endpoint: {}", card.url);
            println!("Protocol: {}", card.protocol_version);
            if card.capabilities.is_empty() {
                println!("Capabilities: (none declared)");
            } else {
                println!("Capabilities:");
                for cap in &card.capabilities {
                    println!("  - {} : {}", cap.name, cap.description);
                }
            }
        }

        A2ACommands::Delegate {
            agent,
            capability,
            input,
            token,
            wait,
        } => {
            let input_json: serde_json::Value = serde_json::from_str(&input)
                .map_err(|e| anyhow::anyhow!("Invalid JSON for --input: {}", e))?;

            let mut client = agentos_mcp::a2a::A2AClient::new(&agent);
            if let Some(t) = token {
                client = client.with_token(&t);
            }

            let sender_url = "http://agentos-agent"; // placeholder sender identity
            let task_id = client
                .submit_task(&capability, input_json, sender_url)
                .await?;
            println!("Task submitted: {}", task_id);

            if wait {
                let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(600);
                loop {
                    if tokio::time::Instant::now() > deadline {
                        eprintln!("Timed out waiting for task {} after 600s", task_id);
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    match client.poll_task(&task_id).await {
                        Ok(a2a_task) => {
                            if a2a_task.is_terminal() {
                                println!("{}", serde_json::to_string_pretty(&a2a_task)?);
                                break;
                            }
                            println!("Status: {:?}... polling", a2a_task.status);
                        }
                        Err(e) => {
                            // Transient network error — print and retry
                            eprintln!("Poll error (retrying): {}", e);
                        }
                    }
                }
            }
        }

        A2ACommands::Tasks { url } => {
            println!(
                "Active A2A task tracking is server-side. Check the agent card at {}/a2a/tasks",
                url.trim_end_matches('/')
            );
            println!(
                "Use 'agentos a2a card --url {}' to see the agent status.",
                url
            );
        }
    }
    Ok(())
}
