use agentos_bus::{KernelCommand, KernelResponse};
use clap::Subcommand;

#[derive(Subcommand)]
pub enum RecommendationsCommands {
    /// List recent proactive recommendations.
    List {
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Accept a recommendation and boost the originating interest.
    Accept { id: String },
    /// Dismiss a recommendation and lower the originating interest weight.
    Dismiss { id: String },
}

pub async fn handle(
    client: &mut agentos_bus::client::BusClient,
    command: RecommendationsCommands,
) -> anyhow::Result<()> {
    let resp = match command {
        RecommendationsCommands::List { limit } => {
            client
                .send_command(KernelCommand::RecommendationList { limit })
                .await?
        }
        RecommendationsCommands::Accept { id } => {
            client
                .send_command(KernelCommand::RecommendationAccept { id })
                .await?
        }
        RecommendationsCommands::Dismiss { id } => {
            client
                .send_command(KernelCommand::RecommendationDismiss { id })
                .await?
        }
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
