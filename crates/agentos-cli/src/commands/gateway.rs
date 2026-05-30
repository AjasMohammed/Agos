//! `agentos gateway` — run AgentOS as a long-lived messaging gateway ("run as
//! a bot").
//!
//! `gateway run` boots the kernel and connects every channel declared in the
//! `[gateway]` config block (see `GatewaySettings` in agentos-kernel), then runs
//! until SIGINT/SIGTERM — intended for systemd / docker-compose deployment.
//!
//! The boot + signal-loop is shared with `agentos start` via
//! `cmd_start(.., gateway = true)` in `main.rs`, so there is exactly one
//! kernel-boot code path.

/// Subcommands for `agentos gateway`.
#[derive(clap::Subcommand)]
pub enum GatewayCommands {
    /// Boot the kernel and connect all configured channels as a daemon.
    Run,
}
