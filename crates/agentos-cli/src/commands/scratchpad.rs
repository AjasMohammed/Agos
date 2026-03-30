use agentos_bus::client::BusClient;
use agentos_bus::message::{KernelCommand, KernelResponse};
use clap::Subcommand;
use serde_json::Value;

#[derive(Subcommand)]
pub enum ScratchpadCommands {
    /// List all scratchpad pages for an agent
    List {
        #[arg(long)]
        agent: String,
    },

    /// Read a scratchpad page by title
    Read {
        title: String,
        #[arg(long)]
        agent: String,
    },

    /// Delete a scratchpad page
    Delete {
        title: String,
        #[arg(long)]
        agent: String,
    },

    /// Show the wikilink graph for a page
    Graph {
        title: String,
        #[arg(long)]
        agent: String,
        #[arg(long, default_value_t = 2)]
        depth: usize,
    },
}

pub async fn handle(client: &mut BusClient, command: ScratchpadCommands) -> anyhow::Result<()> {
    match command {
        ScratchpadCommands::List { agent } => {
            let response = client
                .send_command(KernelCommand::ScratchListPages {
                    agent_id: agent.clone(),
                })
                .await?;

            match response {
                KernelResponse::Success { data: Some(json) } => {
                    print_list_response(&json, &agent);
                }
                KernelResponse::Error { message } => {
                    eprintln!("Error: {}", message);
                }
                _ => eprintln!("Unexpected response"),
            }
        }

        ScratchpadCommands::Read { title, agent } => {
            let response = client
                .send_command(KernelCommand::ScratchReadPage {
                    agent_id: agent,
                    title: title.clone(),
                })
                .await?;

            match response {
                KernelResponse::Success { data: Some(json) } => {
                    print_read_response(&json);
                }
                KernelResponse::Error { message } => {
                    eprintln!("Error: {}", message);
                }
                _ => eprintln!("Unexpected response"),
            }
        }

        ScratchpadCommands::Delete { title, agent } => {
            let response = client
                .send_command(KernelCommand::ScratchDeletePage {
                    agent_id: agent,
                    title: title.clone(),
                })
                .await?;

            match response {
                KernelResponse::Success { data: Some(json) } => {
                    if let Some(true) = json.get("deleted").and_then(|v| v.as_bool()) {
                        println!("Deleted: {}", title);
                    }
                }
                KernelResponse::Error { message } => {
                    eprintln!("Error: {}", message);
                }
                _ => eprintln!("Unexpected response"),
            }
        }

        ScratchpadCommands::Graph {
            title,
            agent,
            depth,
        } => {
            let response = client
                .send_command(KernelCommand::ScratchGraphPage {
                    agent_id: agent,
                    title: title.clone(),
                    depth,
                })
                .await?;

            match response {
                KernelResponse::Success { data: Some(json) } => {
                    print_graph_response(&json);
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

fn print_list_response(data: &Value, agent: &str) {
    if let Some(pages) = data.get("pages").and_then(|v| v.as_array()) {
        println!(
            "Scratchpad pages for agent '{}': ({} total)",
            agent,
            pages.len()
        );
        println!();

        if pages.is_empty() {
            println!("  (no pages)");
            return;
        }

        // Print header
        println!("{:<40} {:<20} {:<30}", "Title", "Tags", "Updated");
        println!("{}", "-".repeat(90));

        for page in pages {
            let title = page
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("(unknown)");
            let tags = page
                .get("tags")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|t| t.as_str())
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .unwrap_or_default();
            let updated = page
                .get("updated_at")
                .and_then(|v| v.as_str())
                .map(|s| s.split('T').next().unwrap_or(s))
                .unwrap_or("(unknown)");

            let title_display = if title.len() > 40 {
                format!("{}...", &title[..37])
            } else {
                title.to_string()
            };
            let tags_display = if tags.len() > 20 {
                format!("{}...", &tags[..17])
            } else {
                tags
            };

            println!("{:<40} {:<20} {:<30}", title_display, tags_display, updated);
        }
    }
}

fn print_read_response(data: &Value) {
    if let Some(true) = data.get("found").and_then(|v| v.as_bool()) {
        let title = data
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("(unknown)");
        let tags = data
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|t| t.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let created = data
            .get("created_at")
            .and_then(|v| v.as_str())
            .unwrap_or("(unknown)");
        let updated = data
            .get("updated_at")
            .and_then(|v| v.as_str())
            .unwrap_or("(unknown)");
        let content = data
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("(empty)");

        println!("Page: {}", title);
        if !tags.is_empty() {
            println!("Tags: {}", tags);
        }
        println!("Created: {}", created);
        println!("Updated: {}", updated);
        println!();
        println!("{}", "-".repeat(60));
        println!("{}", content);
    } else {
        println!("Page not found");
    }
}

fn print_graph_response(data: &Value) {
    let center = data
        .get("center")
        .and_then(|v| v.as_str())
        .unwrap_or("(unknown)");
    let node_count = data.get("node_count").and_then(|v| v.as_u64()).unwrap_or(0);
    let edge_count = data.get("edge_count").and_then(|v| v.as_u64()).unwrap_or(0);
    let depth = data.get("depth").and_then(|v| v.as_u64()).unwrap_or(0);

    println!("Wikilink graph for '{}' (depth: {})", center, depth);
    println!("Nodes: {}, Edges: {}", node_count, edge_count);
    println!();

    if let Some(nodes) = data.get("nodes").and_then(|v| v.as_array()) {
        println!("Nodes:");
        for node in nodes {
            let title = node
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("(unknown)");
            let d = node.get("depth").and_then(|v| v.as_u64()).unwrap_or(0);

            let prefix = "  ".repeat(d as usize);
            println!("{}├─ {}", prefix, title);
        }
    }

    if let Some(edges) = data.get("edges").and_then(|v| v.as_array()) {
        println!();
        println!("Links:");
        for edge in edges {
            let from = edge
                .get("from")
                .and_then(|v| v.as_str())
                .unwrap_or("(unknown)");
            let to = edge
                .get("to")
                .and_then(|v| v.as_str())
                .unwrap_or("(unknown)");
            println!("  {} → {}", from, to);
        }
    }
}
