use agentos_agent_tester::feedback::FeedbackCollector;
use agentos_agent_tester::harness::TestHarness;
use agentos_agent_tester::report::ReportGenerator;
use agentos_agent_tester::scenarios;
use agentos_agent_tester::scenarios::ScenarioOutcome;
use clap::Parser;

#[derive(Parser)]
#[command(name = "agent-tester", about = "LLM-driven AgentOS test harness")]
struct Args {
    /// LLM provider: anthropic, openai, ollama, gemini, mock
    #[arg(long, default_value = "mock")]
    provider: String,

    /// Model name (e.g. claude-sonnet-4-6, gpt-4o, llama3.2)
    #[arg(long, default_value = "mock-model")]
    model: String,

    /// API key (or set AGENTOS_TEST_API_KEY env var)
    #[arg(long, env = "AGENTOS_TEST_API_KEY")]
    api_key: Option<String>,

    /// Comma-separated scenario names to run (default: all)
    #[arg(long)]
    scenarios: Option<String>,

    /// Output directory for reports
    #[arg(long, default_value = "reports")]
    output_dir: String,

    /// Maximum turns per scenario
    #[arg(long, default_value = "10")]
    max_turns: usize,

    /// Number of runs per scenario (for consensus). Default 3 per design.
    #[arg(long, default_value = "3")]
    runs: usize,

    /// Also write a JSON report alongside the markdown report
    #[arg(long)]
    json: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    // Reject path traversal in the output directory per project security convention.
    if args.output_dir.contains("..") {
        return Err(anyhow::anyhow!(
            "Invalid --output-dir '{}': path traversal ('..') is not allowed",
            args.output_dir
        ));
    }

    // A turn budget of zero would produce zero-turn Incomplete results with no signal.
    if args.max_turns == 0 {
        return Err(anyhow::anyhow!("--max-turns must be at least 1 (got 0)"));
    }

    // At least one run is required to produce meaningful consensus data.
    if args.runs == 0 {
        return Err(anyhow::anyhow!("--runs must be at least 1 (got 0)"));
    }

    tracing::info!(provider = %args.provider, model = %args.model, "Starting agent-tester");

    let mut harness =
        TestHarness::boot(&args.provider, &args.model, args.api_key.as_deref()).await?;

    let selected = if let Some(filter) = &args.scenarios {
        let names: Vec<String> = filter.split(',').map(|s| s.trim().to_string()).collect();
        scenarios::filter_scenarios(&names, args.max_turns)
    } else {
        scenarios::builtin_scenarios(args.max_turns)
    };

    let mut collector = FeedbackCollector::new();
    let mut results = Vec::new();

    let use_mock = args.provider == "mock";

    for scenario in &selected {
        tracing::info!(scenario = %scenario.name, "Running scenario");
        for run in 0..args.runs {
            tracing::info!(scenario = %scenario.name, run = run + 1, "Run");
            let result = if use_mock {
                let mock_responses = scenarios::mock_responses_for(&scenario.name);
                harness
                    .run_scenario_with_mock(scenario, mock_responses, &mut collector)
                    .await
            } else {
                harness.run_scenario(scenario, &mut collector).await
            };
            tracing::info!(
                scenario = %result.scenario_name,
                outcome = ?result.outcome,
                turns = result.turns_used,
                "Scenario complete"
            );
            results.push(result);
        }
    }

    collector.deduplicate();
    let stats = collector.stats();
    let deduped_feedback = collector.into_entries();

    let report_md = ReportGenerator::generate(
        &results,
        &deduped_feedback,
        &stats,
        &args.provider,
        &args.model,
    );

    tokio::fs::create_dir_all(&args.output_dir).await?;

    let timestamp = chrono::Utc::now().format("%Y-%m-%d-%H%M%S");
    let report_path = format!("{}/agent-test-{}.md", args.output_dir, timestamp);
    tokio::fs::write(&report_path, &report_md).await?;
    tracing::info!(path = %report_path, "Report written");

    if args.json {
        let json_report = ReportGenerator::generate_json(
            &results,
            &deduped_feedback,
            &stats,
            &args.provider,
            &args.model,
        )?;
        let json_path = format!("{}/agent-test-{}.json", args.output_dir, timestamp);
        tokio::fs::write(&json_path, &json_report).await?;
        tracing::info!(path = %json_path, "JSON report written");
    }

    let complete_count = results
        .iter()
        .filter(|r| r.outcome == ScenarioOutcome::Complete)
        .count();
    let incomplete_count = results
        .iter()
        .filter(|r| r.outcome == ScenarioOutcome::Incomplete)
        .count();
    let errored_count = results
        .iter()
        .filter(|r| r.outcome == ScenarioOutcome::Errored)
        .count();

    println!("\n{}", "=".repeat(60));
    println!("AgentOS LLM Agent Test Report");
    println!("{}", "=".repeat(60));
    println!("Provider: {} | Model: {}", args.provider, args.model);
    println!(
        "Scenarios: {} | Complete: {} | Incomplete: {} | Errored: {}",
        results.len(),
        complete_count,
        incomplete_count,
        errored_count,
    );
    println!(
        "Feedback: {} total ({} errors, {} warnings)",
        stats.total_entries,
        stats
            .by_severity
            .get(&agentos_agent_tester::feedback::FeedbackSeverity::Error)
            .copied()
            .unwrap_or(0),
        stats
            .by_severity
            .get(&agentos_agent_tester::feedback::FeedbackSeverity::Warning)
            .copied()
            .unwrap_or(0),
    );
    println!("Report: {}", report_path);
    println!("{}", "=".repeat(60));

    harness.shutdown().await;
    Ok(())
}
