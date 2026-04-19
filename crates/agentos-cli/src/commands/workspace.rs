/// CLI commands for runtime workspace path management.
///
/// Workspace paths control which directories on the host filesystem agents can
/// read and write beyond the default `data_dir`. Changes take effect immediately
/// for all new tool calls — no kernel restart required.
///
/// **Note:** these changes are runtime-only. To persist them across restarts,
/// add the path to `tools.workspace.allowed_paths` in `config/default.toml`.
use agentos_bus::{BusClient, KernelCommand, KernelResponse};
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum WorkspaceCommands {
    /// Add a directory to the workspace allowlist.
    ///
    /// The path must be absolute. It is canonicalized before being stored.
    /// Adding a non-existent path is allowed so you can register directories
    /// before they are created.
    ///
    /// Example:
    ///   agentos workspace add /home/user/my-repo
    Add {
        /// Absolute path to allow.
        path: String,
    },

    /// Remove a directory from the workspace allowlist.
    ///
    /// Example:
    ///   agentos workspace remove /home/user/old-repo
    Remove {
        /// Absolute path to remove.
        path: String,
    },

    /// List all currently allowed workspace paths.
    ///
    /// Example:
    ///   agentos workspace list
    List,
}

pub async fn handle(client: &mut BusClient, command: WorkspaceCommands) -> anyhow::Result<()> {
    match command {
        WorkspaceCommands::Add { path } => {
            match client
                .send_command(KernelCommand::WorkspaceAdd { path })
                .await?
            {
                KernelResponse::Success { data } => {
                    let added = data
                        .as_ref()
                        .and_then(|d| d["path"].as_str())
                        .unwrap_or("(unknown)");
                    println!("Added: {added}");
                    println!(
                        "(runtime-only — not persisted; add to tools.workspace.allowed_paths \
                         in config/default.toml to persist)"
                    );
                }
                KernelResponse::Error { message } => anyhow::bail!("{message}"),
                other => anyhow::bail!("Unexpected response: {other:?}"),
            }
        }
        WorkspaceCommands::Remove { path } => {
            match client
                .send_command(KernelCommand::WorkspaceRemove { path: path.clone() })
                .await?
            {
                KernelResponse::Success { .. } => {
                    println!("Removed: {path}");
                    println!(
                        "(runtime-only — not persisted; remove from \
                         tools.workspace.allowed_paths in config/default.toml to persist)"
                    );
                }
                KernelResponse::Error { message } => anyhow::bail!("{message}"),
                other => anyhow::bail!("Unexpected response: {other:?}"),
            }
        }
        WorkspaceCommands::List => match client.send_command(KernelCommand::WorkspaceList).await? {
            KernelResponse::WorkspacePaths(paths) => {
                if paths.is_empty() {
                    println!("No workspace paths configured.");
                } else {
                    for path in paths {
                        println!("{path}");
                    }
                }
            }
            KernelResponse::Error { message } => anyhow::bail!("{message}"),
            other => anyhow::bail!("Unexpected response: {other:?}"),
        },
    }
    Ok(())
}
