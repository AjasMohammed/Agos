use agentos_bus::client::BusClient;
use agentos_bus::message::{KernelCommand, KernelResponse};
use clap::Subcommand;

#[derive(Subcommand)]
pub enum RoleCommands {
    /// Create a new role
    Create {
        /// Name of the role
        name: String,
        /// Description of the role (optional positional)
        description: Option<String>,
    },
    /// Delete a role
    Delete {
        /// Name of the role to delete
        name: String,
    },
    /// List all roles
    List,
    /// Grant a permission to a role
    Grant {
        /// Role name
        role: String,
        /// Permission string (e.g., fs.user_data:rw)
        permission: String,
    },
    /// Revoke a permission from a role
    Revoke {
        /// Role name
        role: String,
        /// Permission string (e.g., fs.user_data:rw)
        permission: String,
    },
    /// Assign a role to an agent
    Assign {
        /// Agent name
        agent: String,
        /// Role name
        role: String,
    },
    /// Remove a role from an agent
    Remove {
        /// Agent name
        agent: String,
        /// Role name
        role: String,
    },
}

pub async fn handle(client: &mut BusClient, command: RoleCommands) -> anyhow::Result<()> {
    let cmd = match command {
        RoleCommands::Create { name, description } => KernelCommand::CreateRole {
            role_name: name,
            description: description.unwrap_or_default(),
        },
        RoleCommands::Delete { name } => KernelCommand::DeleteRole { role_name: name },
        RoleCommands::List => KernelCommand::ListRoles,
        RoleCommands::Grant { role, permission } => KernelCommand::RoleGrant {
            role_name: role,
            permission,
        },
        RoleCommands::Revoke { role, permission } => KernelCommand::RoleRevoke {
            role_name: role,
            permission,
        },
        RoleCommands::Assign { agent, role } => KernelCommand::AssignRole {
            agent_name: agent,
            role_name: role,
        },
        RoleCommands::Remove { agent, role } => KernelCommand::RemoveRole {
            agent_name: agent,
            role_name: role,
        },
    };

    let response = client.send_command(cmd.clone()).await?;

    match response {
        KernelResponse::Success { .. } => match cmd {
            KernelCommand::CreateRole { role_name, .. } => {
                println!("✅ Role '{}' created", role_name)
            }
            KernelCommand::DeleteRole { role_name } => println!("✅ Role '{}' deleted", role_name),
            KernelCommand::RoleGrant {
                role_name,
                permission,
            } => println!("✅ Granted '{}' to role '{}'", permission, role_name),
            KernelCommand::RoleRevoke {
                role_name,
                permission,
            } => println!("✅ Revoked '{}' from role '{}'", permission, role_name),
            KernelCommand::AssignRole {
                agent_name,
                role_name,
            } => println!("✅ Assigned role '{}' to agent '{}'", role_name, agent_name),
            KernelCommand::RemoveRole {
                agent_name,
                role_name,
            } => println!(
                "✅ Removed role '{}' from agent '{}'",
                role_name, agent_name
            ),
            _ => {}
        },
        KernelResponse::RoleList(roles) => {
            if roles.is_empty() {
                println!("No roles found.");
            } else {
                println!("{:<20} {:<30} PERMISSIONS", "NAME", "DESCRIPTION");
                println!("{}", "-".repeat(80));
                for r in roles {
                    let perms = r
                        .permissions
                        .entries()
                        .iter()
                        .map(|p| {
                            let mut flg = String::new();
                            if p.read {
                                flg.push('r');
                            }
                            if p.write {
                                flg.push('w');
                            }
                            if p.execute {
                                flg.push('x');
                            }
                            format!("{}:{}", p.resource, flg)
                        })
                        .collect::<Vec<String>>()
                        .join(", ");
                    let p_str = if perms.is_empty() { "none" } else { &perms };
                    println!("{:<20} {:<30} {}", r.name, r.description, p_str);
                }
            }
        }
        KernelResponse::Error { message } => eprintln!("❌ Error: {}", message),
        _ => eprintln!("❌ Unexpected response"),
    }

    Ok(())
}
