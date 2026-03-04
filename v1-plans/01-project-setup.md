# Plan 01 — Project Setup & Cargo Workspace

## Goal

Set up a Cargo workspace with properly separated crates that mirror the AgentOS architecture. Each major component gets its own crate for clear boundaries and independent compilation.

## Workspace Structure

```
agos/
├── Cargo.toml                    # Workspace root
├── Cargo.lock
├── .gitignore
├── README.md
├── config/
│   └── default.toml              # Default kernel config
│
├── crates/
│   ├── agentos-types/            # Shared types (IDs, messages, errors)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── ids.rs            # TaskID, AgentID, ToolID, etc.
│   │       ├── intent.rs         # IntentMessage, IntentType, IntentTarget
│   │       ├── capability.rs     # CapabilityToken, permissions
│   │       ├── task.rs           # AgentTask, TaskState
│   │       ├── context.rs        # ContextWindow, ContextEntry
│   │       ├── tool.rs           # ToolManifest, ToolID
│   │       ├── agent.rs          # AgentProfile, AgentStatus
│   │       ├── secret.rs         # SecretEntry, SecretScope
│   │       └── error.rs          # Error types
│   │
│   ├── agentos-audit/            # Append-only audit log
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       └── log.rs            # AuditLog, AuditEntry
│   │
│   ├── agentos-vault/            # Secrets vault
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── vault.rs          # SecretsVault struct
│   │       ├── crypto.rs         # AES-256-GCM encrypt/decrypt
│   │       └── master_key.rs     # Master key derivation
│   │
│   ├── agentos-capability/       # Capability tokens + permission matrix
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── token.rs          # CapabilityToken creation/validation
│   │       ├── permissions.rs    # PermissionMatrix, PermissionBit
│   │       └── engine.rs         # CapabilityEngine (check intents)
│   │
│   ├── agentos-bus/              # Intent Bus (IPC)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── channel.rs        # IntentChannel (sender/receiver)
│   │       ├── router.rs         # Intent routing logic
│   │       └── transport.rs      # Unix domain socket transport
│   │
│   ├── agentos-kernel/           # Inference Kernel
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── scheduler.rs      # TaskScheduler (priority queue)
│   │       ├── context.rs        # ContextManager
│   │       ├── kernel.rs         # Main Kernel struct and run loop
│   │       └── config.rs         # KernelConfig from TOML
│   │
│   ├── agentos-llm/              # LLM adapter trait + Ollama impl
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── traits.rs         # LLMCore trait definition
│   │       ├── ollama.rs         # OllamaCore implementation
│   │       └── types.rs          # InferenceResult, ModelCapabilities
│   │
│   ├── agentos-tools/            # Core tool implementations
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── traits.rs         # Tool trait definition
│   │       ├── loader.rs         # Tool manifest loader
│   │       ├── runner.rs         # Tool execution with process isolation
│   │       ├── file_reader.rs    # file-reader tool
│   │       ├── file_writer.rs    # file-writer tool
│   │       ├── memory_search.rs  # memory-search tool
│   │       ├── memory_write.rs   # memory-write tool
│   │       └── data_parser.rs    # data-parser tool
│   │
│   └── agentos-cli/              # agentctl CLI binary
│       ├── Cargo.toml
│       └── src/
│           ├── main.rs           # Entry point
│           ├── commands/
│           │   ├── mod.rs
│           │   ├── agent.rs      # agent connect, agent list
│           │   ├── task.rs       # task run, task list, task logs
│           │   ├── tool.rs       # tool install, tool list
│           │   ├── secret.rs     # secret set, secret list, secret revoke
│           │   ├── perm.rs       # perm grant, perm revoke, perm show
│           │   └── status.rs     # system status
│           └── client.rs         # Client that connects to kernel over UDS
│
├── tools/
│   └── core/                     # Tool manifest TOML files
│       ├── file-reader.toml
│       ├── file-writer.toml
│       ├── memory-search.toml
│       ├── memory-write.toml
│       └── data-parser.toml
│
└── tests/
    └── integration/
        ├── kernel_test.rs
        ├── vault_test.rs
        └── e2e_test.rs
```

## Workspace Cargo.toml

Create `agos/Cargo.toml`:

```toml
[workspace]
resolver = "2"
members = [
    "crates/agentos-types",
    "crates/agentos-audit",
    "crates/agentos-vault",
    "crates/agentos-capability",
    "crates/agentos-bus",
    "crates/agentos-kernel",
    "crates/agentos-llm",
    "crates/agentos-tools",
    "crates/agentos-cli",
]

[workspace.package]
version = "0.1.0"
edition = "2021"
license = "Apache-2.0"
repository = "https://github.com/agentos/agentos"

[workspace.dependencies]
# Async runtime
tokio = { version = "1", features = ["full"] }

# Serialization
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"

# Logging / tracing
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }

# Crypto
hmac = "0.12"
sha2 = "0.10"
aes-gcm = "0.10"
rand = "0.8"
zeroize = { version = "1", features = ["derive"] }
argon2 = "0.5"

# Database
rusqlite = { version = "0.31", features = ["bundled"] }

# CLI
clap = { version = "4", features = ["derive"] }

# HTTP client (for Ollama adapter)
reqwest = { version = "0.12", features = ["json"] }

# Error handling
thiserror = "2"
anyhow = "1"

# UUIDs
uuid = { version = "1", features = ["v4", "serde"] }

# Time
chrono = { version = "0.4", features = ["serde"] }

# Misc
bytes = "1"
futures = "0.3"
async-trait = "0.1"
```

## Crate Dependency Graph

```
agentos-types          (no internal deps — leaf crate)
     ↑
     ├── agentos-audit         (depends on: types)
     ├── agentos-vault         (depends on: types, audit)
     ├── agentos-capability    (depends on: types)
     ├── agentos-bus           (depends on: types)
     ├── agentos-llm           (depends on: types)
     ├── agentos-tools         (depends on: types, bus, capability)
     │
     └── agentos-kernel        (depends on: types, audit, vault, capability, bus, llm, tools)
              ↑
              └── agentos-cli  (depends on: types, kernel — or just connects over UDS)
```

## Individual Crate Cargo.toml Examples

### `crates/agentos-types/Cargo.toml`

```toml
[package]
name = "agentos-types"
version.workspace = true
edition.workspace = true

[dependencies]
serde = { workspace = true }
serde_json = { workspace = true }
uuid = { workspace = true }
chrono = { workspace = true }
thiserror = { workspace = true }
```

### `crates/agentos-kernel/Cargo.toml`

```toml
[package]
name = "agentos-kernel"
version.workspace = true
edition.workspace = true

[dependencies]
agentos-types = { path = "../agentos-types" }
agentos-audit = { path = "../agentos-audit" }
agentos-vault = { path = "../agentos-vault" }
agentos-capability = { path = "../agentos-capability" }
agentos-bus = { path = "../agentos-bus" }
agentos-llm = { path = "../agentos-llm" }
agentos-tools = { path = "../agentos-tools" }
tokio = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
serde = { workspace = true }
toml = { workspace = true }
anyhow = { workspace = true }
```

### `crates/agentos-cli/Cargo.toml`

```toml
[package]
name = "agentos-cli"
version.workspace = true
edition.workspace = true

[[bin]]
name = "agentctl"
path = "src/main.rs"

[dependencies]
agentos-types = { path = "../agentos-types" }
agentos-kernel = { path = "../agentos-kernel" }
tokio = { workspace = true }
clap = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
anyhow = { workspace = true }
```

## .gitignore

```
/target
**/*.rs.bk
*.swp
.env
/data/
/vault/
```

## Default Config File

Create `config/default.toml`:

```toml
[kernel]
max_concurrent_tasks = 16
default_task_timeout_secs = 300
context_window_max_entries = 100

[secrets]
vault_path = "/opt/agentos/vault/secrets.db"

[audit]
log_path = "/opt/agentos/data/audit.db"

[tools]
core_tools_dir = "/opt/agentos/tools/core"
user_tools_dir = "/opt/agentos/tools/user"
data_dir = "/opt/agentos/data"

[bus]
socket_path = "/tmp/agentos-kernel.sock"

[ollama]
host = "http://localhost:11434"
default_model = "llama3.2"
```

## Steps to Execute

1. Create the workspace root directory and `Cargo.toml`
2. Create each crate directory with its `Cargo.toml` and `src/lib.rs` (or `src/main.rs` for CLI)
3. Add empty module files with `// TODO` comments
4. Create `.gitignore`
5. Create `config/default.toml`
6. Create `tools/core/` directory with empty `.toml` manifests
7. Run `cargo check` to verify workspace compiles (everything will be empty stubs)
8. Run `cargo test` to verify no compilation errors

## Verification

```bash
cd agos
cargo check          # Should compile with no errors
cargo test           # Should pass (no tests yet, that's ok)
cargo build          # Should produce agentctl binary in target/debug/
```
