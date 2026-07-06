/// `agentos mcp` — MCP (Model Context Protocol) adapter commands.
///
/// Subcommands:
///   `serve`  — Expose registered AgentOS tools as an MCP server on stdio.
///              Intended for use with Claude Desktop, Cursor, or any MCP client.
///   `list`   — List MCP server connections defined in the current config.
///   `status` — Show live connection health for all configured MCP servers.
use std::sync::Arc;

use agentos_bus::{BusClient, KernelCommand, KernelResponse};
use agentos_mcp::{
    A2AClient, AgentCard, AuthRequirement, McpAuthValidator, McpServer, McpToolDef, McpToolExecutor,
};
use agentos_tools::runner::ToolRunner;
use async_trait::async_trait;
use clap::Subcommand;
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

#[derive(Debug, Subcommand)]
pub enum McpCommands {
    /// Expose all registered AgentOS tools as an MCP server.
    ///
    /// Default transport is stdio (reads stdin, writes stdout) for use with
    /// Claude Desktop, Cursor, and similar local MCP clients.
    ///
    /// Use `--transport http` to expose via HTTP POST for remote clients.
    ///
    /// Examples:
    ///   # stdio (default) — pipe from Claude Desktop config
    ///   agentos mcp serve
    ///
    ///   # HTTP — listen on port 3002 with bearer-token auth
    ///   agentos mcp serve --transport http --port 3002 --token mysecret
    Serve {
        /// Transport mode: "stdio" (default) or "http"
        #[arg(long, default_value = "stdio")]
        transport: String,

        /// Port to listen on (HTTP transport only, default: 3002)
        #[arg(long, default_value_t = 3002)]
        port: u16,

        /// Bearer token required for HTTP clients (HTTP transport only).
        /// REQUIRED for `--transport http` (may also be set via AGENTOS_MCP_TOKEN);
        /// the server refuses to start the HTTP transport without it. Ignored for
        /// stdio.
        #[arg(long)]
        token: Option<String>,
    },

    /// List available MCP tools (requires no kernel connection).
    ///
    /// Loads the tool runner from config and prints all available tool names.
    Tools,

    /// Call a single MCP tool and print the result.
    ///
    /// Example:
    ///   agentos mcp call --tool file-reader --input '{"path": "notes.txt"}'
    Call {
        /// Name of the tool to invoke
        #[arg(long)]
        tool: String,

        /// JSON input for the tool (defaults to empty object)
        #[arg(long, default_value = "{}")]
        input: String,
    },

    /// List MCP server connections configured in the kernel config file.
    List,

    /// Show live connection health for all configured MCP servers.
    ///
    /// Requires a running kernel. Reports each server's name, connection
    /// state, registered tool count, and last error (if any).
    Status,

    /// Attach an MCP server to the running kernel at runtime.
    ///
    /// Spawns the server process (stdio) or opens an HTTP connection, performs
    /// the MCP handshake, and registers its tools immediately — no restart needed.
    ///
    /// The attachment is persisted to SQLite and automatically restored on
    /// kernel restart. Use `mcp detach` to remove it permanently.
    ///
    /// Examples:
    ///   agentos mcp attach filesystem -- npx -y @modelcontextprotocol/server-filesystem /tmp
    ///   agentos mcp attach github --env GITHUB_TOKEN=vault:github_token -- npx -y @modelcontextprotocol/server-github
    ///   agentos mcp attach remote --url http://localhost:8080/mcp --token mytoken
    ///   agentos mcp attach zomato --url https://mcp-server.zomato.com/mcp --oauth-connector zomato
    Attach {
        /// Unique name for this server (used in logs, status, and detach).
        name: String,

        /// HTTP endpoint URL (for HTTP transport). Mutually exclusive with trailing command.
        #[arg(long)]
        url: Option<String>,

        /// Static Bearer auth token for HTTP transport.
        /// Mutually exclusive with `--oauth-connector`.
        #[arg(long)]
        token: Option<String>,

        /// OAuth2 connector ID referencing a credential stored via `mcp oauth-store`.
        /// Enables automatic token refresh and retry on 401.
        /// Mutually exclusive with `--token`.
        #[arg(long, value_name = "CONNECTOR_ID")]
        oauth_connector: Option<String>,

        /// Per-request timeout in seconds (default: 30).
        #[arg(long)]
        timeout: Option<u64>,

        /// Environment variable for the subprocess in KEY=VALUE format.
        ///
        /// Use `vault:SECRET_NAME` as the value to read from the kernel vault:
        ///   --env GITHUB_TOKEN=vault:github_token
        ///
        /// Can be repeated for multiple variables:
        ///   --env FOO=bar --env BAZ=vault:my_secret
        #[arg(long = "env", value_name = "KEY=VALUE")]
        env_vars: Vec<String>,

        /// Command and arguments for stdio transport (everything after `--`).
        ///
        /// Example: `-- npx -y @modelcontextprotocol/server-filesystem /tmp`
        #[arg(last = true)]
        command_and_args: Vec<String>,
    },

    /// Store an OAuth2 credential in the vault for MCP server authentication.
    ///
    /// The credential is encrypted at rest (AES-256-GCM) and referenced by
    /// `--oauth-connector` in `mcp attach`. Token refresh is handled automatically.
    ///
    /// Examples:
    ///   # Store a Zomato OAuth credential (obtain the initial token via Claude Desktop or browser)
    ///   agentos mcp oauth-store zomato \
    ///     --provider zomato \
    ///     --access-token "eyJ..." \
    ///     --refresh-token "dGhp..." \
    ///     --token-endpoint "https://accounts.zomato.com/oauth/token" \
    ///     --client-id "myapp_client_id" \
    ///     --client-secret "myapp_secret" \
    ///     --scopes "order:read,order:write" \
    ///     --expires-in 3600
    OauthStore {
        /// Unique identifier for this credential (e.g. "zomato", "github").
        /// Used in `mcp attach --oauth-connector <ID>`.
        connector_id: String,

        /// Human-readable provider name (e.g. "zomato", "github").
        #[arg(long, default_value = "custom")]
        provider: String,

        /// OAuth2 access token obtained from the provider.
        #[arg(long)]
        access_token: String,

        /// OAuth2 refresh token (used to obtain new access tokens on expiry).
        #[arg(long)]
        refresh_token: Option<String>,

        /// OAuth2 token endpoint URL for refresh requests.
        ///
        /// Example: https://accounts.zomato.com/oauth/token
        #[arg(long)]
        token_endpoint: String,

        /// OAuth2 client ID registered with the provider.
        #[arg(long)]
        client_id: String,

        /// OAuth2 client secret (for confidential clients).
        #[arg(long)]
        client_secret: Option<String>,

        /// Comma-separated scopes granted by this token (e.g. "order:read,order:write").
        #[arg(long, value_name = "SCOPES")]
        scopes: Option<String>,

        /// Token lifetime in seconds. Used to compute when the token expires.
        /// If omitted, the token is treated as non-expiring.
        #[arg(long, value_name = "SECONDS")]
        expires_in: Option<i64>,
    },

    /// Detach an MCP server from the running kernel.
    ///
    /// Closes the connection and removes the server from the supervisor.
    /// Requires a running kernel.
    Detach {
        /// Name of the server to detach (as given to `mcp attach` or configured at boot).
        name: String,
    },

    /// Discover a remote A2A agent's capabilities (fetch its Agent Card).
    ///
    /// Example:
    ///   agentos mcp a2a-discover http://remote-agent.example.com
    A2aDiscover {
        /// Base URL of the remote agent (e.g. http://localhost:3001)
        url: String,
    },

    /// Delegate a task to a remote A2A agent.
    ///
    /// Example:
    ///   agentos mcp a2a-delegate --url http://remote --capability echo --input '{"msg":"hi"}'
    A2aDelegate {
        /// Base URL of the remote A2A agent
        #[arg(long)]
        url: String,

        /// Capability name to invoke
        #[arg(long)]
        capability: String,

        /// JSON input for the capability (default: {})
        #[arg(long, default_value = "{}")]
        input: String,

        /// Bearer token for authenticating with the remote agent
        #[arg(long)]
        token: Option<String>,
    },

    /// Show this agent's A2A card (what external agents would see).
    A2aCard,

    /// Browse the curated MCP server catalog (requires a running kernel).
    Catalog {
        #[command(subcommand)]
        command: CatalogSubcommand,
    },

    /// Install an MCP server from the catalog in one step.
    ///
    /// Examples:
    ///   agentos mcp install filesystem --yes
    ///   agentos mcp install github --yes      # seed GITHUB token first: agentos secret set github_token <PAT>
    Install {
        /// Catalog entry id (e.g. `filesystem`).
        id: String,
        /// Proceed without interactive confirmation.
        #[arg(long)]
        yes: bool,
        /// Allow installing a community-tier entry.
        #[arg(long)]
        unsafe_allow_community: bool,
        /// Use a specific runtime binary instead of auto-resolving.
        #[arg(long, value_name = "PATH")]
        runtime_binary: Option<String>,
        /// Skip auth-credential injection.
        #[arg(long)]
        no_auth: bool,
    },

    /// Uninstall (detach) a previously-installed catalog server.
    Uninstall {
        /// Catalog entry id.
        id: String,
        /// Also purge any cached package/credential artifacts.
        #[arg(long)]
        purge: bool,
    },
}

/// `mcp catalog …` subcommands.
#[derive(Debug, clap::Subcommand)]
pub enum CatalogSubcommand {
    /// List all catalog entries.
    List {
        /// Filter by trust tier (core | verified | community).
        #[arg(long, value_name = "TIER")]
        trust: Option<String>,
    },
    /// Search catalog entries by id, name, or description.
    Search {
        /// Search query.
        query: String,
    },
    /// Show full details for a single catalog entry as JSON.
    Info {
        /// Catalog entry id (e.g. `filesystem`).
        id: String,
    },
}

/// Run the requested MCP subcommand.
///
/// `serve`, `tools`, `call`, and `list` are offline commands (no bus needed).
/// `status`, `attach`, and `detach` require a running kernel and are
/// handled inline in `main.rs` where the `BusClient` is available.
pub async fn handle(command: McpCommands, config_path: &str) -> anyhow::Result<()> {
    match command {
        McpCommands::Serve {
            transport,
            port,
            token,
        } => cmd_serve(config_path, &transport, port, token).await,
        McpCommands::Tools => cmd_tools(config_path).await,
        McpCommands::Call { tool, input } => cmd_call(config_path, &tool, &input).await,
        McpCommands::List => cmd_list(config_path),
        McpCommands::A2aDiscover { url } => cmd_a2a_discover(&url).await,
        McpCommands::A2aDelegate {
            url,
            capability,
            input,
            token,
        } => cmd_a2a_delegate(&url, &capability, &input, token.as_deref()).await,
        McpCommands::A2aCard => cmd_a2a_card(config_path).await,
        McpCommands::Status
        | McpCommands::Attach { .. }
        | McpCommands::Detach { .. }
        | McpCommands::OauthStore { .. }
        | McpCommands::Catalog { .. }
        | McpCommands::Install { .. }
        | McpCommands::Uninstall { .. } => {
            anyhow::bail!("this mcp subcommand requires a running kernel")
        }
    }
}

// ── status ────────────────────────────────────────────────────────────────────

/// Query the kernel for live MCP server health and print a table.
pub async fn cmd_mcp_status(bus: &mut BusClient) -> anyhow::Result<()> {
    match bus.send_command(KernelCommand::McpStatus).await? {
        KernelResponse::McpServerStatusList(list) => {
            if list.is_empty() {
                println!("No MCP servers configured.");
                return Ok(());
            }
            println!("{:<20} {:<12} {:<8} LAST ERROR", "NAME", "STATUS", "TOOLS");
            println!("{}", "-".repeat(70));
            for s in list {
                let status = if s.connected {
                    "connected"
                } else {
                    "disconnected"
                };
                let err = s.last_error.as_deref().unwrap_or("-");
                println!("{:<20} {:<12} {:<8} {}", s.name, status, s.tool_count, err);
            }
        }
        KernelResponse::Error { message } => {
            anyhow::bail!("Kernel error: {}", message);
        }
        other => {
            anyhow::bail!("Unexpected response: {:?}", other);
        }
    }
    Ok(())
}

// ── serve ─────────────────────────────────────────────────────────────────────

/// Boot a `ToolRunner` from the config, then serve all registered tools as an
/// MCP server over the selected transport.
async fn cmd_serve(
    config_path: &str,
    transport: &str,
    port: u16,
    token: Option<String>,
) -> anyhow::Result<()> {
    let config = agentos_kernel::config::load_config(std::path::Path::new(config_path))?;
    let data_dir = std::path::PathBuf::from(&config.tools.data_dir);

    let tool_runner = Arc::new(ToolRunner::new(&data_dir).map_err(|e| anyhow::anyhow!(e))?);
    let executor = Arc::new(ToolRunnerExecutor {
        runner: tool_runner,
        data_dir,
    });

    match transport {
        "stdio" => {
            let server = McpServer::new(executor);
            eprintln!("AgentOS MCP server running on stdio.");
            server.serve_stdio().await?;
        }
        "http" => {
            // The HTTP transport exposes full tool execution (shell, filesystem,
            // network) over the network and binds 0.0.0.0. Refuse to start
            // without a bearer token — otherwise this is unauthenticated remote
            // code execution for anyone who can reach the port. The token may be
            // supplied via --token or the AGENTOS_MCP_TOKEN env var.
            let token = token.or_else(|| std::env::var("AGENTOS_MCP_TOKEN").ok());
            let token = match token {
                Some(t) if !t.trim().is_empty() => t,
                _ => anyhow::bail!(
                    "Refusing to start MCP HTTP transport without authentication. \
                     Pass --token <secret> (or set AGENTOS_MCP_TOKEN). Use --transport stdio \
                     for local, unauthenticated use."
                ),
            };
            let auth: Arc<dyn McpAuthValidator> = Arc::new(BearerTokenAuth(token));
            let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
            eprintln!(
                "AgentOS MCP HTTP server on http://0.0.0.0:{port}/mcp (bearer auth required)"
            );
            agentos_mcp::serve_http(executor, auth, addr).await?;
        }
        other => anyhow::bail!("Unknown transport '{}'. Use 'stdio' or 'http'.", other),
    }
    Ok(())
}

// ── tools ─────────────────────────────────────────────────────────────────────

/// List all available tools.
async fn cmd_tools(config_path: &str) -> anyhow::Result<()> {
    let config = agentos_kernel::config::load_config(std::path::Path::new(config_path))?;
    let data_dir = std::path::PathBuf::from(&config.tools.data_dir);
    let runner = ToolRunner::new(&data_dir).map_err(|e| anyhow::anyhow!(e))?;
    let executor = ToolRunnerExecutor {
        runner: Arc::new(runner),
        data_dir,
    };

    let tools = executor.list_tools().await;
    if tools.is_empty() {
        println!("No tools registered.");
        return Ok(());
    }
    println!("{:<30} DESCRIPTION", "TOOL");
    println!("{}", "-".repeat(70));
    for t in &tools {
        println!("{:<30} {}", t.name, t.description);
    }
    println!("\n{} tool(s) available.", tools.len());
    Ok(())
}

// ── call ──────────────────────────────────────────────────────────────────────

/// Call a single tool and print the result.
async fn cmd_call(config_path: &str, tool_name: &str, input_json: &str) -> anyhow::Result<()> {
    let config = agentos_kernel::config::load_config(std::path::Path::new(config_path))?;
    let data_dir = std::path::PathBuf::from(&config.tools.data_dir);
    let runner = Arc::new(ToolRunner::new(&data_dir).map_err(|e| anyhow::anyhow!(e))?);
    let executor = ToolRunnerExecutor { runner, data_dir };

    let args: serde_json::Value = serde_json::from_str(input_json)
        .map_err(|e| anyhow::anyhow!("Invalid JSON input: {}", e))?;

    match executor.call_tool(tool_name, args).await {
        Ok(result) => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        Err(e) => {
            eprintln!("Tool error: {}", e);
            std::process::exit(1);
        }
    }
    Ok(())
}

// ── list ──────────────────────────────────────────────────────────────────────

fn cmd_list(config_path: &str) -> anyhow::Result<()> {
    let config = agentos_kernel::config::load_config(std::path::Path::new(config_path))?;

    if config.mcp.servers.is_empty() {
        println!("No MCP servers configured.");
        println!();
        println!("To add one, edit your config file and add:");
        println!("  [[mcp.servers]]");
        println!("  name = \"filesystem\"");
        println!("  command = \"npx\"");
        println!("  args = [\"-y\", \"@modelcontextprotocol/server-filesystem\", \"/tmp\"]");
        return Ok(());
    }

    println!("{:<20} COMMAND", "NAME");
    println!("{}", "-".repeat(60));
    for srv in &config.mcp.servers {
        let transport = if let Some(ref cmd) = srv.command {
            if srv.args.is_empty() {
                cmd.clone()
            } else {
                format!("{} {}", cmd, srv.args.join(" "))
            }
        } else if let Some(ref url) = srv.url {
            url.clone()
        } else {
            "(no transport configured)".to_string()
        };
        println!("{:<20} {}", srv.name, transport);
    }
    Ok(())
}

// ── attach / detach ───────────────────────────────────────────────────────────

/// Attach an MCP server to the running kernel at runtime.
#[allow(clippy::too_many_arguments)]
pub async fn cmd_mcp_attach(
    bus: &mut BusClient,
    name: String,
    command_and_args: Vec<String>,
    url: Option<String>,
    token: Option<String>,
    oauth_connector: Option<String>,
    timeout: Option<u64>,
    env_vars: Vec<String>,
) -> anyhow::Result<()> {
    if token.is_some() && oauth_connector.is_some() {
        anyhow::bail!("--token and --oauth-connector are mutually exclusive");
    }
    if url.is_some() && !command_and_args.is_empty() {
        anyhow::bail!("--url and a trailing command are mutually exclusive — use one or the other");
    }
    if url.is_none() && command_and_args.is_empty() {
        anyhow::bail!(
            "provide either --url <URL> or a command after -- (e.g. -- npx -y @mcp/server)"
        );
    }

    let (command, args) = if !command_and_args.is_empty() {
        (
            Some(command_and_args[0].clone()),
            command_and_args[1..].to_vec(),
        )
    } else {
        (None, vec![])
    };

    // Parse KEY=VALUE env var strings.
    let mut env = std::collections::HashMap::new();
    for kv in env_vars {
        match kv.split_once('=') {
            Some((k, v)) => {
                env.insert(k.to_string(), v.to_string());
            }
            None => anyhow::bail!("--env value '{}' must be in KEY=VALUE format", kv),
        }
    }

    let cmd = KernelCommand::McpAttach {
        name: name.clone(),
        command,
        args,
        url,
        auth_token: token,
        oauth_connector_id: oauth_connector,
        timeout_secs: timeout,
        env,
    };

    match bus.send_command(cmd).await? {
        KernelResponse::McpAttached { tool_count, tools } => {
            println!("Attached '{}' — {} tool(s) registered.", name, tool_count);
            if !tools.is_empty() {
                for t in &tools {
                    println!("  + {}", t);
                }
            }
        }
        KernelResponse::Error { message } => {
            anyhow::bail!("Kernel error: {}", message);
        }
        other => {
            anyhow::bail!("Unexpected response: {:?}", other);
        }
    }
    Ok(())
}

/// Store an OAuth2 credential in the vault for MCP server authentication.
#[allow(clippy::too_many_arguments)]
pub async fn cmd_mcp_oauth_store(
    bus: &mut BusClient,
    connector_id: String,
    provider: String,
    access_token: String,
    refresh_token: Option<String>,
    token_endpoint: String,
    client_id: String,
    client_secret: Option<String>,
    scopes: Option<String>,
    expires_in: Option<i64>,
) -> anyhow::Result<()> {
    let scopes_vec: Vec<String> = scopes
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();

    let cmd = KernelCommand::McpOAuthStore {
        connector_id: connector_id.clone(),
        provider,
        access_token: Zeroizing::new(access_token),
        refresh_token: refresh_token.map(Zeroizing::new),
        token_endpoint,
        client_id,
        client_secret: client_secret.map(Zeroizing::new),
        scopes: scopes_vec,
        expires_in_secs: expires_in,
    };

    match bus.send_command(cmd).await? {
        KernelResponse::McpOAuthStored { connector_id } => {
            println!("OAuth credential '{}' stored in vault.", connector_id);
            println!(
                "Use it with: agentos mcp attach <name> --url <url> --oauth-connector {}",
                connector_id
            );
        }
        KernelResponse::Error { message } => {
            anyhow::bail!("Kernel error: {}", message);
        }
        other => {
            anyhow::bail!("Unexpected response: {:?}", other);
        }
    }
    Ok(())
}

/// Detach an MCP server from the running kernel.
pub async fn cmd_mcp_detach(bus: &mut BusClient, name: String) -> anyhow::Result<()> {
    match bus
        .send_command(KernelCommand::McpDetach { name: name.clone() })
        .await?
    {
        KernelResponse::McpDetached => {
            println!("Detached '{}'.", name);
        }
        KernelResponse::Error { message } => {
            anyhow::bail!("Kernel error: {}", message);
        }
        other => {
            anyhow::bail!("Unexpected response: {:?}", other);
        }
    }
    Ok(())
}

// ── catalog ─────────────────────────────────────────────────────────────────

/// Print a table of catalog entries from a `McpCatalogList` response, applying
/// an optional client-side trust-tier filter.
fn print_catalog_list(
    entries: Vec<agentos_bus::CatalogSummary>,
    trust: Option<String>,
) -> anyhow::Result<()> {
    let entries: Vec<_> = match trust {
        Some(t) => {
            let t = t.to_lowercase();
            entries
                .into_iter()
                .filter(|e| e.trust_tier.to_lowercase() == t)
                .collect()
        }
        None => entries,
    };

    if entries.is_empty() {
        println!("No catalog entries.");
        return Ok(());
    }

    println!(
        "{:<16} {:<24} {:<10} {:<8} RUNTIME",
        "ID", "NAME", "TIER", "TRANSPORT"
    );
    println!("{}", "-".repeat(72));
    for e in &entries {
        println!(
            "{:<16} {:<24} {:<10} {:<8} {}",
            truncate(&e.id, 16),
            truncate(&e.display_name, 24),
            truncate(&e.trust_tier, 10),
            truncate(&e.transport, 8),
            e.runtime.as_deref().unwrap_or("-"),
        );
    }
    Ok(())
}

/// `mcp catalog list` — list all catalog entries, optionally filtered by tier.
pub async fn cmd_catalog_list(bus: &mut BusClient, trust: Option<String>) -> anyhow::Result<()> {
    match bus.send_command(KernelCommand::McpCatalogList).await? {
        KernelResponse::McpCatalogList(entries) => print_catalog_list(entries, trust),
        KernelResponse::Error { message } => anyhow::bail!("Kernel error: {message}"),
        other => anyhow::bail!("Unexpected response: {other:?}"),
    }
}

/// `mcp catalog search <query>` — search entries by id/name/description.
pub async fn cmd_catalog_search(bus: &mut BusClient, query: String) -> anyhow::Result<()> {
    match bus
        .send_command(KernelCommand::McpCatalogSearch { query })
        .await?
    {
        KernelResponse::McpCatalogList(entries) => print_catalog_list(entries, None),
        KernelResponse::Error { message } => anyhow::bail!("Kernel error: {message}"),
        other => anyhow::bail!("Unexpected response: {other:?}"),
    }
}

/// `mcp catalog info <id>` — print the full entry as pretty JSON.
pub async fn cmd_catalog_info(bus: &mut BusClient, id: String) -> anyhow::Result<()> {
    match bus
        .send_command(KernelCommand::McpCatalogInfo { id })
        .await?
    {
        KernelResponse::McpCatalogInfo(value) => {
            println!("{}", serde_json::to_string_pretty(&value)?);
            Ok(())
        }
        KernelResponse::Error { message } => anyhow::bail!("Kernel error: {message}"),
        other => anyhow::bail!("Unexpected response: {other:?}"),
    }
}

// ── install / uninstall ──────────────────────────────────────────────────────

/// `mcp install <id>` — one-command catalog install.
pub async fn cmd_mcp_install(
    bus: &mut BusClient,
    id: String,
    yes: bool,
    allow_community: bool,
    runtime_binary: Option<String>,
    no_auth: bool,
) -> anyhow::Result<()> {
    match bus
        .send_command(KernelCommand::McpInstall {
            id: id.clone(),
            assume_yes: yes,
            allow_community,
            runtime_binary_override: runtime_binary,
            no_auth,
        })
        .await?
    {
        KernelResponse::McpAttached { tool_count, tools } => {
            println!("Installed '{id}' — {tool_count} tool(s) registered.");
            if !tools.is_empty() {
                println!("  {}", tools.join(", "));
            }
            Ok(())
        }
        KernelResponse::Error { message } => anyhow::bail!("Kernel error: {message}"),
        other => anyhow::bail!("Unexpected response: {other:?}"),
    }
}

/// `mcp uninstall <id>` — detach a catalog server.
pub async fn cmd_mcp_uninstall(bus: &mut BusClient, id: String, purge: bool) -> anyhow::Result<()> {
    match bus
        .send_command(KernelCommand::McpUninstall {
            id: id.clone(),
            purge,
        })
        .await?
    {
        KernelResponse::McpDetached => {
            println!("Uninstalled '{id}'.");
            Ok(())
        }
        KernelResponse::Error { message } => anyhow::bail!("Kernel error: {message}"),
        other => anyhow::bail!("Unexpected response: {other:?}"),
    }
}

/// Truncate a string to `max` chars, appending `…` if it was cut.
fn truncate(s: &str, max: usize) -> String {
    let mut chars = s.chars();
    let out: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{out}…")
    } else {
        out
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Build a broad `PermissionSet` suitable for the `mcp serve` operator context.
///
/// `mcp serve` is run directly by the operator to expose AgentOS tools to a
/// local MCP client (e.g. Claude Desktop). The operator has explicitly chosen
/// to expose these tools, so they receive access to all standard tool resource
/// categories. SSRF protection for network resources is enforced by
/// `PermissionSet::is_denied()` regardless of these grants.
fn operator_permissions() -> agentos_types::PermissionSet {
    use agentos_types::{PermissionOp, PermissionSet};
    let mut p = PermissionSet::new();
    // Filesystem — covers fs.user_data, fs.app_logs, fs.system_logs, and all path-based tools.
    p.grant("fs:".into(), true, true, true, None);
    p.grant("fs.user_data".into(), true, true, false, None);
    // Memory subsystem — semantic, episodic, blocks, procedural.
    p.grant("memory.".into(), true, true, false, None);
    // Network URL-style resources (http_client, etc.).
    // SSRF protection for private ranges is enforced by PermissionSet::is_denied() regardless.
    p.grant("net:".into(), true, true, true, None);
    // Network dot-notation resources: network.outbound (web_fetch), network.logs (network_monitor).
    p.grant("network.".into(), true, false, true, None);
    // Hardware HAL queries (hardware.system, hardware.gpu, etc.) — read-only.
    p.grant("hardware.".into(), true, false, false, None);
    // Process tools: process.exec (shell_exec), process.list / process.kill (process_manager).
    p.grant("process.".into(), true, false, true, None);
    // Task queries: task.query (task_status, task_list).
    p.grant("task.".into(), true, false, false, None);
    // Escalation queries: escalation.query uses PermissionOp::Query (not covered by grant()).
    p.grant("escalation.".into(), true, false, false, None);
    p.grant_op("escalation.".into(), PermissionOp::Query, None);
    // User interaction tools: user.notify (notify_user), user.interact (ask_user).
    p.grant("user.".into(), true, true, true, None);
    // Agent registry and messaging: agent.registry (agent_list), agent.message (agent_message).
    p.grant("agent.".into(), true, false, true, None);
    // Data and pipeline tools.
    p.grant("data.".into(), true, true, false, None);
    p
}

// ── BearerTokenAuth ───────────────────────────────────────────────────────────

/// Static bearer-token authenticator for `mcp serve --transport http`.
///
/// Uses constant-time comparison to prevent timing side-channel attacks.
struct BearerTokenAuth(String);

#[async_trait]
impl McpAuthValidator for BearerTokenAuth {
    async fn validate_token(&self, token: &str) -> Result<(), String> {
        let expected = self.0.as_bytes();
        let provided = token.as_bytes();
        // Always compare the same number of bytes to avoid length-based timing leaks.
        // If lengths differ, compare against self to ensure constant work, then reject.
        let ok: bool = if expected.len() == provided.len() {
            expected.ct_eq(provided).into()
        } else {
            // Perform a dummy comparison so branch timing is uniform.
            let _ = expected.ct_eq(expected);
            false
        };
        if ok {
            Ok(())
        } else {
            Err("Invalid bearer token".to_string())
        }
    }
}

// ── McpToolExecutor impl ──────────────────────────────────────────────────────

/// Wraps an AgentOS `ToolRunner` as an `McpToolExecutor` so the kernel's
/// registered tools can be exposed via the MCP server.
struct ToolRunnerExecutor {
    runner: Arc<ToolRunner>,
    data_dir: std::path::PathBuf,
}

#[async_trait]
impl McpToolExecutor for ToolRunnerExecutor {
    async fn list_tools(&self) -> Vec<McpToolDef> {
        self.runner
            .list_tools()
            .into_iter()
            .map(|name| McpToolDef {
                description: format!("AgentOS tool: {}", name),
                input_schema: serde_json::json!({ "type": "object" }),
                name,
            })
            .collect()
    }

    async fn list_resources(&self) -> Vec<agentos_mcp::McpResourceDef> {
        vec![
            agentos_mcp::McpResourceDef {
                uri: "agentos://tools".to_string(),
                name: "tools".to_string(),
                description: "List of all registered AgentOS tools with schemas".to_string(),
                mime_type: "application/json".to_string(),
            },
            agentos_mcp::McpResourceDef {
                uri: "agentos://status".to_string(),
                name: "status".to_string(),
                description: "Runtime status of the AgentOS tool server".to_string(),
                mime_type: "application/json".to_string(),
            },
        ]
    }

    async fn read_resource(&self, uri: &str) -> Result<agentos_mcp::McpResourceContent, String> {
        match uri {
            "agentos://tools" => {
                let tools = self.list_tools().await;
                let text = serde_json::to_string_pretty(&tools).map_err(|e| e.to_string())?;
                Ok(agentos_mcp::McpResourceContent {
                    uri: uri.to_string(),
                    mime_type: "application/json".to_string(),
                    text,
                })
            }
            "agentos://status" => {
                let tool_count = self.runner.list_tools().len();
                let text = serde_json::to_string_pretty(&serde_json::json!({
                    "status": "running",
                    "tool_count": tool_count,
                    "transport": "mcp"
                }))
                .map_err(|e| e.to_string())?;
                Ok(agentos_mcp::McpResourceContent {
                    uri: uri.to_string(),
                    mime_type: "application/json".to_string(),
                    text,
                })
            }
            _ => Err(format!("Resource not found: {}", uri)),
        }
    }

    async fn list_prompts(&self) -> Vec<agentos_mcp::McpPromptDef> {
        vec![agentos_mcp::McpPromptDef {
            name: "use-tool".to_string(),
            description: "Generate a prompt to invoke a specific AgentOS tool".to_string(),
            arguments: vec![
                agentos_mcp::McpPromptArgument {
                    name: "tool_name".to_string(),
                    description: "Name of the tool to invoke".to_string(),
                    required: true,
                },
                agentos_mcp::McpPromptArgument {
                    name: "goal".to_string(),
                    description: "What you want to accomplish with this tool".to_string(),
                    required: true,
                },
            ],
        }]
    }

    async fn get_prompt(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> Result<Vec<agentos_mcp::McpPromptMessage>, String> {
        match name {
            "use-tool" => {
                let tool_name = args
                    .get("tool_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let goal = args
                    .get("goal")
                    .and_then(|v| v.as_str())
                    .unwrap_or("complete the task");
                Ok(vec![agentos_mcp::McpPromptMessage {
                    role: "user".to_string(),
                    content: agentos_mcp::McpPromptContent {
                        content_type: "text".to_string(),
                        text: format!("Use the '{}' tool to: {}", tool_name, goal),
                    },
                }])
            }
            _ => Err(format!("Prompt not found: {}", name)),
        }
    }

    async fn call_tool(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        use agentos_tools::traits::ToolExecutionContext;
        use agentos_types::*;
        use tokio_util::sync::CancellationToken;

        let ctx = ToolExecutionContext {
            data_dir: self.data_dir.clone(),
            task_id: TaskID::new(),
            agent_id: AgentID::new(),
            trace_id: TraceID::new(),
            // mcp serve is an operator-invoked local command: grant broad access
            // to all core tool categories.  SSRF protection for network resources
            // is enforced by PermissionSet::is_denied() regardless of these grants.
            permissions: operator_permissions(),
            vault: None,
            hal: None,
            file_lock_registry: None,
            agent_registry: None,
            task_registry: None,
            escalation_query: None,
            workspace_paths: vec![],
            workspace_paths_writable: vec![],
            workspace_paths_executable: vec![],
            capability_registry: None,
            capability_dispatcher: None,
            storage_zone_query: None,
            cancellation_token: CancellationToken::new(),
            tool_categories: None,
        };

        self.runner
            .execute(name, args, ctx)
            .await
            .map_err(|e| e.to_string())
    }
}

// ── A2A commands ──────────────────────────────────────────────────────────────

/// Fetch and display a remote agent's A2A Agent Card.
async fn cmd_a2a_discover(url: &str) -> anyhow::Result<()> {
    let client = A2AClient::new(url);
    let card: AgentCard = client.discover().await?;

    println!("Agent Card: {}", card.name);
    println!("  Description: {}", card.description);
    println!("  Provider:    {}", card.provider);
    println!("  Version:     {}", card.version);
    println!("  Protocol:    {}", card.protocol_version);
    println!("  URL:         {}", card.url);
    println!();
    if card.capabilities.is_empty() {
        println!("  No capabilities advertised.");
    } else {
        println!("  Capabilities ({}):", card.capabilities.len());
        for cap in &card.capabilities {
            println!("    • {}  — {}", cap.name, cap.description);
        }
    }
    Ok(())
}

/// Delegate a task to a remote A2A agent and poll for the result.
async fn cmd_a2a_delegate(
    url: &str,
    capability: &str,
    input_json: &str,
    token: Option<&str>,
) -> anyhow::Result<()> {
    let input: serde_json::Value = serde_json::from_str(input_json)
        .map_err(|e| anyhow::anyhow!("Invalid JSON input: {}", e))?;

    let mut client = A2AClient::new(url);
    if let Some(t) = token {
        client = client.with_token(t);
    }

    let task_id = client
        .submit_task(capability, input, "http://localhost")
        .await?;
    println!("Task submitted: {}", task_id);

    // Poll until terminal state (max 30 attempts, 1s apart)
    for _ in 0..30 {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let task = client.poll_task(&task_id).await?;
        match &task.status {
            agentos_mcp::A2ATaskStatus::Completed { output } => {
                println!("Completed:");
                println!("{}", serde_json::to_string_pretty(output)?);
                return Ok(());
            }
            agentos_mcp::A2ATaskStatus::Failed { error } => {
                anyhow::bail!("Task failed: {}", error);
            }
            agentos_mcp::A2ATaskStatus::Cancelled => {
                anyhow::bail!("Task was cancelled");
            }
            state => {
                println!("Status: {:?}", state);
            }
        }
    }
    anyhow::bail!("Timed out waiting for task to complete")
}

/// Show the A2A Agent Card this server would advertise.
async fn cmd_a2a_card(config_path: &str) -> anyhow::Result<()> {
    let config = agentos_kernel::config::load_config(std::path::Path::new(config_path))?;
    let data_dir = std::path::PathBuf::from(&config.tools.data_dir);
    let runner = ToolRunner::new(&data_dir).map_err(|e| anyhow::anyhow!(e))?;
    let tool_names: Vec<String> = runner.list_tools();

    let card = AgentCard::from_tools(
        "agentos",
        "AgentOS — secure LLM-native agent runtime",
        "http://localhost:3001",
        &tool_names,
        AuthRequirement::Bearer {
            description: "Provide a CapabilityToken as Bearer token".to_string(),
        },
    );

    println!("{}", serde_json::to_string_pretty(&card)?);
    Ok(())
}
