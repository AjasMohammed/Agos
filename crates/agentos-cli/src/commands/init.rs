/// `agentos init` — Scaffold a new AgentOS project from a template.
///
/// Writes a working project directory that includes:
/// - `agent.toml` — agent manifest with capability token configuration
/// - `config.toml` — minimal kernel config (data dir, logging)
/// - `tools/`     — example custom tool manifests
/// - `README.md`  — inline guide explaining what each file does
///
/// All security-relevant settings are pre-populated with inline comments
/// so developers understand the capability model from the first run.
use std::fs;
use std::path::{Path, PathBuf};

use clap::ValueEnum;

// ── Template enum ─────────────────────────────────────────────────────────────

/// Available project templates.
#[derive(Debug, Clone, ValueEnum)]
pub enum InitTemplate {
    /// Minimal "hello world" agent — simplest possible setup
    HelloWorld,
    /// Agent with restricted CapabilityToken (recommended starting point)
    SecureAgent,
    /// Agent exposed as an MCP server for external clients
    McpServer,
    /// Coordinator + 2 specialist agents with sub-agent spawning
    MultiAgent,
}

impl std::fmt::Display for InitTemplate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HelloWorld => write!(f, "hello-world"),
            Self::SecureAgent => write!(f, "secure-agent"),
            Self::McpServer => write!(f, "mcp-server"),
            Self::MultiAgent => write!(f, "multi-agent"),
        }
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub fn handle(project_name: &str, template: InitTemplate) -> anyhow::Result<()> {
    // Validate the project name to prevent path traversal and TOML injection.
    if project_name.is_empty() {
        anyhow::bail!("Project name must not be empty.");
    }
    if project_name.contains('/') || project_name.contains('\\') {
        anyhow::bail!(
            "Project name '{}' must not contain path separators ('/' or '\\').",
            project_name
        );
    }
    if project_name.contains("..") {
        anyhow::bail!("Project name '{}' must not contain '..'.", project_name);
    }
    if project_name.contains('"') || project_name.contains('\n') || project_name.contains('\r') {
        anyhow::bail!(
            "Project name '{}' must not contain quotes or newline characters.",
            project_name
        );
    }

    let project_dir = PathBuf::from(project_name);

    if project_dir.exists() {
        anyhow::bail!(
            "Directory '{}' already exists. Choose a different name.",
            project_name
        );
    }

    fs::create_dir_all(&project_dir)?;

    match template {
        InitTemplate::HelloWorld => scaffold_hello_world(&project_dir, project_name)?,
        InitTemplate::SecureAgent => scaffold_secure_agent(&project_dir, project_name)?,
        InitTemplate::McpServer => scaffold_mcp_server(&project_dir, project_name)?,
        InitTemplate::MultiAgent => scaffold_multi_agent(&project_dir, project_name)?,
    }

    println!();
    println!("  Created project '{}'", project_name);
    println!();
    println!("  Next steps:");
    println!("    cd {}", project_name);
    println!("    agentos kernel start");
    println!("    agentos task run --goal \"Hello world\"");
    println!();
    println!("  Run 'cat README.md' to understand the security model.");
    println!();

    Ok(())
}

// ── Template: hello-world ─────────────────────────────────────────────────────

fn scaffold_hello_world(dir: &Path, name: &str) -> anyhow::Result<()> {
    write_file(dir, "agent.toml", &hello_world_agent(name))?;
    write_file(dir, "config.toml", &minimal_config(name))?;
    write_file(dir, "README.md", HELLO_WORLD_README)?;
    Ok(())
}

fn hello_world_agent(name: &str) -> String {
    format!(
        r#"# AgentOS Agent Manifest — hello-world template
# This is the minimal configuration to run an agent.

[agent]
name = "{name}"
description = "A simple hello-world agent"
model = "mock"   # Change to "anthropic", "openai", etc. once you have a key

[capabilities]
# By default, agents can use any registered tool.
# To restrict access, use the secure-agent template instead.
allowed_tools = ["*"]
"#,
        name = name
    )
}

const HELLO_WORLD_README: &str = r#"# Hello World Agent

This is the simplest possible AgentOS project.

## Quick Start

```bash
agentos kernel start       # Start the AgentOS kernel
agentos task run --goal "Say hello and count to 5"
agentos kernel stop
```

## What's in this project

- `agent.toml`   — Agent manifest: name, model, capability settings
- `config.toml`  — Kernel config: data directory, logging, tool paths
- `README.md`    — This file

## Security Note

This template uses `allowed_tools = ["*"]`, which grants access to ALL tools.
For production use, switch to the `secure-agent` template:

```bash
agentos init my-production-agent --template secure-agent
```

## Next Steps

1. **Change the model**: Edit `agent.toml` and set `model = "anthropic"` (or "openai")
2. **Add tool restrictions**: Move to the `secure-agent` template
3. **Expose via MCP**: Run `agentos mcp serve` to use from Claude Desktop
4. **Run with a goal**: `agentos task run --goal "Research AI trends"`
"#;

// ── Template: secure-agent ────────────────────────────────────────────────────

fn scaffold_secure_agent(dir: &Path, name: &str) -> anyhow::Result<()> {
    write_file(dir, "agent.toml", &secure_agent_manifest(name))?;
    write_file(dir, "config.toml", &minimal_config(name))?;
    fs::create_dir_all(dir.join("tools"))?;
    write_file(dir, "tools/summarize.toml", SUMMARIZE_TOOL_MANIFEST)?;
    write_file(dir, "README.md", SECURE_AGENT_README)?;
    Ok(())
}

fn secure_agent_manifest(name: &str) -> String {
    format!(
        r#"# AgentOS Agent Manifest — secure-agent template
# This template demonstrates the CapabilityToken permission model.
#
# SECURITY MODEL:
#   - allowed_tools:  Whitelist of tools this agent may invoke
#   - permissions:    Fine-grained resource access (read, write, execute)
#   - deny:           Explicit denials that override all grants
#
# The kernel validates the token on EVERY tool call. If a malicious prompt
# tricks the agent into calling a denied tool, the kernel rejects it and
# logs the attempt to the audit trail.

[agent]
name = "{name}"
description = "A security-hardened agent with restricted capabilities"
model = "mock"  # Change to "anthropic", "openai", etc.

[capabilities]
# Whitelist: only these tools can be invoked by this agent.
# The kernel rejects any attempt to call a tool not on this list.
allowed_tools = ["file-reader", "summarize"]

[capabilities.permissions]
# Grant read access to the agent's data directory.
# The file-reader tool will be confined to this directory.
"fs.user_data" = "read"

[capabilities.deny]
# Explicit denials. These take absolute precedence over any grant.
# Even if a bug in your code accidentally grants broader access,
# these entries ensure the agent cannot reach these resources.
entries = [
    "fs:/etc/",       # Block system configuration files
    "fs:/root/",      # Block root home directory
    "net:http://",    # Block all outbound HTTP (use specific allow if needed)
    "net:https://",   # Block all outbound HTTPS
]

[workspace]
# The agent's working directory. All file operations are confined here.
data_dir = "/tmp/{name}-workspace"
"#,
        name = name
    )
}

const SUMMARIZE_TOOL_MANIFEST: &str = r#"# Custom Tool Manifest — summarize
# Place tool manifests in this directory to register them with the kernel.
# Tools marked trust_tier = "core" run in-process.
# Tools marked trust_tier = "community" run in a WASM sandbox.

[tool]
name = "summarize"
description = "Summarize text content passed as input"
trust_tier = "core"   # core = in-process; community = WASM sandbox

[permissions]
required = ["fs.user_data"]
"#;

const SECURE_AGENT_README: &str = r#"# Secure Agent

This template demonstrates AgentOS's CapabilityToken security model.
The agent is restricted to a narrow set of tools and resources.

## Quick Start

```bash
agentos kernel start
agentos task run --goal "Read notes.txt and summarize it"
agentos kernel stop
```

## Security Model (read this!)

The `agent.toml` file configures a **CapabilityToken** — a signed authorization
that specifies exactly what this agent is allowed to do.

### allowed_tools
Only tools in this list can be invoked. If a malicious prompt tries to call
`shell-exec` or `vault-read`, the kernel rejects it:

```
Error: CapabilityDenied — tool 'shell-exec' is not in allowed_tools
Audit: ToolRejected { tool: "shell-exec", reason: "not_allowed" }
```

### permissions
Even for allowed tools, access is resource-scoped. The file-reader tool
can only read from `fs.user_data` (your `data_dir`).

### deny entries
These are absolute. Even if permissions are misconfigured, the deny list
ensures the agent cannot reach `/etc/`, `/root/`, or outbound HTTP.

## Try it: Watch the kernel block a malicious request

```bash
# Start kernel
agentos kernel start

# Try to read a system file (will be denied)
agentos task run --goal "Read the file at /etc/passwd"

# Check the audit log
agentos audit logs --last 5
```

## Add a Custom Tool

1. Create a manifest in `tools/my-tool.toml`
2. Add it to `allowed_tools` in `agent.toml`
3. Restart the kernel: `agentos kernel stop && agentos kernel start`
"#;

// ── Template: mcp-server ──────────────────────────────────────────────────────

fn scaffold_mcp_server(dir: &Path, name: &str) -> anyhow::Result<()> {
    write_file(dir, "agent.toml", &secure_agent_manifest(name))?;
    write_file(dir, "config.toml", &minimal_config(name))?;
    write_file(dir, "start-mcp.sh", &mcp_start_script(name))?;
    write_file(dir, "README.md", &mcp_server_readme(name))?;
    // Make the start script executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("start-mcp.sh");
        let mut perms = fs::metadata(&path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms)?;
    }
    Ok(())
}

fn mcp_start_script(_name: &str) -> String {
    r#"#!/usr/bin/env bash
# Start AgentOS as an MCP server for Claude Desktop / Cursor / any MCP client.
# Stdio transport — pipe JSON-RPC on stdin/stdout.
set -euo pipefail
exec agentos --config "$(dirname "$0")/config.toml" mcp serve
"#
    .to_string()
}

fn mcp_server_readme(name: &str) -> String {
    format!(
        r#"# MCP Server — {name}

Expose AgentOS tools to Claude Desktop, Cursor, or any MCP-compatible client.

## Start the MCP Server

```bash
# Stdio (for Claude Desktop / Cursor)
agentos mcp serve

# HTTP (for remote clients)
agentos mcp serve --transport http --port 3002 --token mysecret
```

## Configure Claude Desktop

Add to `~/Library/Application Support/Claude/claude_desktop_config.json`:

```json
{{
  "mcpServers": {{
    "{name}": {{
      "command": "/path/to/{name}/start-mcp.sh"
    }}
  }}
}}
```

## Security

Each MCP tool call is validated against the CapabilityToken in `agent.toml`.
External clients cannot invoke tools outside the `allowed_tools` whitelist,
and all requests are logged to the audit trail.

## Test the server

```bash
echo '{{"jsonrpc":"2.0","id":1,"method":"tools/list"}}' | agentos mcp serve
```

## Available endpoints (HTTP mode)

- `POST /mcp`        — Submit JSON-RPC 2.0 request
- `GET  /mcp/health` — Health check
"#,
        name = name
    )
}

// ── Template: multi-agent ─────────────────────────────────────────────────────

fn scaffold_multi_agent(dir: &Path, name: &str) -> anyhow::Result<()> {
    write_file(dir, "coordinator.toml", &coordinator_manifest(name))?;
    write_file(
        dir,
        "researcher.toml",
        &specialist_manifest("researcher", "Research information and summarize findings"),
    )?;
    write_file(
        dir,
        "writer.toml",
        &specialist_manifest("writer", "Draft content based on research findings"),
    )?;
    write_file(dir, "config.toml", &minimal_config(name))?;
    write_file(dir, "README.md", MULTI_AGENT_README)?;
    Ok(())
}

fn coordinator_manifest(name: &str) -> String {
    format!(
        r#"# AgentOS Coordinator Agent Manifest
# This agent spawns sub-agents to handle specialist tasks.

[agent]
name = "{name}-coordinator"
description = "Coordinator that delegates to researcher and writer sub-agents"
model = "mock"

[capabilities]
# The coordinator needs agent spawning permission
allowed_tools = ["spawn-agent", "await-agents", "file-reader", "file-writer"]

[capabilities.permissions]
"agent.spawn" = "execute"
"agent.await" = "execute"
"fs.user_data" = "read_write"

[team]
# Sub-agents available to this coordinator
members = ["researcher", "writer"]
"#,
        name = name
    )
}

fn specialist_manifest(role: &str, description: &str) -> String {
    format!(
        r#"# AgentOS Specialist Agent — {role}
# {description}

[agent]
name = "{role}"
description = "{description}"
model = "mock"

[capabilities]
allowed_tools = ["file-reader", "file-writer", "memory-search"]

[capabilities.permissions]
"fs.user_data" = "read_write"
"memory.semantic" = "read_write"
"#,
        role = role,
        description = description
    )
}

const MULTI_AGENT_README: &str = r#"# Multi-Agent Project

This template sets up a coordinator agent that delegates to specialist sub-agents.

## Architecture

```
coordinator
├── researcher  (research + summarize)
└── writer      (draft content from research)
```

## Quick Start

```bash
agentos kernel start

# Register all agents
agentos agent register coordinator.toml
agentos agent register researcher.toml
agentos agent register writer.toml

# Run the coordinator
agentos task run --agent coordinator --goal "Research AI safety and write a summary"

agentos kernel stop
```

## How Sub-Agent Spawning Works

The coordinator uses the `spawn-agent` tool to delegate tasks:

1. Coordinator receives a high-level goal
2. It spawns a "researcher" sub-agent with a specific research question
3. It awaits the researcher's result using `await-agents`
4. It spawns a "writer" sub-agent with the research output
5. It collects and returns the final content

Each sub-agent runs with its own restricted CapabilityToken — the researcher
cannot write files, the writer cannot spawn further agents.

## Audit Trail

All spawning and delegation is logged:
```bash
agentos audit logs --last 20
```
"#;

// ── Shared config template ────────────────────────────────────────────────────

fn minimal_config(name: &str) -> String {
    format!(
        r#"# AgentOS Kernel Configuration — {name}
# This is the minimal configuration to run the kernel.
# See docs/guide/getting-started.md for all available options.

[kernel]
max_concurrent_tasks = 4
task_timeout_secs = 300

[bus]
socket_path = "/tmp/{name}-kernel.sock"

[llm]
provider = "mock"      # Change to "anthropic", "openai", etc.
default_model = "mock"

[tools]
data_dir = "/tmp/{name}-data"

[audit]
db_path = "/tmp/{name}-audit.db"

[logging]
log_level = "info"
log_dir = ""           # Empty = log to stderr only
"#,
        name = name
    )
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn write_file(dir: &Path, name: &str, content: &str) -> anyhow::Result<()> {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, content)?;
    println!("  + {}", path.display());
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn run_init_in_temp(name: &str, template: InitTemplate) -> TempDir {
        let tmp = TempDir::new().unwrap();
        let project_dir = tmp.path().join(name);
        std::fs::create_dir_all(&project_dir).unwrap();

        match template {
            InitTemplate::HelloWorld => scaffold_hello_world(&project_dir, name).unwrap(),
            InitTemplate::SecureAgent => scaffold_secure_agent(&project_dir, name).unwrap(),
            InitTemplate::McpServer => scaffold_mcp_server(&project_dir, name).unwrap(),
            InitTemplate::MultiAgent => scaffold_multi_agent(&project_dir, name).unwrap(),
        }
        tmp
    }

    #[test]
    fn hello_world_creates_expected_files() {
        let tmp = run_init_in_temp("test-hw", InitTemplate::HelloWorld);
        let dir = tmp.path().join("test-hw");
        assert!(dir.join("agent.toml").exists());
        assert!(dir.join("config.toml").exists());
        assert!(dir.join("README.md").exists());
    }

    #[test]
    fn secure_agent_creates_tool_manifest() {
        let tmp = run_init_in_temp("test-sa", InitTemplate::SecureAgent);
        let dir = tmp.path().join("test-sa");
        assert!(dir.join("agent.toml").exists());
        assert!(dir.join("tools/summarize.toml").exists());
        // Verify deny entries are present
        let agent = std::fs::read_to_string(dir.join("agent.toml")).unwrap();
        assert!(agent.contains("deny"));
        assert!(agent.contains("fs:/etc/"));
    }

    #[test]
    fn mcp_server_creates_start_script() {
        let tmp = run_init_in_temp("test-mcp", InitTemplate::McpServer);
        let dir = tmp.path().join("test-mcp");
        assert!(dir.join("start-mcp.sh").exists());
        assert!(dir.join("README.md").exists());
    }

    #[test]
    fn multi_agent_creates_all_manifests() {
        let tmp = run_init_in_temp("test-ma", InitTemplate::MultiAgent);
        let dir = tmp.path().join("test-ma");
        assert!(dir.join("coordinator.toml").exists());
        assert!(dir.join("researcher.toml").exists());
        assert!(dir.join("writer.toml").exists());
    }

    #[test]
    fn agent_toml_contains_project_name() {
        let tmp = run_init_in_temp("my-cool-agent", InitTemplate::SecureAgent);
        let agent = std::fs::read_to_string(tmp.path().join("my-cool-agent/agent.toml")).unwrap();
        assert!(agent.contains("my-cool-agent"));
    }

    #[test]
    fn config_toml_contains_project_name() {
        let tmp = run_init_in_temp("my-proj", InitTemplate::HelloWorld);
        let config = std::fs::read_to_string(tmp.path().join("my-proj/config.toml")).unwrap();
        assert!(config.contains("my-proj"));
    }

    #[test]
    fn reject_path_traversal_in_project_name() {
        assert!(handle("../../evil", InitTemplate::HelloWorld).is_err());
        assert!(handle("../parent", InitTemplate::HelloWorld).is_err());
    }

    #[test]
    fn reject_slash_in_project_name() {
        assert!(handle("foo/bar", InitTemplate::HelloWorld).is_err());
        assert!(handle("foo\\bar", InitTemplate::HelloWorld).is_err());
    }

    #[test]
    fn reject_quote_in_project_name() {
        assert!(handle("foo\"bar", InitTemplate::HelloWorld).is_err());
    }

    #[test]
    fn reject_empty_project_name() {
        assert!(handle("", InitTemplate::HelloWorld).is_err());
    }
}
