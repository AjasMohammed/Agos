use agentos_bus::client::BusClient;
use agentos_bus::message::{KernelCommand, KernelResponse};

pub async fn handle(client: &mut BusClient) -> anyhow::Result<()> {
    let response = client.send_command(KernelCommand::GetStatus).await?;
    match response {
        KernelResponse::Status(status) => {
            println!("🟢 AgentOS Status");
            println!("   Uptime:           {}s", status.uptime_secs);
            println!("   Connected agents: {}", status.connected_agents);
            println!("   Active tasks:     {}", status.active_tasks);
            println!("   Installed tools:  {}", status.installed_tools);
            println!("   Audit entries:    {}", status.total_audit_entries);
        }
        KernelResponse::Error { message } => eprintln!("❌ Error: {}", message),
        _ => eprintln!("❌ Cannot reach kernel. Is it running?"),
    }
    Ok(())
}
