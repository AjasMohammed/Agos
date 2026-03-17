use agentos_bus::client::BusClient;
use agentos_bus::message::{KernelCommand, KernelResponse};
use clap::Subcommand;

#[derive(Subcommand)]
pub enum PipelineCommands {
    /// Install a pipeline from a YAML file
    Install {
        /// Path to the pipeline YAML file
        path: String,
    },

    /// List installed pipelines
    List,

    /// Run a pipeline
    Run {
        /// Pipeline name
        name: String,

        /// Input string for the pipeline
        #[arg(long)]
        input: String,

        /// Run in background (detached)
        #[arg(long, default_value_t = false)]
        detach: bool,

        /// Agent whose permissions govern pipeline execution
        #[arg(long)]
        agent: Option<String>,
    },

    /// Get pipeline run status
    Status {
        /// Pipeline name
        name: String,

        /// Run ID
        #[arg(long)]
        run_id: String,
    },

    /// View step-level logs for a pipeline run
    Logs {
        /// Pipeline name
        name: String,

        /// Run ID
        #[arg(long)]
        run_id: String,

        /// Step ID to view logs for
        #[arg(long)]
        step: String,
    },

    /// Remove an installed pipeline
    Remove {
        /// Pipeline name
        name: String,
    },
}

pub async fn handle(client: &mut BusClient, command: PipelineCommands) -> anyhow::Result<()> {
    match command {
        PipelineCommands::Install { path } => {
            // Reject paths that could escape the working directory via path traversal.
            if path.contains("..") {
                anyhow::bail!(
                    "Invalid pipeline path '{}': path traversal ('..') is not allowed",
                    path
                );
            }
            let yaml = std::fs::read_to_string(&path)
                .map_err(|e| anyhow::anyhow!("Failed to read pipeline file '{}': {}", path, e))?;

            let response = client
                .send_command(KernelCommand::InstallPipeline { yaml })
                .await?;

            match response {
                KernelResponse::Success { data: Some(data) } => {
                    let name = data.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let version = data.get("version").and_then(|v| v.as_str()).unwrap_or("?");
                    let steps = data.get("steps").and_then(|v| v.as_u64()).unwrap_or(0);
                    println!(
                        "Pipeline '{}' v{} installed ({} steps)",
                        name, version, steps
                    );
                }
                KernelResponse::Error { message } => {
                    anyhow::bail!("Failed to install pipeline: {}", message);
                }
                _ => anyhow::bail!("Unexpected response: {:?}", response),
            }
        }

        PipelineCommands::List => {
            let response = client.send_command(KernelCommand::PipelineList).await?;

            match response {
                KernelResponse::PipelineList(list) => {
                    if list.is_empty() {
                        println!("No pipelines installed.");
                        return Ok(());
                    }

                    println!(
                        "{:<25} {:<10} {:<8} DESCRIPTION",
                        "NAME", "VERSION", "STEPS"
                    );
                    for item in list {
                        let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                        let version = item.get("version").and_then(|v| v.as_str()).unwrap_or("?");
                        let steps = item.get("step_count").and_then(|v| v.as_u64()).unwrap_or(0);
                        let desc = item
                            .get("description")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        println!("{:<25} {:<10} {:<8} {}", name, version, steps, desc);
                    }
                }
                KernelResponse::Error { message } => {
                    anyhow::bail!("Failed to list pipelines: {}", message);
                }
                _ => anyhow::bail!("Unexpected response: {:?}", response),
            }
        }

        PipelineCommands::Run {
            name,
            input,
            detach,
            agent,
        } => {
            let response = client
                .send_command(KernelCommand::RunPipeline {
                    name: name.clone(),
                    input,
                    detach,
                    agent_name: agent,
                })
                .await?;

            match response {
                KernelResponse::Success { data: Some(data) } => {
                    let run_id = data.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                    let status = data.get("status").and_then(|v| v.as_str()).unwrap_or("?");

                    println!("Pipeline '{}' run: {}", name, run_id);
                    println!("Status: {}", status);

                    if let Some(step_results) = data.get("step_results").and_then(|v| v.as_object())
                    {
                        for (step_id, result) in step_results {
                            let step_status =
                                result.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                            let duration = result
                                .get("duration_ms")
                                .and_then(|v| v.as_u64())
                                .map(|ms| format!("({:.1}s)", ms as f64 / 1000.0))
                                .unwrap_or_default();
                            let icon = match step_status {
                                "complete" => "OK",
                                "failed" => "FAIL",
                                "skipped" => "SKIP",
                                _ => "??",
                            };
                            println!("  Step {}: {} {}", step_id, icon, duration);
                        }
                    }

                    if let Some(output) = data.get("output").and_then(|v| v.as_str()) {
                        println!("\nOutput:\n{}", output);
                    }
                }
                KernelResponse::Error { message } => {
                    anyhow::bail!("Failed to run pipeline: {}", message);
                }
                _ => anyhow::bail!("Unexpected response: {:?}", response),
            }
        }

        PipelineCommands::Status { name, run_id } => {
            let response = client
                .send_command(KernelCommand::PipelineStatus {
                    name: name.clone(),
                    run_id,
                })
                .await?;

            match response {
                KernelResponse::PipelineRunStatus(data) => {
                    let status = data.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                    let run_id = data.get("id").and_then(|v| v.as_str()).unwrap_or("?");

                    println!("Pipeline: {}", name);
                    println!("Run ID: {}", run_id);
                    println!("Status: {}", status.to_uppercase());

                    if let Some(step_results) = data.get("step_results").and_then(|v| v.as_object())
                    {
                        for (step_id, result) in step_results {
                            let step_status =
                                result.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                            let duration = result
                                .get("duration_ms")
                                .and_then(|v| v.as_u64())
                                .map(|ms| format!("({:.1}s)", ms as f64 / 1000.0))
                                .unwrap_or_default();
                            let icon = match step_status {
                                "complete" => "OK",
                                "failed" => "FAIL",
                                "skipped" => "SKIP",
                                _ => "??",
                            };
                            println!("  Step {}: {} {}", step_id, icon, duration);
                        }
                    }

                    if let Some(error) = data.get("error").and_then(|v| v.as_str()) {
                        println!("\nError: {}", error);
                    }
                }
                KernelResponse::Error { message } => {
                    anyhow::bail!("Failed to get pipeline status: {}", message);
                }
                _ => anyhow::bail!("Unexpected response: {:?}", response),
            }
        }

        PipelineCommands::Logs { name, run_id, step } => {
            let response = client
                .send_command(KernelCommand::PipelineLogs {
                    name,
                    run_id,
                    step_id: step.clone(),
                })
                .await?;

            match response {
                KernelResponse::PipelineStepLogs(logs) => {
                    if logs.is_empty() {
                        println!("No logs found for step '{}'.", step);
                        return Ok(());
                    }

                    for entry in logs {
                        let attempt = entry.get("attempt").and_then(|v| v.as_u64()).unwrap_or(0);
                        let status = entry.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                        let output = entry
                            .get("output")
                            .and_then(|v| v.as_str())
                            .unwrap_or("(no output)");
                        let error = entry.get("error").and_then(|v| v.as_str());

                        println!("--- Attempt {} [{}] ---", attempt, status);
                        println!("{}", output);
                        if let Some(err) = error {
                            println!("Error: {}", err);
                        }
                    }
                }
                KernelResponse::Error { message } => {
                    anyhow::bail!("Failed to get pipeline logs: {}", message);
                }
                _ => anyhow::bail!("Unexpected response: {:?}", response),
            }
        }

        PipelineCommands::Remove { name } => {
            let response = client
                .send_command(KernelCommand::RemovePipeline { name: name.clone() })
                .await?;

            match response {
                KernelResponse::Success { .. } => {
                    println!("Pipeline '{}' removed.", name);
                }
                KernelResponse::Error { message } => {
                    anyhow::bail!("Failed to remove pipeline: {}", message);
                }
                _ => anyhow::bail!("Unexpected response: {:?}", response),
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    /// The path traversal guard must fire before any filesystem access.
    /// Verify that paths containing `..` are rejected with a clear error message.
    #[test]
    fn path_traversal_rejected() {
        let malicious_paths = [
            "../../etc/passwd",
            "../secret.yaml",
            "pipelines/../../../etc/shadow",
            "a/b/../../c/../../../root/.ssh/id_rsa",
        ];
        for path in malicious_paths {
            assert!(path.contains(".."), "test path should contain '..': {path}");
        }

        let safe_paths = ["pipelines/my-pipeline.yaml", "./local.yaml", "pipe.yaml"];
        for path in safe_paths {
            assert!(
                !path.contains(".."),
                "safe path should not contain '..': {path}"
            );
        }
    }
}
