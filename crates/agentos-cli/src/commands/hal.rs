use agentos_bus::client::BusClient;
use agentos_bus::message::{KernelCommand, KernelResponse};
use clap::Subcommand;

#[derive(Subcommand)]
pub enum HalCommands {
    /// List all registered hardware devices and their status
    List,

    /// Register a new device (places it in quarantine pending approval)
    Register {
        /// Device ID (e.g. gpu:0, usb:1, cam:0)
        #[arg(long)]
        id: String,
        /// Human-readable device type (e.g. "nvidia-rtx-4090", "webcam")
        #[arg(long = "type")]
        device_type: String,
    },

    /// Approve a quarantined device for a specific agent
    Approve {
        /// Device ID to approve
        device: String,
        /// Agent name to grant access to
        #[arg(long)]
        agent: String,
    },

    /// Quarantine a device and deny access for all agents
    Deny {
        /// Device ID to deny
        device: String,
    },

    /// Revoke a specific agent's access to a device
    Revoke {
        /// Device ID
        device: String,
        /// Agent name to revoke access from
        #[arg(long)]
        agent: String,
    },
}

pub async fn handle(client: &mut BusClient, cmd: HalCommands) -> anyhow::Result<()> {
    match cmd {
        HalCommands::List => {
            let response = client.send_command(KernelCommand::HalListDevices).await?;
            match response {
                KernelResponse::HalDeviceList(devices) => {
                    if devices.is_empty() {
                        println!("No hardware devices registered.");
                        return Ok(());
                    }
                    println!(
                        "{:<20} {:<20} {:<12} Granted To",
                        "Device ID", "Type", "Status"
                    );
                    println!("{}", "-".repeat(74));
                    for d in &devices {
                        let id = d.get("id").and_then(|v| v.as_str()).unwrap_or("-");
                        let dtype = d.get("device_type").and_then(|v| v.as_str()).unwrap_or("-");
                        let status = d.get("status").and_then(|v| v.as_str()).unwrap_or("-");
                        let granted = d
                            .get("granted_to")
                            .and_then(|v| v.as_array())
                            .map(|a| a.len().to_string())
                            .unwrap_or_else(|| "0".to_string());
                        println!("{:<20} {:<20} {:<12} {} agents", id, dtype, status, granted);
                    }
                }
                KernelResponse::Error { message } => eprintln!("Error: {}", message),
                _ => eprintln!("Unexpected response"),
            }
        }

        HalCommands::Register { id, device_type } => {
            let response = client
                .send_command(KernelCommand::HalRegisterDevice {
                    device_id: id.clone(),
                    device_type: device_type.clone(),
                })
                .await?;
            match response {
                KernelResponse::Success { data } => {
                    if let Some(d) = &data {
                        if let Some(err) = d.get("error").and_then(|v| v.as_str()) {
                            eprintln!("Error: {}", err);
                        } else {
                            let is_new = d.get("is_new").and_then(|v| v.as_bool()).unwrap_or(false);
                            if is_new {
                                println!(
                                    "Device '{}' registered as '{}' — status: Pending.",
                                    id, device_type
                                );
                                println!("Use 'hal approve' to grant agent access.");
                            } else {
                                println!("Device '{}' already registered.", id);
                            }
                        }
                    }
                }
                KernelResponse::Error { message } => eprintln!("Error: {}", message),
                _ => eprintln!("Unexpected response"),
            }
        }

        HalCommands::Approve { device, agent } => {
            let response = client
                .send_command(KernelCommand::HalApproveDevice {
                    device_id: device.clone(),
                    agent_name: agent.clone(),
                })
                .await?;
            match response {
                KernelResponse::Success { data } => {
                    if let Some(d) = &data {
                        if let Some(err) = d.get("error").and_then(|v| v.as_str()) {
                            eprintln!("Error: {}", err);
                        } else {
                            println!("Device '{}' approved for agent '{}'.", device, agent);
                        }
                    }
                }
                KernelResponse::Error { message } => eprintln!("Error: {}", message),
                _ => eprintln!("Unexpected response"),
            }
        }

        HalCommands::Deny { device } => {
            let response = client
                .send_command(KernelCommand::HalDenyDevice {
                    device_id: device.clone(),
                })
                .await?;
            match response {
                KernelResponse::Success { data } => {
                    if let Some(d) = &data {
                        if let Some(err) = d.get("error").and_then(|v| v.as_str()) {
                            eprintln!("Error: {}", err);
                        } else {
                            println!("Device '{}' quarantined.", device);
                        }
                    }
                }
                KernelResponse::Error { message } => eprintln!("Error: {}", message),
                _ => eprintln!("Unexpected response"),
            }
        }

        HalCommands::Revoke { device, agent } => {
            let response = client
                .send_command(KernelCommand::HalRevokeDevice {
                    device_id: device.clone(),
                    agent_name: agent.clone(),
                })
                .await?;
            match response {
                KernelResponse::Success { data } => {
                    if let Some(d) = &data {
                        if let Some(err) = d.get("error").and_then(|v| v.as_str()) {
                            eprintln!("Error: {}", err);
                        } else {
                            println!("Revoked '{}' access to device '{}'.", agent, device);
                        }
                    }
                }
                KernelResponse::Error { message } => eprintln!("Error: {}", message),
                _ => eprintln!("Unexpected response"),
            }
        }
    }
    Ok(())
}
