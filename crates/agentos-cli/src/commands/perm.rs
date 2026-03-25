use agentos_bus::client::BusClient;
use agentos_bus::message::{KernelCommand, KernelResponse};
use clap::Subcommand;

#[derive(Subcommand)]
pub enum PermCommands {
    /// Grant a permission to an agent
    Grant {
        /// Agent name
        agent: String,
        /// Permission string (e.g. "fs.user_data:rw")
        permission: String,
        /// Expiration time in seconds
        #[arg(long)]
        expires: Option<u64>,
    },
    /// Revoke a permission from an agent
    Revoke {
        /// Agent name
        agent: String,
        /// Permission string
        permission: String,
    },
    /// Show all permissions for an agent
    #[command(alias = "list")]
    Show {
        /// Agent name
        agent: String,
    },
    /// Manage permission profiles
    Profile {
        #[command(subcommand)]
        command: PermProfileCommands,
    },
}

#[derive(Subcommand)]
pub enum PermProfileCommands {
    /// Create a new permission profile
    Create {
        name: String,
        description: String,
        permissions: Vec<String>,
    },
    /// Delete a permission profile
    Delete { name: String },
    /// List all permission profiles
    List,
    /// Assign a permission profile to an agent
    Assign {
        agent_name: String,
        profile_name: String,
    },
}

pub async fn handle(client: &mut BusClient, command: PermCommands) -> anyhow::Result<()> {
    match command {
        PermCommands::Grant {
            agent,
            permission,
            expires,
        } => {
            let response = if let Some(secs) = expires {
                client
                    .send_command(KernelCommand::GrantPermissionTimed {
                        agent_name: agent.clone(),
                        permission: permission.clone(),
                        expires_secs: secs,
                    })
                    .await?
            } else {
                client
                    .send_command(KernelCommand::GrantPermission {
                        agent_name: agent.clone(),
                        permission: permission.clone(),
                    })
                    .await?
            };
            match response {
                KernelResponse::Success { .. } => println!(
                    "✅ Granted permission '{}' to agent '{}'",
                    permission, agent
                ),
                KernelResponse::Error { message } => eprintln!("❌ Error: {}", message),
                _ => eprintln!("❌ Unexpected response"),
            }
        }
        PermCommands::Revoke { agent, permission } => {
            let response = client
                .send_command(KernelCommand::RevokePermission {
                    agent_name: agent.clone(),
                    permission: permission.clone(),
                })
                .await?;
            match response {
                KernelResponse::Success { .. } => println!(
                    "✅ Revoked permission '{}' from agent '{}'",
                    permission, agent
                ),
                KernelResponse::Error { message } => eprintln!("❌ Error: {}", message),
                _ => eprintln!("❌ Unexpected response"),
            }
        }
        PermCommands::Show { agent } => {
            let response = client
                .send_command(KernelCommand::ShowPermissions {
                    agent_name: agent.clone(),
                })
                .await?;
            match response {
                KernelResponse::Permissions(perms) => {
                    println!("Permissions for agent '{}':", agent);
                    for p in perms.entries.iter() {
                        let r = if p.read { "r" } else { "-" };
                        let w = if p.write { "w" } else { "-" };
                        let x = if p.execute { "x" } else { "-" };
                        println!(" - {} [{}{}{}]", p.resource, r, w, x);
                    }
                    if perms.entries.is_empty() {
                        println!(" (None)");
                    }
                }
                KernelResponse::Error { message } => eprintln!("❌ Error: {}", message),
                _ => eprintln!("❌ Unexpected response"),
            }
        }
        PermCommands::Profile { command } => match command {
            PermProfileCommands::Create {
                name,
                description,
                permissions,
            } => {
                let response = client
                    .send_command(KernelCommand::CreatePermProfile {
                        name: name.clone(),
                        description,
                        permissions,
                    })
                    .await?;
                match response {
                    KernelResponse::Success { .. } => println!("✅ Created profile '{}'", name),
                    KernelResponse::Error { message } => eprintln!("❌ Error: {}", message),
                    _ => eprintln!("❌ Unexpected response"),
                }
            }
            PermProfileCommands::Delete { name } => {
                let response = client
                    .send_command(KernelCommand::DeletePermProfile { name: name.clone() })
                    .await?;
                match response {
                    KernelResponse::Success { .. } => println!("✅ Deleted profile '{}'", name),
                    KernelResponse::Error { message } => eprintln!("❌ Error: {}", message),
                    _ => eprintln!("❌ Unexpected response"),
                }
            }
            PermProfileCommands::List => {
                let response = client.send_command(KernelCommand::ListPermProfiles).await?;
                match response {
                    KernelResponse::PermProfileList(profiles) => {
                        println!("Permission Profiles:");
                        for p in &profiles {
                            println!("- {} ({})", p.name, p.description);
                        }
                        if profiles.is_empty() {
                            println!(" (None)");
                        }
                    }
                    KernelResponse::Error { message } => eprintln!("❌ Error: {}", message),
                    _ => eprintln!("❌ Unexpected response"),
                }
            }
            PermProfileCommands::Assign {
                agent_name,
                profile_name,
            } => {
                let response = client
                    .send_command(KernelCommand::AssignPermProfile {
                        agent_name: agent_name.clone(),
                        profile_name: profile_name.clone(),
                    })
                    .await?;
                match response {
                    KernelResponse::Success { .. } => println!(
                        "✅ Assigned profile '{}' to agent '{}'",
                        profile_name, agent_name
                    ),
                    KernelResponse::Error { message } => eprintln!("❌ Error: {}", message),
                    _ => eprintln!("❌ Unexpected response"),
                }
            }
        },
    }
    Ok(())
}
