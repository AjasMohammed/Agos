use agentos_bus::client::BusClient;
use agentos_bus::message::{KernelCommand, KernelResponse};
use clap::Subcommand;

#[derive(Subcommand)]
pub enum CostCommands {
    /// Show cost report for all agents or a specific agent
    Show {
        /// Agent name (omit for all agents)
        #[arg(long)]
        agent: Option<String>,
    },
    /// Show retrieval refresh/reuse efficiency metrics
    Retrieval,
}

pub async fn handle(client: &mut BusClient, cmd: CostCommands) -> anyhow::Result<()> {
    match cmd {
        CostCommands::Show { agent } => {
            let response = client
                .send_command(KernelCommand::GetCostReport { agent_name: agent })
                .await?;

            match response {
                KernelResponse::CostReport(snapshots) => {
                    if snapshots.is_empty() {
                        println!("No cost data available.");
                        return Ok(());
                    }

                    println!(
                        "{:<20} {:>12} {:>12} {:>12} {:>10} {:>10} {:>10}",
                        "Agent", "Tokens", "Cost (USD)", "Tool Calls", "Tok %", "Cost %", "Call %"
                    );
                    println!("{}", "-".repeat(88));

                    for snap in &snapshots {
                        println!(
                            "{:<20} {:>12} {:>12.6} {:>12} {:>9.1}% {:>9.1}% {:>9.1}%",
                            snap.agent_name,
                            snap.tokens_used,
                            snap.cost_usd,
                            snap.tool_calls,
                            snap.tokens_pct,
                            snap.cost_pct,
                            snap.tool_calls_pct,
                        );
                    }

                    println!("{}", "-".repeat(88));
                    let total_cost: f64 = snapshots.iter().map(|s| s.cost_usd).sum();
                    let total_tokens: u64 = snapshots.iter().map(|s| s.tokens_used).sum();
                    let total_calls: u64 = snapshots.iter().map(|s| s.tool_calls).sum();
                    println!(
                        "{:<20} {:>12} {:>12.6} {:>12}",
                        "TOTAL", total_tokens, total_cost, total_calls
                    );
                }
                KernelResponse::Error { message } => {
                    eprintln!("Error: {}", message);
                }
                _ => {
                    eprintln!("Unexpected response");
                }
            }
        }
        CostCommands::Retrieval => {
            let response = client
                .send_command(KernelCommand::GetRetrievalMetrics)
                .await?;
            match response {
                KernelResponse::Success { data: Some(data) } => {
                    let refresh_total = data
                        .get("refresh_total")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let reuse_total = data
                        .get("reuse_total")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let total_decisions = data
                        .get("total_decisions")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let refresh_ratio = data
                        .get("refresh_ratio")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    let reuse_ratio = data
                        .get("reuse_ratio")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);

                    println!("Retrieval Refresh Efficiency");
                    println!("  Refresh decisions: {}", refresh_total);
                    println!("  Reuse decisions:   {}", reuse_total);
                    println!("  Total decisions:   {}", total_decisions);
                    println!("  Refresh ratio:     {:.2}%", refresh_ratio * 100.0);
                    println!("  Reuse ratio:       {:.2}%", reuse_ratio * 100.0);
                }
                KernelResponse::Error { message } => {
                    eprintln!("Error: {}", message);
                }
                _ => {
                    eprintln!("Unexpected response");
                }
            }
        }
    }
    Ok(())
}
