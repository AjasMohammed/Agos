# Quickstart

Get from zero to a running task in minutes. The binary is **`agentos`**.

## 1. Install

### One-line installer (recommended)

```bash
curl -fsSL https://raw.githubusercontent.com/AjasMohammed/Agos/main/scripts/install.sh | bash
```

The installer detects your OS/arch, downloads the matching prebuilt binary, **verifies the
SHA-256 checksum (mandatory)** and the minisign signature when available, installs to
`~/.local/bin`, and runs `agentos doctor`. Pin a version with `AGENTOS_VERSION=v1.0.0` or
change the target with `AGENTOS_INSTALL_DIR`.

> Linux is the primary target. On macOS, seccomp sandboxing and most hardware (HAL) tools
> are unavailable and degrade gracefully. On Windows, use WSL2.

### Homebrew

```bash
brew tap agentos/tap
brew install agentos
```

Homebrew installs the same prebuilt, signed binary per architecture.

### From source (developer path)

```bash
cargo install --git https://github.com/AjasMohammed/Agos --tag v1.0.0 --locked agentos-cli
```

Building from source requires the **Rust 1.91+** toolchain ([rustup.rs](https://rustup.rs)).
On Linux, install bubblewrap for the `shell-exec` sandbox: `sudo apt install bubblewrap`.

## 2. Configure

Run the interactive wizard. It configures providers, agents, and data paths. **API keys are
never written to disk** — only a reference to the environment variable holding the key is
stored.

```bash
agentos onboard
```

## 3. Verify your install

```bash
agentos doctor
```

`doctor` runs checks for the config file, TOML validity, vault/audit directory write access,
the bus socket, and tool loading. Add `--fix` to auto-repair common issues.

## 4. Run something

Start the web UI:

```bash
agentos web serve            # http://127.0.0.1:8080
```

…or run a task straight from the CLI. Connect an agent, grant it a permission, then run:

```bash
agentos agent connect --provider ollama --model llama3.2 --name analyst
agentos perm grant analyst fs.user_data:rw
agentos task run --agent analyst "Summarize the files in my data directory"
```

If you omit `--agent`, the kernel's task router selects an agent automatically.

## Next steps

- **[CLI Reference](./cli.md)** — every command group.
- **[Configuration](./configuration.md)** — tune the kernel, providers, and paths.
- **[Deployment](./deploy/index.md)** — run AgentOS as a service or a bot.
- **[Security](./security.md)** — permissions, the vault, and trust tiers.
