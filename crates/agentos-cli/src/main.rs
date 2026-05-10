use agentos_bus::client::BusClient;
use agentos_hal::drivers::log_reader::LogReaderDriver;
use agentos_hal::drivers::network::NetworkDriver;
use agentos_hal::drivers::process::ProcessDriver;
use agentos_hal::drivers::system::SystemDriver;
use agentos_hal::HardwareAbstractionLayer;
use agentos_kernel::{load_config, resolve_boot_vault_passphrase, Kernel};
use agentos_vault::ZeroizingString;
use clap::{Parser, Subcommand};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

mod commands;
mod embedded;
pub mod package_index;

#[derive(clap::Subcommand)]
pub enum ConfigSubcommand {
    /// Read a configuration value by dotted key (e.g. llm.primary)
    Get {
        /// Dotted key path (e.g. llm.primary)
        key: String,
    },
    /// Write a configuration value by dotted key
    Set {
        /// Dotted key path (e.g. llm.primary)
        key: String,
        /// New value to set
        value: String,
    },
    /// List all top-level config sections
    List,
}

use commands::{
    a2a::A2ACommands, agent::AgentCommands, audit::AuditCommands, bg::BgCommands,
    channel::ChannelCommands, cost::CostCommands, escalation::EscalationCommands,
    event::EventCommands, hal::HalCommands, identity::IdentityCommands, init::InitTemplate,
    log::LogCommands, mcp::McpCommands, notifications::NotificationCommands, perm::PermCommands,
    pipeline::PipelineCommands, provider::ProviderCommands, resource::ResourceCommands,
    role::RoleCommands, schedule::ScheduleCommands, scratchpad::ScratchpadCommands,
    secret::SecretCommands, skill::SkillCommands, snapshot::SnapshotCommands, task::TaskCommands,
    team::TeamCommands, tool::ToolCommands, web::WebCommands,
};

#[derive(Parser)]
#[command(name = "agentos")]
#[command(about = "AgentOS — Control CLI for the LLM-native operating system")]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Path to kernel config file
    #[arg(long, env = "AGENTOS_CONFIG", default_value = "config/default.toml")]
    pub config: String,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Boot the AgentOS kernel
    Start,

    /// Gracefully shut down the running kernel
    Stop,

    /// Manage LLM agents
    Agent {
        #[command(subcommand)]
        command: AgentCommands,
    },

    /// Manage tasks
    Task {
        #[command(subcommand)]
        command: TaskCommands,
    },

    /// Manage tools
    Tool {
        #[command(subcommand)]
        command: ToolCommands,
    },

    /// Manage secrets
    Secret {
        #[command(subcommand)]
        command: SecretCommands,
    },

    /// Manage agent permissions
    Perm {
        #[command(subcommand)]
        command: PermCommands,
    },

    /// Manage OS roles
    Role {
        #[command(subcommand)]
        command: RoleCommands,
    },

    /// Show system status
    Status,

    /// View audit logs
    Audit {
        #[command(subcommand)]
        command: AuditCommands,
    },

    /// Manage scheduled background jobs
    Schedule {
        #[command(subcommand)]
        command: ScheduleCommands,
    },

    /// Manage background tasks
    Bg {
        #[command(subcommand)]
        command: BgCommands,
    },

    /// Manage multi-agent pipelines
    Pipeline {
        #[command(subcommand)]
        command: PipelineCommands,
    },

    /// Run and manage agent teams (coordinator + workers)
    Team {
        #[command(subcommand)]
        command: TeamCommands,
    },

    /// View agent cost and budget reports
    Cost {
        #[command(subcommand)]
        command: CostCommands,
    },

    /// Manage resource locks (arbitration)
    Resource {
        #[command(subcommand)]
        command: ResourceCommands,
    },

    /// View and resolve human approval requests from agents
    Escalation {
        #[command(subcommand)]
        command: EscalationCommands,
    },

    /// Manage task snapshots and rollback
    Snapshot {
        #[command(subcommand)]
        command: SnapshotCommands,
    },

    /// Manage agent scratchpad notes
    Scratchpad {
        #[command(subcommand)]
        command: ScratchpadCommands,
    },

    /// Manage event subscriptions and view event history
    Event {
        #[command(subcommand)]
        command: EventCommands,
    },

    /// Manage agent cryptographic identities
    Identity {
        #[command(subcommand)]
        command: IdentityCommands,
    },

    /// Manage hardware device access (HAL)
    Hal {
        #[command(subcommand)]
        command: HalCommands,
    },

    /// Web UI server
    Web {
        #[command(subcommand)]
        command: WebCommands,
    },

    /// Control runtime logging (log level, format)
    Log {
        #[command(subcommand)]
        command: LogCommands,
    },

    /// Check if the kernel health endpoint is responding (used by Docker HEALTHCHECK)
    Healthz {
        /// Health server port
        #[arg(long, default_value_t = 9091)]
        port: u16,
    },

    /// View and respond to agent notifications
    Notifications {
        #[command(subcommand)]
        command: NotificationCommands,
    },

    /// Manage bidirectional notification channels (Telegram, ntfy, email)
    Channel {
        #[command(subcommand)]
        command: ChannelCommands,
    },

    /// Manage autonomous skill packages (system prompt + tools + triggers + budget)
    Skill {
        #[command(subcommand)]
        command: SkillCommands,
    },

    /// MCP (Model Context Protocol) adapter — import/export tools via the standard protocol
    Mcp {
        #[command(subcommand)]
        command: McpCommands,
    },

    /// A2A (Agent-to-Agent) protocol — discover and delegate to external agents
    A2a {
        #[command(subcommand)]
        command: A2ACommands,
    },

    /// List and inspect available LLM providers (built-in + catalog)
    Provider {
        #[command(subcommand)]
        command: ProviderCommands,
    },

    /// Manage plugins — list, enable, disable, and inspect plugin manifests.
    Plugin {
        #[command(subcommand)]
        command: commands::plugin::PluginCommands,
    },

    /// Interactive setup wizard — configure providers, agents, and data paths.
    Onboard,

    /// Diagnose configuration issues and optionally auto-repair them.
    Doctor {
        /// Attempt to auto-repair detected issues.
        #[arg(long, default_value_t = false)]
        fix: bool,
    },

    /// Read or write configuration values without editing TOML manually.
    Config {
        #[command(subcommand)]
        command: ConfigSubcommand,
    },

    /// Scaffold a new AgentOS project from a template.
    ///
    /// Creates a project directory with a working agent configuration,
    /// tool manifests, and an inline README explaining the security model.
    ///
    /// Templates:
    ///   hello-world     — Minimal agent that responds to a prompt
    ///   secure-agent    — Agent with restricted CapabilityToken (recommended)
    ///   mcp-server      — Agent exposed as an MCP server
    ///   multi-agent     — Coordinator + 2 specialist agents
    ///
    /// Examples:
    ///   agentos init my-project
    ///   agentos init my-project --template secure-agent
    Init {
        /// Project name (becomes the directory name)
        name: String,

        /// Template to use (default: secure-agent)
        #[arg(long, short, default_value = "secure-agent")]
        template: InitTemplate,
    },
}

fn main() -> anyhow::Result<()> {
    // Sandbox execution mode: spawned as a child process by SandboxExecutor.
    // Checked before Cli::parse() so clap never sees the flag.
    // Uses a single-threaded tokio runtime to minimize address-space and FD
    // consumption inside the seccomp+rlimit sandbox.
    let raw_args: Vec<String> = std::env::args().collect();
    if raw_args.get(1).map(|s| s.as_str()) == Some("--sandbox-exec") {
        let request_path = raw_args
            .get(2)
            .ok_or_else(|| anyhow::anyhow!("--sandbox-exec requires a request file path"))?
            .clone();
        close_inherited_fds();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| anyhow::anyhow!("sandbox runtime build failed: {e}"))?;
        return rt.block_on(run_sandbox_exec(&request_path));
    }

    tokio_main()
}

fn close_inherited_fds() {
    #[cfg(target_os = "linux")]
    unsafe {
        let ret = libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32);
        if ret != 0 {
            for fd in 3..1024 {
                libc::close(fd);
            }
        }
    }
}

#[tokio::main]
async fn tokio_main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Try to read logging config before init so we know where to write log files.
    // Fall back to defaults if config is missing or unparseable at this point.
    let logging_cfg = {
        let config_path = Path::new(&cli.config);
        if config_path.exists() {
            agentos_kernel::config::load_config(config_path)
                .map(|c| c.logging)
                .unwrap_or_default()
        } else {
            agentos_kernel::config::LoggingSettings::default()
        }
    };

    init_logging(&logging_cfg);

    // Extract embedded assets to working directory if not present
    let data_dir = std::env::var("AGENTOS_DATA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    if let Err(e) = embedded::extract_assets_if_needed(&data_dir) {
        eprintln!("Warning: failed to extract embedded assets: {e}");
    }

    match cli.command {
        Commands::Start => {
            cmd_start(&cli.config).await?;
        }

        Commands::Web { command } => {
            let config_path = Path::new(&cli.config);
            if !config_path.exists() {
                anyhow::bail!("Config file not found: {}", cli.config);
            }
            match command {
                commands::web::WebCommands::Serve { port, host } => {
                    commands::web::handle_serve(config_path, &host, port).await?;
                }
            }
        }

        // Offline tool subcommands run without a kernel connection
        Commands::Tool { command } if commands::tool::is_offline(&command) => {
            commands::tool::handle_offline(command).await?;
        }

        // Offline skill subcommands run without a kernel connection
        Commands::Skill { command } if commands::skill::is_offline(&command) => {
            commands::skill::handle_offline(command).await?;
        }

        // MCP subcommands: `serve` and `list` are offline; the rest require a running kernel.
        Commands::Mcp {
            command: McpCommands::Status,
        } => {
            let config_path = Path::new(&cli.config);
            if !config_path.exists() {
                anyhow::bail!("Config file not found: {}", cli.config);
            }
            let config = agentos_kernel::config::load_config(config_path)?;
            let mut bus_client = BusClient::connect(Path::new(&config.bus.socket_path)).await?;
            commands::mcp::cmd_mcp_status(&mut bus_client).await?;
        }
        Commands::Mcp {
            command:
                McpCommands::Attach {
                    name,
                    url,
                    token,
                    oauth_connector,
                    timeout,
                    env_vars,
                    command_and_args,
                },
        } => {
            let config_path = Path::new(&cli.config);
            if !config_path.exists() {
                anyhow::bail!("Config file not found: {}", cli.config);
            }
            let config = agentos_kernel::config::load_config(config_path)?;
            let mut bus_client = BusClient::connect(Path::new(&config.bus.socket_path)).await?;
            commands::mcp::cmd_mcp_attach(
                &mut bus_client,
                name,
                command_and_args,
                url,
                token,
                oauth_connector,
                timeout,
                env_vars,
            )
            .await?;
        }
        Commands::Mcp {
            command: McpCommands::Detach { name },
        } => {
            let config_path = Path::new(&cli.config);
            if !config_path.exists() {
                anyhow::bail!("Config file not found: {}", cli.config);
            }
            let config = agentos_kernel::config::load_config(config_path)?;
            let mut bus_client = BusClient::connect(Path::new(&config.bus.socket_path)).await?;
            commands::mcp::cmd_mcp_detach(&mut bus_client, name).await?;
        }
        Commands::Mcp {
            command:
                McpCommands::OauthStore {
                    connector_id,
                    provider,
                    access_token,
                    refresh_token,
                    token_endpoint,
                    client_id,
                    client_secret,
                    scopes,
                    expires_in,
                },
        } => {
            let config_path = Path::new(&cli.config);
            if !config_path.exists() {
                anyhow::bail!("Config file not found: {}", cli.config);
            }
            let config = agentos_kernel::config::load_config(config_path)?;
            let mut bus_client = BusClient::connect(Path::new(&config.bus.socket_path)).await?;
            commands::mcp::cmd_mcp_oauth_store(
                &mut bus_client,
                connector_id,
                provider,
                access_token,
                refresh_token,
                token_endpoint,
                client_id,
                client_secret,
                scopes,
                expires_in,
            )
            .await?;
        }
        Commands::Mcp { command } => {
            commands::mcp::handle(command, &cli.config).await?;
        }

        Commands::Init { name, template } => {
            commands::init::handle(&name, template)?;
        }

        // Onboarding wizard — no kernel connection needed
        Commands::Onboard => {
            commands::onboard::handle().await?;
        }

        // Doctor — offline diagnostics
        Commands::Doctor { fix } => {
            commands::doctor::handle(fix).await?;
        }

        // Config get/set/list — offline TOML editing
        Commands::Config { command } => match command {
            ConfigSubcommand::Get { key } => {
                commands::config_cmd::handle_get(&key)?;
            }
            ConfigSubcommand::Set { key, value } => {
                commands::config_cmd::handle_set(&key, &value)?;
            }
            ConfigSubcommand::List => {
                commands::config_cmd::handle_list(None)?;
            }
        },

        // A2A commands are offline (direct HTTP to A2A server, no kernel bus needed)
        Commands::A2a { command } => {
            commands::a2a::handle(command).await?;
        }

        Commands::Healthz { port } => {
            commands::healthz::handle(port).await?;
        }

        Commands::Stop => {
            let config_path = Path::new(&cli.config);
            if !config_path.exists() {
                anyhow::bail!("Config file not found: {}", cli.config);
            }
            let config = agentos_kernel::config::load_config(config_path)?;
            let mut client = BusClient::connect(Path::new(&config.bus.socket_path)).await?;
            match client
                .send_command(agentos_bus::KernelCommand::Shutdown)
                .await?
            {
                agentos_bus::KernelResponse::Success { .. } => {
                    println!("Kernel shutdown initiated.");
                }
                agentos_bus::KernelResponse::Error { message } => {
                    anyhow::bail!("Shutdown failed: {}", message);
                }
                other => {
                    anyhow::bail!("Unexpected response: {:?}", other);
                }
            }
        }

        other => {
            let config_path = Path::new(&cli.config);
            if !config_path.exists() {
                anyhow::bail!("Config file not found: {}", cli.config);
            }
            let config = agentos_kernel::config::load_config(config_path)?;
            let mut client = BusClient::connect(Path::new(&config.bus.socket_path)).await?;
            commands::handle_command(&mut client, other).await?;
        }
    }

    Ok(())
}

fn init_logging(cfg: &agentos_kernel::config::LoggingSettings) {
    // RUST_LOG takes priority; fall back to config value.
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new(format!("agentos={}", cfg.log_level))
    });

    // Wrap the filter in a reload layer so `agentos log set-level` can update it at runtime.
    let (filter_reload_layer, reload_handle) = tracing_subscriber::reload::Layer::new(env_filter);

    // Register the reload setter in the kernel so the SetLogLevel command can call it.
    agentos_kernel::logging::register_log_level_setter(Box::new(move |level: &str| {
        let new_filter = tracing_subscriber::EnvFilter::try_new(level)
            .map_err(|e| format!("Invalid log level '{}': {}", level, e))?;
        reload_handle
            .modify(|f| *f = new_filter)
            .map_err(|e| e.to_string())
    }));

    let use_json = cfg.log_format == "json";

    // Stderr-only path (file logging disabled).
    if cfg.log_dir.is_empty() {
        init_logging_stderr_only(filter_reload_layer, use_json);
        return;
    }

    // Create log directory if it doesn't exist.
    if let Err(e) = std::fs::create_dir_all(&cfg.log_dir) {
        eprintln!(
            "Warning: could not create log directory '{}': {}",
            cfg.log_dir, e
        );
        init_logging_stderr_only(filter_reload_layer, use_json);
        return;
    }

    // Probe write access — create_dir_all succeeds even on read-only fs if the
    // directory already exists (e.g. baked into a distroless image). Without
    // this check the rolling appender panics on its first file creation.
    let probe_path = std::path::Path::new(&cfg.log_dir).join(".write_probe");
    match std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&probe_path)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe_path);
        }
        Err(e) => {
            eprintln!(
                "Warning: log directory '{}' is not writable ({}), falling back to stderr",
                cfg.log_dir, e
            );
            init_logging_stderr_only(filter_reload_layer, use_json);
            return;
        }
    }

    let file_appender = tracing_appender::rolling::daily(&cfg.log_dir, "agentos.log");
    // non_blocking returns a guard — must be kept alive for the process lifetime.
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    // Leak the guard so it lives for the entire process.
    std::mem::forget(guard);

    // Helper: builds a chat log layer that captures `agentos::chat` target events
    // into a dedicated `chat.log` file. Uses a macro to avoid generic type mismatch
    // between the JSON and text subscriber stacks.
    macro_rules! make_chat_layer {
        ($log_dir:expr) => {{
            let chat_appender = tracing_appender::rolling::daily($log_dir, "chat.log");
            let (chat_nb, chat_guard) = tracing_appender::non_blocking(chat_appender);
            std::mem::forget(chat_guard);
            tracing_subscriber::fmt::layer()
                .with_target(true)
                .with_thread_ids(false)
                .with_file(true)
                .with_line_number(true)
                .with_ansi(false)
                .with_writer(chat_nb)
                .with_filter(tracing_subscriber::filter::filter_fn(|metadata| {
                    metadata.target().starts_with("agentos::chat")
                }))
        }};
    }

    // Each format branch moves `non_blocking` exactly once.
    if use_json {
        tracing_subscriber::registry()
            .with(filter_reload_layer)
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_target(true)
                    .with_current_span(true)
                    .with_span_list(true),
            )
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_target(true)
                    .with_current_span(true)
                    .with_span_list(true)
                    .with_ansi(false)
                    .with_writer(non_blocking),
            )
            .with(make_chat_layer!(&cfg.log_dir))
            .init();
    } else {
        tracing_subscriber::registry()
            .with(filter_reload_layer)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(true)
                    .with_thread_ids(false)
                    .with_file(true)
                    .with_line_number(true),
            )
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(true)
                    .with_thread_ids(false)
                    .with_file(true)
                    .with_line_number(true)
                    .with_ansi(false)
                    .with_writer(non_blocking),
            )
            .with(make_chat_layer!(&cfg.log_dir))
            .init();
    }
}

fn init_logging_stderr_only(
    filter_reload_layer: tracing_subscriber::reload::Layer<
        tracing_subscriber::EnvFilter,
        tracing_subscriber::Registry,
    >,
    use_json: bool,
) {
    if use_json {
        tracing_subscriber::registry()
            .with(filter_reload_layer)
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_target(true)
                    .with_current_span(true)
                    .with_span_list(true),
            )
            .init();
    } else {
        tracing_subscriber::registry()
            .with(filter_reload_layer)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(true)
                    .with_thread_ids(false)
                    .with_file(true)
                    .with_line_number(true),
            )
            .init();
    }
}

/// Run a single tool in-process, write JSON result to stdout, and exit.
/// Called when the kernel spawns this binary as a sandboxed child via
/// `SandboxExecutor::spawn()`.
async fn run_sandbox_exec(request_path: &str) -> anyhow::Result<()> {
    let contents = std::fs::read_to_string(request_path)
        .map_err(|e| anyhow::anyhow!("sandbox-exec: cannot read request file: {}", e))?;
    let request: agentos_sandbox::SandboxExecRequest = serde_json::from_str(&contents)
        .map_err(|e| anyhow::anyhow!("sandbox-exec: invalid JSON in request file: {}", e))?;
    let agentos_sandbox::SandboxExecRequest {
        tool_name,
        payload,
        data_dir,
        manifest_weight,
        task_id,
        agent_id,
        trace_id,
        permissions,
        workspace_paths,
    } = request;

    let tool = agentos_tools::build_single_tool_with_model_cache_and_weight(
        &tool_name,
        manifest_weight.as_deref(),
        &data_dir,
        &data_dir.join("models"),
    )
    .map_err(|e| anyhow::anyhow!("sandbox-exec: failed to build tool '{}': {}", tool_name, e))?
    .ok_or_else(|| {
        anyhow::anyhow!(
            "sandbox-exec: tool '{}' cannot run in sandbox (unknown, special, or kernel-context)",
            tool_name
        )
    })?;

    for (resource, operation) in tool.required_permissions() {
        if !permissions.check(&resource, operation) {
            anyhow::bail!(
                "sandbox-exec: permission denied for tool '{}' on '{}' ({:?})",
                tool_name,
                resource,
                operation
            );
        }
    }

    let hal = if matches!(
        agentos_tools::tool_category_with_weight(&tool_name, manifest_weight.as_deref()),
        Some(agentos_tools::ToolCategory::Hal)
    ) {
        Some(build_sandbox_hal())
    } else {
        None
    };

    let ctx = agentos_tools::ToolExecutionContext {
        data_dir,
        task_id: task_id.unwrap_or_default(),
        agent_id: agent_id.unwrap_or_default(),
        trace_id: trace_id.unwrap_or_default(),
        permissions,
        vault: None,
        hal,
        // Each sandbox child executes exactly one tool in a dedicated process,
        // so a process-local registry would not coordinate with sibling children.
        file_lock_registry: None,
        agent_registry: None,
        task_registry: None,
        escalation_query: None,
        workspace_paths: workspace_paths.unwrap_or_default(),
        capability_registry: None,
        capability_dispatcher: None,
        storage_zone_query: None,
        cancellation_token: tokio_util::sync::CancellationToken::new(),
        tool_categories: None,
    };

    let result = tool
        .execute(payload, ctx)
        .await
        .map_err(|e| anyhow::anyhow!("sandbox-exec: tool error: {}", e))?;

    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

fn build_sandbox_hal() -> Arc<HardwareAbstractionLayer> {
    let mut hal = HardwareAbstractionLayer::new();
    hal.register(Box::new(SystemDriver::new()));
    hal.register(Box::new(ProcessDriver::new()));
    hal.register(Box::new(NetworkDriver::new()));

    let app_logs = HashMap::new();
    let mut system_logs = HashMap::new();
    system_logs.insert("syslog".to_string(), "/var/log/syslog".into());
    system_logs.insert("kernlog".to_string(), "/var/log/kern.log".into());
    hal.register(Box::new(LogReaderDriver::new(app_logs, system_logs)));

    Arc::new(hal)
}

async fn cmd_start(config_str: &str) -> anyhow::Result<()> {
    let config_path = Path::new(config_str);
    if !config_path.exists() {
        anyhow::bail!("Config file not found: {}", config_str);
    }

    let config = load_config(config_path)?;
    let passphrase = match resolve_boot_vault_passphrase(&config) {
        Ok(Some(passphrase)) => passphrase,
        Ok(None) => ZeroizingString::new(match std::env::var("AGENTOS_VAULT_PASSPHRASE") {
            Ok(env_pass) if !env_pass.is_empty() => env_pass,
            _ => {
                eprint!("Enter vault passphrase: ");
                rpassword::read_password()?
            }
        }),
        Err(err) => return Err(err),
    };

    println!("🚀 Booting AgentOS kernel...");

    let kernel = Arc::new(Kernel::boot(config_path, &passphrase).await?);
    kernel.wire_inbound_chat_bridge();

    // Start the webhook wake-up loop now that kernel is wrapped in Arc.
    kernel.start_webhook_wakeup().await;

    println!("✅ Kernel started");
    println!("   Bus: {}", kernel.config.bus.socket_path);
    println!(
        "   Tools: {} loaded",
        kernel.tool_registry.read().await.list_all().len()
    );

    // ── API server ─────────────────────────────────────────────────────
    if kernel.config.api.enabled {
        let api_host = kernel.config.api.host.clone();
        let api_port = kernel.config.api.port;
        let api_addr: std::net::SocketAddr = format!("{api_host}:{api_port}")
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid API bind address: {e}"))?;

        // Derive api_keys.db path from the same directory as the kernel state DB.
        let api_keys_db = {
            let state_path = std::path::Path::new(&kernel.config.kernel.state_db_path);
            let dir = state_path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(std::path::Path::new("."));
            dir.join("api_keys.db")
        };
        let key_store = agentos_api::ApiKeyStore::open(&api_keys_db)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to open API key store: {e}"))?;
        // Revoke any stale bootstrap keys from previous runs before issuing a
        // fresh one — prevents unbounded accumulation of full-admin keys in the DB.
        key_store.revoke_by_name("bootstrap").await;
        let bootstrap_key = key_store
            .create_key("bootstrap".into(), vec!["*".into()], None)
            .await;

        println!("   API: http://{api_addr}");
        println!();
        println!("╔══════════════════════════════════════════════════════════════════════╗");
        println!("║  Bootstrap API key (full admin access — store securely):             ║");
        println!("║  {bootstrap_key}  ║");
        println!("╚══════════════════════════════════════════════════════════════════════╝");
        println!();
        println!(
            "  Example: curl -H \"Authorization: Bearer {bootstrap_key}\" http://{api_addr}/v1/status"
        );

        let service: Arc<dyn agentos_api::KernelService> = kernel.clone();
        let broadcaster = agentos_api::ws::broadcaster::WsBroadcaster::new();
        tokio::spawn(async move {
            if let Err(e) =
                agentos_api::run_api_server(service, key_store, broadcaster, api_addr).await
            {
                tracing::error!("API server exited with error: {e}");
            }
        });
    }

    println!();
    println!("AgentOS is running. Use another terminal to run agentos commands.");
    println!("Press Ctrl+C to shutdown.");

    // Run kernel supervisor in a child task so we can race signals against it.
    // On SIGINT/SIGTERM we call `kernel.shutdown()` first, which writes the
    // `KernelShutdown` audit entry and cancels the supervisor's cancellation
    // token. Without this, signal-killed processes exit before any audit
    // entry is written, breaking crash forensics.
    let kernel_for_run = kernel.clone();
    let mut kernel_handle = tokio::spawn(async move { kernel_for_run.run().await });

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("Ctrl+C received, shutting down...");
            kernel.shutdown();
        }
        _ = sigterm.recv() => {
            tracing::info!("SIGTERM received, shutting down...");
            kernel.shutdown();
        }
        res = &mut kernel_handle => {
            return match res {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(e),
                Err(join_err) => Err(anyhow::anyhow!("kernel run task panicked: {join_err}")),
            };
        }
    }

    // Wait for supervisor to finish writing its audit trail and clean up.
    match kernel_handle.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(join_err) => Err(anyhow::anyhow!("kernel run task panicked: {join_err}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_sandbox::SandboxExecRequest;
    use agentos_types::PermissionSet;
    use tempfile::{NamedTempFile, TempDir};

    fn write_sandbox_request(request: &SandboxExecRequest) -> NamedTempFile {
        let file = NamedTempFile::new().unwrap();
        std::fs::write(file.path(), serde_json::to_vec(request).unwrap()).unwrap();
        file
    }

    fn write_raw_sandbox_request(value: &serde_json::Value) -> NamedTempFile {
        let file = NamedTempFile::new().unwrap();
        std::fs::write(file.path(), serde_json::to_vec(value).unwrap()).unwrap();
        file
    }

    #[test]
    fn test_cli_parses_start_command() {
        let cli = Cli::try_parse_from(["agentos", "start"]).unwrap();
        assert!(matches!(cli.command, Commands::Start));
    }

    #[test]
    fn test_cli_parses_agent_connect() {
        let cli = Cli::try_parse_from([
            "agentos",
            "agent",
            "connect",
            "--provider",
            "openai",
            "--model",
            "gpt-4",
            "--name",
            "analyst-1",
        ])
        .unwrap();

        match cli.command {
            Commands::Agent {
                command:
                    AgentCommands::Connect {
                        provider,
                        model,
                        name,
                        ..
                    },
            } => {
                assert_eq!(provider, "openai");
                assert_eq!(model, "gpt-4");
                assert_eq!(name, "analyst-1");
            }
            _ => panic!("Wrong command parsed"),
        }
    }

    #[test]
    fn test_cli_parses_task_run() {
        let cli = Cli::try_parse_from([
            "agentos",
            "task",
            "run",
            "--agent",
            "analyst",
            "Summarize the report",
        ])
        .unwrap();

        match cli.command {
            Commands::Task {
                command:
                    TaskCommands::Run {
                        agent,
                        prompt,
                        autonomous: _,
                        no_checkpoint: _,
                        thinking: _,
                    },
            } => {
                assert_eq!(agent, Some("analyst".to_string()));
                assert_eq!(prompt, "Summarize the report");
            }
            _ => panic!("Wrong command parsed"),
        }
    }

    #[test]
    fn test_cli_parses_secret_set_with_scope() {
        let cli = Cli::try_parse_from([
            "agentos",
            "secret",
            "set",
            "SLACK_TOKEN",
            "--scope",
            "agent:notifier",
        ])
        .unwrap();

        match cli.command {
            Commands::Secret {
                command: SecretCommands::Set { name, scope },
            } => {
                assert_eq!(name, "SLACK_TOKEN");
                assert_eq!(scope, "agent:notifier");
            }
            _ => panic!("Wrong command parsed"),
        }
    }

    #[test]
    fn test_cli_parses_perm_grant() {
        let cli = Cli::try_parse_from(["agentos", "perm", "grant", "analyst", "fs.user_data:rw"])
            .unwrap();

        match cli.command {
            Commands::Perm {
                command:
                    PermCommands::Grant {
                        agent,
                        permission,
                        expires: _,
                    },
            } => {
                assert_eq!(agent, "analyst");
                assert_eq!(permission, "fs.user_data:rw");
            }
            _ => panic!("Wrong command parsed"),
        }
    }

    #[test]
    fn test_cli_parses_schedule_create() {
        let cli = Cli::try_parse_from([
            "agentos",
            "schedule",
            "create",
            "--name",
            "daily-report",
            "--cron",
            "0 0 * * * *",
            "--agent",
            "analyst",
            "--task",
            "Summarize logs",
            "--permissions",
            "fs.logs:r",
        ])
        .unwrap();

        match cli.command {
            Commands::Schedule {
                command:
                    ScheduleCommands::Create {
                        name,
                        cron,
                        agent,
                        task: _,
                        permissions,
                    },
            } => {
                assert_eq!(name, "daily-report");
                assert_eq!(cron, "0 0 * * * *");
                assert_eq!(agent, "analyst");
                assert_eq!(permissions, "fs.logs:r");
            }
            _ => panic!("Wrong command parsed"),
        }
    }

    #[test]
    fn test_cli_parses_bg_run() {
        let cli = Cli::try_parse_from([
            "agentos",
            "bg",
            "run",
            "--name",
            "process-data",
            "--agent",
            "worker",
            "--task",
            "process all files",
            "--detach",
        ])
        .unwrap();

        match cli.command {
            Commands::Bg {
                command:
                    BgCommands::Run {
                        name,
                        agent,
                        task: _,
                        detach,
                    },
            } => {
                assert_eq!(name, "process-data");
                assert_eq!(agent, "worker");
                assert!(detach);
            }
            _ => panic!("Wrong command parsed"),
        }
    }

    #[test]
    fn test_cli_parses_event_subscribe() {
        let cli = Cli::try_parse_from([
            "agentos",
            "event",
            "subscribe",
            "--agent",
            "analyst",
            "--event",
            "AgentAdded",
            "--throttle",
            "once_per:30s",
            "--priority",
            "high",
        ])
        .unwrap();

        match cli.command {
            Commands::Event {
                command:
                    EventCommands::Subscribe {
                        agent,
                        event,
                        filter,
                        throttle,
                        priority,
                    },
            } => {
                assert_eq!(agent, "analyst");
                assert_eq!(event, "AgentAdded");
                assert_eq!(filter, None);
                assert_eq!(throttle, Some("once_per:30s".to_string()));
                assert_eq!(priority, "high");
            }
            _ => panic!("Wrong command parsed"),
        }
    }

    #[test]
    fn test_cli_parses_event_subscribe_with_payload_filter() {
        let cli = Cli::try_parse_from([
            "agentos",
            "event",
            "subscribe",
            "--agent",
            "analyst",
            "--event",
            "CPUSpikeDetected",
            "--filter",
            "cpu_percent > 90 AND severity == Critical",
        ])
        .unwrap();

        match cli.command {
            Commands::Event {
                command:
                    EventCommands::Subscribe {
                        agent,
                        event,
                        filter,
                        throttle,
                        priority,
                    },
            } => {
                assert_eq!(agent, "analyst");
                assert_eq!(event, "CPUSpikeDetected");
                assert_eq!(
                    filter,
                    Some("cpu_percent > 90 AND severity == Critical".to_string())
                );
                assert_eq!(throttle, None);
                assert_eq!(priority, "normal");
            }
            _ => panic!("Wrong command parsed"),
        }
    }

    #[test]
    fn test_cli_parses_event_subscriptions_list() {
        let cli = Cli::try_parse_from(["agentos", "event", "subscriptions", "list"]).unwrap();

        match cli.command {
            Commands::Event {
                command: EventCommands::Subscriptions { command: _ },
            } => {}
            _ => panic!("Wrong command parsed"),
        }
    }

    #[test]
    fn test_cli_parses_event_history() {
        let cli = Cli::try_parse_from(["agentos", "event", "history", "--last", "50"]).unwrap();

        match cli.command {
            Commands::Event {
                command: EventCommands::History { last },
            } => {
                assert_eq!(last, 50);
            }
            _ => panic!("Wrong command parsed"),
        }
    }

    #[test]
    fn test_cli_parses_hal_register() {
        let cli = Cli::try_parse_from([
            "agentos",
            "hal",
            "register",
            "--id",
            "gpu:0",
            "--type",
            "nvidia-rtx-4090",
        ])
        .unwrap();

        match cli.command {
            Commands::Hal {
                command: HalCommands::Register { id, device_type },
            } => {
                assert_eq!(id, "gpu:0");
                assert_eq!(device_type, "nvidia-rtx-4090");
            }
            _ => panic!("Wrong command parsed"),
        }
    }

    #[test]
    fn test_cli_parses_hal_approve() {
        let cli = Cli::try_parse_from(["agentos", "hal", "approve", "gpu:0", "--agent", "worker"])
            .unwrap();

        match cli.command {
            Commands::Hal {
                command: HalCommands::Approve { device, agent },
            } => {
                assert_eq!(device, "gpu:0");
                assert_eq!(agent, "worker");
            }
            _ => panic!("Wrong command parsed"),
        }
    }

    #[test]
    fn test_cli_parses_web_serve() {
        let cli = Cli::try_parse_from([
            "agentos", "web", "serve", "--port", "9090", "--host", "0.0.0.0",
        ])
        .unwrap();

        match cli.command {
            Commands::Web {
                command: WebCommands::Serve { port, host, .. },
            } => {
                assert_eq!(port, 9090);
                assert_eq!(host, "0.0.0.0");
            }
            _ => panic!("Wrong command parsed"),
        }
    }

    #[test]
    fn test_cli_parses_web_serve_defaults() {
        let cli = Cli::try_parse_from(["agentos", "web", "serve"]).unwrap();

        match cli.command {
            Commands::Web {
                command: WebCommands::Serve { port, host, .. },
            } => {
                assert_eq!(port, 8080);
                assert_eq!(host, "127.0.0.1");
            }
            _ => panic!("Wrong command parsed"),
        }
    }

    #[test]
    fn test_cli_parses_log_set_level() {
        let cli = Cli::try_parse_from(["agentos", "log", "set-level", "debug"]).unwrap();
        match cli.command {
            Commands::Log {
                command: LogCommands::SetLevel { level },
            } => {
                assert_eq!(level, "debug");
            }
            _ => panic!("Wrong command parsed"),
        }
    }

    #[test]
    fn test_cli_parses_log_set_level_compound_directive() {
        let cli = Cli::try_parse_from([
            "agentos",
            "log",
            "set-level",
            "agentos=debug,agentos_kernel=trace",
        ])
        .unwrap();
        match cli.command {
            Commands::Log {
                command: LogCommands::SetLevel { level },
            } => {
                assert_eq!(level, "agentos=debug,agentos_kernel=trace");
            }
            _ => panic!("Wrong command parsed"),
        }
    }

    #[tokio::test]
    async fn test_run_sandbox_exec_rejects_kernel_context_tool() {
        let temp_dir = TempDir::new().unwrap();
        let request = SandboxExecRequest {
            tool_name: "agent-message".to_string(),
            payload: serde_json::json!({}),
            data_dir: temp_dir.path().to_path_buf(),
            manifest_weight: None,
            task_id: None,
            agent_id: None,
            trace_id: None,
            permissions: PermissionSet::new(),
            workspace_paths: None,
        };
        let request_file = write_sandbox_request(&request);

        let err = run_sandbox_exec(request_file.path().to_str().unwrap())
            .await
            .unwrap_err();

        assert!(err
            .to_string()
            .contains("cannot run in sandbox (unknown, special, or kernel-context)"));
    }

    #[tokio::test]
    async fn test_run_sandbox_exec_enforces_required_permissions() {
        let temp_dir = TempDir::new().unwrap();
        let mut permissions = PermissionSet::new();
        permissions.grant("fs.user_data".to_string(), true, false, false, None);

        let allowed_request = SandboxExecRequest {
            tool_name: "file-reader".to_string(),
            payload: serde_json::json!({"path": "missing.txt"}),
            data_dir: temp_dir.path().to_path_buf(),
            manifest_weight: None,
            task_id: None,
            agent_id: None,
            trace_id: None,
            permissions,
            workspace_paths: None,
        };
        let denied_request = SandboxExecRequest {
            permissions: PermissionSet::new(),
            ..allowed_request.clone()
        };

        let denied_file = write_sandbox_request(&denied_request);
        let denied_err = run_sandbox_exec(denied_file.path().to_str().unwrap())
            .await
            .unwrap_err();
        assert!(denied_err
            .to_string()
            .contains("permission denied for tool 'file-reader'"));

        let allowed_file = write_sandbox_request(&allowed_request);
        let allowed_err = run_sandbox_exec(allowed_file.path().to_str().unwrap())
            .await
            .unwrap_err();
        assert!(allowed_err
            .to_string()
            .contains("sandbox-exec: tool error:"));
        assert!(!allowed_err.to_string().contains("permission denied"));
    }

    #[tokio::test]
    async fn test_run_sandbox_exec_rejects_request_missing_permissions() {
        let temp_dir = TempDir::new().unwrap();
        let raw_request = serde_json::json!({
            "tool_name": "datetime",
            "payload": {},
            "data_dir": temp_dir.path(),
        });
        let request_file = write_raw_sandbox_request(&raw_request);

        let err = run_sandbox_exec(request_file.path().to_str().unwrap())
            .await
            .unwrap_err();

        assert!(err.to_string().contains("sandbox-exec: invalid JSON"));
        assert!(err.to_string().contains("permissions"));
    }
}
