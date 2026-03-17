use agentos_kernel::{load_config, Kernel};
use agentos_vault::ZeroizingString;
use agentos_web::WebServer;
use clap::Subcommand;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[derive(Subcommand, Debug)]
pub enum WebCommands {
    /// Start the web UI server
    Serve {
        /// Port to bind the web server on
        #[arg(long, default_value = "8080")]
        port: u16,

        /// Host/IP to bind on
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        // vault_passphrase argument REMOVED -- use AGENTOS_VAULT_PASSPHRASE env var or interactive prompt
    },
}

pub async fn handle_serve(config_path: &Path, host: &str, port: u16) -> anyhow::Result<()> {
    let passphrase = ZeroizingString::new(match std::env::var("AGENTOS_VAULT_PASSPHRASE") {
        Ok(env_pass) if !env_pass.is_empty() => env_pass,
        _ => {
            eprint!("Enter vault passphrase: ");
            rpassword::read_password()?
        }
    });

    let config = load_config(config_path)?;
    // Canonicalize at startup so handler comparisons are O(1) in-memory with no blocking I/O.
    let allowed_tool_dirs: Arc<Vec<PathBuf>> = Arc::new(
        [
            PathBuf::from(&config.tools.core_tools_dir),
            PathBuf::from(&config.tools.user_tools_dir),
        ]
        .into_iter()
        .filter_map(|p| match std::fs::canonicalize(&p) {
            Ok(resolved) => Some(resolved),
            Err(e) => {
                tracing::warn!(path = %p.display(), error = %e, "Could not resolve tool directory; skipping");
                None
            }
        })
        .collect(),
    );
    anyhow::ensure!(
        !allowed_tool_dirs.is_empty(),
        "No tool directories could be resolved — check [tools] config"
    );

    let kernel = Arc::new(Kernel::boot(config_path, &passphrase).await?);

    let addr: SocketAddr = format!("{}:{}", host, port).parse()?;

    println!("Web UI: http://{}", addr);
    println!("Press Ctrl+C to shutdown.");

    let server = WebServer::new(addr, kernel.clone(), Arc::clone(&allowed_tool_dirs))?;

    let shutdown_token = CancellationToken::new();

    // Spawn kernel run loop
    let kernel_handle = {
        let kernel = kernel.clone();
        let token = shutdown_token.clone();
        tokio::spawn(async move {
            tokio::select! {
                result = kernel.clone().run() => {
                    if let Err(e) = result {
                        tracing::error!(error = %e, "Kernel exited with error");
                    }
                    token.cancel();
                }
                _ = token.cancelled() => {
                    tracing::info!("Kernel received shutdown signal");
                    // Signal kernel's internal cancellation before the run future is dropped
                    kernel.shutdown();
                }
            }
        })
    };

    // Spawn web server with graceful shutdown
    let server_handle = {
        let token = shutdown_token.clone();
        tokio::spawn(async move {
            if let Err(e) = server.start_with_shutdown(token.clone()).await {
                tracing::error!(error = %e, "Web server exited with error");
            }
            token.cancel();
        })
    };

    // Wait for Ctrl+C or either task to finish
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("Ctrl+C received, shutting down...");
            kernel.shutdown();
            shutdown_token.cancel();
        }
        _ = shutdown_token.cancelled() => {
            tracing::info!("Component exited, shutting down...");
            kernel.shutdown();
        }
    }

    // Wait for both to finish cleanly
    let _ = tokio::join!(kernel_handle, server_handle);

    Ok(())
}
