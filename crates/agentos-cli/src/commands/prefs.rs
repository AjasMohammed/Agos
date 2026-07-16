use agentos_bus::{KernelCommand, KernelResponse};
use clap::Subcommand;

#[derive(Subcommand)]
pub enum PrefsCommands {
    /// List pending user preference proposals.
    Review {
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Accept a proposal and write it to context memory.
    Accept { proposal_id: String },
    /// Reject a proposal.
    Reject { proposal_id: String },
    /// Show queue stats.
    Stats,
}

pub async fn handle(
    client: &mut agentos_bus::client::BusClient,
    command: PrefsCommands,
) -> anyhow::Result<()> {
    let resp = match command {
        PrefsCommands::Review { limit } => {
            client
                .send_command(KernelCommand::UserPrefsListPending { limit })
                .await?
        }
        PrefsCommands::Accept { proposal_id } => {
            client
                .send_command(KernelCommand::UserPrefsAccept { proposal_id })
                .await?
        }
        PrefsCommands::Reject { proposal_id } => {
            client
                .send_command(KernelCommand::UserPrefsReject { proposal_id })
                .await?
        }
        PrefsCommands::Stats => client.send_command(KernelCommand::UserPrefsStats).await?,
    };

    match resp {
        KernelResponse::Success { data } => {
            if let Some(v) = data {
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                println!("ok");
            }
        }
        KernelResponse::Error { message } => anyhow::bail!(message),
        other => anyhow::bail!("unexpected response: {:?}", other),
    }
    Ok(())
}
