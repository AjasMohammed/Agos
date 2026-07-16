//! `agentos approval` — manage approval mode and learned policy entries.
//!
//! Approval mode controls when the kernel auto-approves vs. escalates a tool
//! call for human review. Per-agent overrides take precedence over the
//! global mode. Learned "allow always" policy entries lift a `Prompt`
//! decision to `Allow` for a specific `(tool, payload, agent)` match.

use agentos_bus::client::BusClient;
use agentos_bus::message::{KernelCommand, KernelResponse};
use clap::Subcommand;

#[derive(Subcommand)]
pub enum ApprovalCommands {
    /// Manage the approval mode (global default + per-agent overrides).
    Mode {
        #[command(subcommand)]
        command: ModeCommands,
    },
    /// Add a learned "allow always" policy entry. Future calls matching this
    /// rule will be auto-approved without prompting, even when the mode
    /// would normally escalate.
    Allow {
        /// Tool name (e.g. `file-writer`).
        tool: String,
        /// Optional glob to match against the payload's `path` field
        /// (e.g. `/home/alice/project/**`).
        #[arg(long)]
        path: Option<String>,
        /// Scope this entry to a single agent by display name (default: all agents).
        #[arg(long)]
        agent: Option<String>,
    },
    /// List active learned approval policy entries.
    List,
    /// Revoke a learned policy entry by its numeric `id` (see `approval list`).
    Revoke { id: i64 },
}

#[derive(Subcommand)]
pub enum ModeCommands {
    /// Show the current global mode + per-agent overrides.
    Get,
    /// Set the global approval mode.
    Set {
        /// One of: auto | ask_edit | ask_always | deny
        mode: String,
        /// Apply only to a single agent (sets a per-agent override).
        #[arg(long)]
        agent: Option<String>,
    },
    /// Clear a per-agent mode override and fall back to the global default.
    Clear {
        /// Agent display name.
        agent: String,
    },
}

pub async fn handle(client: &mut BusClient, command: ApprovalCommands) -> anyhow::Result<()> {
    match command {
        ApprovalCommands::Mode { command } => mode(client, command).await,
        ApprovalCommands::Allow { tool, path, agent } => allow(client, tool, path, agent).await,
        ApprovalCommands::List => list(client).await,
        ApprovalCommands::Revoke { id } => revoke(client, id).await,
    }
}

async fn mode(client: &mut BusClient, cmd: ModeCommands) -> anyhow::Result<()> {
    match cmd {
        ModeCommands::Get => {
            let resp = client
                .send_command(KernelCommand::GetApprovalConfig)
                .await?;
            match resp {
                KernelResponse::ApprovalConfigSnapshot {
                    mode,
                    agent_overrides,
                } => {
                    println!("global mode: {mode}");
                    if agent_overrides.is_empty() {
                        println!("(no per-agent overrides)");
                    } else {
                        println!("per-agent overrides:");
                        for (agent, m) in agent_overrides {
                            println!("  {agent} = {m}");
                        }
                    }
                    Ok(())
                }
                KernelResponse::Error { message } => anyhow::bail!("{}", message),
                other => anyhow::bail!("Unexpected response: {:?}", other),
            }
        }
        ModeCommands::Set { mode, agent } => {
            let resp = match agent {
                Some(name) => {
                    client
                        .send_command(KernelCommand::SetApprovalAgentOverride {
                            agent_name: name.clone(),
                            mode: mode.clone(),
                        })
                        .await?
                }
                None => {
                    client
                        .send_command(KernelCommand::SetApprovalMode { mode: mode.clone() })
                        .await?
                }
            };
            match resp {
                KernelResponse::Success { .. } => {
                    println!("approval mode set to {mode}");
                    println!(
                        "note: this updates the running kernel only. Edit \
                         config/default.toml `[approval]` to persist across restarts."
                    );
                    Ok(())
                }
                KernelResponse::Error { message } => anyhow::bail!("{}", message),
                other => anyhow::bail!("Unexpected response: {:?}", other),
            }
        }
        ModeCommands::Clear { agent } => {
            let resp = client
                .send_command(KernelCommand::ClearApprovalAgentOverride {
                    agent_name: agent.clone(),
                })
                .await?;
            match resp {
                KernelResponse::Success { data } => {
                    let removed = data
                        .as_ref()
                        .and_then(|v| v.get("removed"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if removed {
                        println!("cleared override for agent '{agent}'");
                    } else {
                        println!("no override was set for agent '{agent}'");
                    }
                    Ok(())
                }
                KernelResponse::Error { message } => anyhow::bail!("{}", message),
                other => anyhow::bail!("Unexpected response: {:?}", other),
            }
        }
    }
}

async fn allow(
    client: &mut BusClient,
    tool: String,
    path: Option<String>,
    agent: Option<String>,
) -> anyhow::Result<()> {
    let resp = client
        .send_command(KernelCommand::AddApprovalPolicy {
            tool_name: tool.clone(),
            path_glob: path.clone(),
            agent_name: agent.clone(),
        })
        .await?;
    match resp {
        KernelResponse::ApprovalPolicyAdded {
            id,
            tool_name,
            path_glob,
            agent_name,
        } => {
            println!(
                "added policy #{id}: tool='{tool_name}' path={} agent={}",
                path_glob.as_deref().unwrap_or("(any)"),
                agent_name.as_deref().unwrap_or("(any)"),
            );
            Ok(())
        }
        KernelResponse::Error { message } => anyhow::bail!("{}", message),
        other => anyhow::bail!("Unexpected response: {:?}", other),
    }
}

async fn list(client: &mut BusClient) -> anyhow::Result<()> {
    let resp = client
        .send_command(KernelCommand::ListApprovalPolicies)
        .await?;
    match resp {
        KernelResponse::ApprovalPolicyList(entries) => {
            if entries.is_empty() {
                println!("No active approval policies.");
                println!(
                    "Add one with: agentos approval allow <tool> [--path <glob>] [--agent <name>]"
                );
                return Ok(());
            }
            println!(
                "{:<4} {:<22} {:<32} {:<22} GRANTED_BY",
                "ID", "TOOL", "PATH_GLOB", "AGENT"
            );
            println!("{}", "-".repeat(100));
            for e in entries {
                let id = e.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
                let tool = e.get("tool_name").and_then(|v| v.as_str()).unwrap_or("-");
                let path = e
                    .get("path_glob")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(any)");
                let agent = e
                    .get("agent_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(any)");
                let granted_by = e.get("granted_by").and_then(|v| v.as_str()).unwrap_or("-");
                println!(
                    "{:<4} {:<22} {:<32} {:<22} {}",
                    id,
                    tool.chars().take(22).collect::<String>(),
                    path.chars().take(32).collect::<String>(),
                    agent.chars().take(22).collect::<String>(),
                    granted_by,
                );
            }
            Ok(())
        }
        KernelResponse::Error { message } => anyhow::bail!("{}", message),
        other => anyhow::bail!("Unexpected response: {:?}", other),
    }
}

async fn revoke(client: &mut BusClient, id: i64) -> anyhow::Result<()> {
    let resp = client
        .send_command(KernelCommand::RevokeApprovalPolicy { id })
        .await?;
    match resp {
        KernelResponse::ApprovalPolicyRevoked { ok } => {
            if ok {
                println!("revoked policy #{id}");
            } else {
                println!("no active policy with id {id}");
            }
            Ok(())
        }
        KernelResponse::Error { message } => anyhow::bail!("{}", message),
        other => anyhow::bail!("Unexpected response: {:?}", other),
    }
}
