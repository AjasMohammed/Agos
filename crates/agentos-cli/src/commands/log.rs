use agentos_bus::{client::BusClient, KernelCommand, KernelResponse};
use clap::Subcommand;

#[derive(Subcommand)]
pub enum LogCommands {
    /// Change the active log level at runtime (no restart required)
    SetLevel {
        /// Log level: trace | debug | info | warn | error
        /// Also accepts compound directives, e.g. "agentos=debug,agentos_kernel=trace"
        level: String,
    },
}

pub async fn handle(client: &mut BusClient, command: LogCommands) -> anyhow::Result<()> {
    match command {
        LogCommands::SetLevel { level } => {
            let response = client
                .send_command(KernelCommand::SetLogLevel {
                    level: level.clone(),
                })
                .await?;
            match response {
                KernelResponse::Success { .. } => {
                    println!("Log level updated to '{}'", level);
                }
                KernelResponse::Error { message } => {
                    anyhow::bail!("Failed to set log level: {}", message);
                }
                other => {
                    anyhow::bail!("Unexpected response: {:?}", other);
                }
            }
        }
    }
    Ok(())
}
