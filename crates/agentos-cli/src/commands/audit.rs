use clap::Subcommand;
use agentos_bus::client::BusClient;
use agentos_bus::message::{KernelCommand, KernelResponse};

#[derive(Subcommand)]
pub enum AuditCommands {
    /// View recent audit log entries
    Logs {
        /// Number of recent entries to show
        #[arg(long, default_value = "50")]
        last: u32,
    },
}

pub async fn handle(client: &mut BusClient, command: AuditCommands) -> anyhow::Result<()> {
    match command {
        AuditCommands::Logs { last } => {
            let response = client.send_command(KernelCommand::GetAuditLogs { limit: last }).await?;
            match response {
                KernelResponse::AuditLogs(entries) => {
                    if entries.is_empty() {
                        println!("No audit entries.");
                    } else {
                        println!("{:<30} {:<25} {:<10} {}", "TIMESTAMP", "EVENT TYPE", "SEVERITY", "DETAILS");
                        println!("{}", "-".repeat(100));
                        for entry in entries {
                            let details_str = serde_json::to_string(&entry.details).unwrap_or_default();
                            println!("{:<30} {:<25} {:<10} {}",
                                entry.timestamp.to_rfc3339(),
                                format!("{:?}", entry.event_type),
                                format!("{:?}", entry.severity),
                                if details_str.len() > 30 { format!("{}...", &details_str[..30]) } else { details_str }
                            );
                        }
                    }
                }
                KernelResponse::Error { message } => eprintln!("❌ Error: {}", message),
                _ => eprintln!("❌ Unexpected response"),
            }
        }
    }
    Ok(())
}
