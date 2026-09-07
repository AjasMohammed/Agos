use agentos_bus::client::BusClient;
use agentos_bus::message::{KernelCommand, KernelResponse};
use clap::Subcommand;

#[derive(Subcommand)]
pub enum OrgCommands {
    /// Add (or update) a node in an agent org chart.
    AddNode {
        /// Org id (UUID). Generate one with any uuid tool; reuse it for the whole org.
        #[arg(long)]
        org: String,
        /// Registered agent name this node represents.
        #[arg(long)]
        agent: String,
        /// Manager node id (UUID). Omit for the top of the org (the CEO).
        #[arg(long)]
        manager: Option<String>,
        /// "coordinator" or "worker" (default: worker).
        #[arg(long, default_value = "worker")]
        role: String,
        /// Human-readable title, e.g. "Researcher".
        #[arg(long, default_value = "")]
        title: String,
        /// Capability grant(s) of the form `<resource>:<rwxqo>` (repeatable),
        /// e.g. --scope 'fs:/home/u/docs/:r' --scope 'net::r'. Must be a subset
        /// of the manager's scope (enforced kernel-side).
        #[arg(long = "scope")]
        scope: Vec<String>,
    },
    /// List every node in an org chart.
    Show {
        /// Org id (UUID).
        #[arg(long)]
        org: String,
    },
}

pub async fn handle(client: &mut BusClient, cmd: OrgCommands) -> anyhow::Result<()> {
    match cmd {
        OrgCommands::AddNode {
            org,
            agent,
            manager,
            role,
            title,
            scope,
        } => {
            let response = client
                .send_command(KernelCommand::OrgAddNode {
                    org_id: org,
                    agent_name: agent,
                    manager_node_id: manager,
                    role,
                    title,
                    scope,
                })
                .await?;
            match response {
                KernelResponse::Success { data } => {
                    let node_id = data
                        .as_ref()
                        .and_then(|d| d.get("node_id"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    println!("Added org node {node_id}");
                }
                KernelResponse::Error { message } => {
                    anyhow::bail!("{message}");
                }
                other => anyhow::bail!("unexpected response: {other:?}"),
            }
        }
        OrgCommands::Show { org } => {
            let response = client
                .send_command(KernelCommand::OrgShow { org_id: org })
                .await?;
            match response {
                KernelResponse::Success { data } => {
                    let nodes = data
                        .as_ref()
                        .and_then(|d| d.get("nodes"))
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    if nodes.is_empty() {
                        println!("No nodes in this org.");
                        return Ok(());
                    }
                    println!(
                        "{:<38} {:<16} {:<12} {:<20} MANAGER",
                        "NODE ID", "AGENT", "ROLE", "TITLE"
                    );
                    for n in &nodes {
                        let get = |k: &str| n.get(k).and_then(|v| v.as_str()).unwrap_or("");
                        let manager = n
                            .get("manager_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("(root)");
                        println!(
                            "{:<38} {:<16} {:<12} {:<20} {}",
                            get("node_id"),
                            get("agent_name"),
                            get("role"),
                            get("title"),
                            manager
                        );
                    }
                }
                KernelResponse::Error { message } => {
                    anyhow::bail!("{message}");
                }
                other => anyhow::bail!("unexpected response: {other:?}"),
            }
        }
    }
    Ok(())
}
