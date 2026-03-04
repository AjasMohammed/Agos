use agentos_bus::client::BusClient;
use crate::Commands;

pub mod agent;
pub mod task;
pub mod tool;
pub mod secret;
pub mod perm;
pub mod status;
pub mod audit;
pub mod role;
pub mod schedule;
pub mod bg;

pub async fn handle_command(client: &mut BusClient, command: Commands) -> anyhow::Result<()> {
    match command {
        Commands::Agent { command } => agent::handle(client, command).await,
        Commands::Task { command } => task::handle(client, command).await,
        Commands::Tool { command } => tool::handle(client, command).await,
        Commands::Secret { command } => secret::handle(client, command).await,
        Commands::Perm { command } => perm::handle(client, command).await,
        Commands::Status => status::handle(client).await,
        Commands::Audit { command } => audit::handle(client, command).await,
        Commands::Role { command } => role::handle(client, command).await,
        Commands::Schedule { command } => schedule::handle(client, command).await,
        Commands::Bg { command } => bg::handle(client, command).await,
        _ => unreachable!(),
    }
}
