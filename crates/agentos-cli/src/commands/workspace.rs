//! `agentos workspace` — grant, revoke, and list user filesystem workspace grants.
//!
//! A grant lets an agent (or every agent) act inside a host directory tree
//! with a chosen permission mode (`r`/`rw`/`rwx`). Without a grant, file tools
//! that target an absolute path outside `data_dir` return `PermissionDenied`.

use std::path::PathBuf;

use agentos_bus::client::BusClient;
use agentos_bus::message::{KernelCommand, KernelResponse};
use clap::Subcommand;

/// Directly-under-`$HOME` directories that are "broad" enough to deserve a
/// confirmation prompt before granting.
const BROAD_GRANT_BASENAMES: &[&str] = &[
    "Desktop",
    "Documents",
    "Downloads",
    "Pictures",
    "Music",
    "Videos",
    "Public",
];

#[derive(Subcommand)]
pub enum WorkspaceCommands {
    /// Grant an agent (or every agent) access to a host directory.
    ///
    /// Examples:
    ///   agentos workspace grant ~/project --mode rw
    ///   agentos workspace grant /tmp/work --mode rwx --agent research-bot
    Grant {
        /// Absolute path or `~/...`. Subpaths are also covered.
        path: String,
        /// Permission bits: any combination of `r`, `w`, `x` (default: `rw`).
        #[arg(long, default_value = "rw")]
        mode: String,
        /// Scope to a single agent by display name or `AgentID` UUID
        /// (default: global, applies to every agent).
        #[arg(long)]
        agent: Option<String>,
        /// Skip the broad-grant confirmation prompt for paths like `~/Desktop`.
        #[arg(long)]
        yes: bool,
    },

    /// Revoke an active grant. `--agent` must match the original scope
    /// (omit for a global grant; supply for an agent-scoped one).
    Revoke {
        path: String,
        #[arg(long)]
        agent: Option<String>,
    },

    /// List active grants. With `--agent`, show grants that apply to that
    /// agent (its own + global). Otherwise show every active grant.
    List {
        #[arg(long)]
        agent: Option<String>,
    },
}

pub async fn handle(client: &mut BusClient, command: WorkspaceCommands) -> anyhow::Result<()> {
    match command {
        WorkspaceCommands::Grant {
            path,
            mode,
            agent,
            yes,
        } => grant(client, &path, &mode, agent, yes).await,
        WorkspaceCommands::Revoke { path, agent } => revoke(client, &path, agent).await,
        WorkspaceCommands::List { agent } => list(client, agent).await,
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn expand_path(input: &str) -> PathBuf {
    if let Some(rest) = input.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    } else if input == "~" {
        if let Some(home) = home_dir() {
            return home;
        }
    }
    PathBuf::from(input)
}

fn looks_broad(path: &std::path::Path) -> bool {
    let home = match home_dir() {
        Some(h) => h,
        None => return false,
    };
    if path == home {
        return true;
    }
    let basename = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    path.parent() == Some(home.as_path()) && BROAD_GRANT_BASENAMES.contains(&basename)
}

async fn grant(
    client: &mut BusClient,
    path_raw: &str,
    mode: &str,
    agent: Option<String>,
    yes: bool,
) -> anyhow::Result<()> {
    let path = expand_path(path_raw);
    if !yes && looks_broad(&path) {
        eprintln!(
            "warning: '{}' is a broad user directory and may contain a wide mix of personal files.",
            path.display()
        );
        eprintln!("Re-run with --yes to confirm, or grant a narrower subdirectory instead.");
        anyhow::bail!("aborted: broad grant requires --yes");
    }
    let response = client
        .send_command(KernelCommand::GrantWorkspace {
            path: path.clone(),
            agent_name: agent.clone(),
            mode: mode.to_string(),
        })
        .await?;
    match response {
        KernelResponse::WorkspaceGrantCreated(g) => {
            println!(
                "granted #{} path={} agent={} mode={}",
                g.id,
                g.path.display(),
                g.agent_id
                    .as_ref()
                    .map(|a| a.to_string())
                    .unwrap_or_else(|| "*".to_string()),
                g.mode,
            );
            Ok(())
        }
        KernelResponse::Error { message } => anyhow::bail!("{}", message),
        other => anyhow::bail!("Unexpected response: {:?}", other),
    }
}

async fn revoke(
    client: &mut BusClient,
    path_raw: &str,
    agent: Option<String>,
) -> anyhow::Result<()> {
    let path = expand_path(path_raw);
    let response = client
        .send_command(KernelCommand::RevokeWorkspace {
            path: path.clone(),
            agent_name: agent,
        })
        .await?;
    match response {
        KernelResponse::WorkspaceGrantRevoked { count } => {
            if count == 0 {
                println!("no matching active grant found for {}", path.display());
            } else {
                println!("revoked {count} grant(s) for {}", path.display());
            }
            Ok(())
        }
        KernelResponse::Error { message } => anyhow::bail!("{}", message),
        other => anyhow::bail!("Unexpected response: {:?}", other),
    }
}

async fn list(client: &mut BusClient, agent: Option<String>) -> anyhow::Result<()> {
    let response = client
        .send_command(KernelCommand::ListWorkspaceGrants { agent_name: agent })
        .await?;
    match response {
        KernelResponse::WorkspaceGrantList(grants) => {
            if grants.is_empty() {
                println!("No active workspace grants.");
                println!(
                    "Grant access with: agentos workspace grant <path> [--mode rwx] [--agent <name>]"
                );
                return Ok(());
            }
            println!(
                "{:<4} {:<5} {:<14} {:<24} PATH",
                "ID", "MODE", "SOURCE", "AGENT"
            );
            println!("{}", "-".repeat(80));
            for g in grants {
                let agent_s = g
                    .agent_id
                    .as_ref()
                    .map(|a| a.to_string())
                    .unwrap_or_else(|| "*".to_string());
                let agent_short = agent_s.chars().take(22).collect::<String>();
                println!(
                    "{:<4} {:<5} {:<14} {:<24} {}",
                    g.id,
                    g.mode,
                    g.source,
                    agent_short,
                    g.path.display(),
                );
            }
            Ok(())
        }
        KernelResponse::Error { message } => anyhow::bail!("{}", message),
        other => anyhow::bail!("Unexpected response: {:?}", other),
    }
}
