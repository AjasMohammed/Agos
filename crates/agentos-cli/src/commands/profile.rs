use agentos_bus::{KernelCommand, KernelResponse};
use clap::Subcommand;

#[derive(Subcommand)]
pub enum ProfileCommands {
    /// List learned user-profile facts.
    List {
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Show a single user-profile fact.
    Show { id: String },
    /// Edit a learned user-profile fact.
    Edit {
        id: String,
        #[arg(long)]
        value: Option<String>,
        #[arg(long)]
        confidence: Option<f32>,
        #[arg(long)]
        category: Option<String>,
    },
    /// Forget a learned user-profile fact.
    Forget { id: String },
}

pub async fn handle(
    client: &mut agentos_bus::client::BusClient,
    command: ProfileCommands,
) -> anyhow::Result<()> {
    let resp = match command {
        ProfileCommands::List { limit } => {
            client
                .send_command(KernelCommand::ProfileList { limit })
                .await?
        }
        ProfileCommands::Show { id } => {
            client
                .send_command(KernelCommand::ProfileShow { id })
                .await?
        }
        ProfileCommands::Edit {
            id,
            value,
            confidence,
            category,
        } => {
            client
                .send_command(KernelCommand::ProfileEdit {
                    id,
                    value,
                    confidence,
                    category,
                })
                .await?
        }
        ProfileCommands::Forget { id } => {
            client
                .send_command(KernelCommand::ProfileForget { id })
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
