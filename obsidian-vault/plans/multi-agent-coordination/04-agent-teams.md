---
title: "Phase 04 — Agent Teams"
tags:
  - kernel
  - agents
  - teams
  - v4
  - plan
date: 2026-04-02
status: planned
effort: 1.5d
priority: high
---

# Phase 04 — Agent Teams

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> Build on the existing `CreateAgentGroup` + `BroadcastToGroup` commands to add coordinator/worker roles, shared context namespaces, and a `run_team` CLI command that executes a named team configuration.

---

## Why This Phase

Phases 1–3 give individual agents the ability to spawn and await sub-agents ad-hoc. Teams are the structured version: a named group with declared roles (one coordinator, N workers), a shared memory namespace, and a single entry point (`run_team`). This is what makes multi-agent coordination reusable and inspectable.

---

## Current → Target State

| Aspect | Current | Target |
|--------|---------|--------|
| `AgentMessageBus` groups | Name + member list only | Groups have a `coordinator_id`, `role` per member |
| Shared context | None | `TeamContext` — a shared context namespace scoped to the group |
| CLI | No team commands | `agentos team create`, `agentos team run`, `agentos team list` |
| Team config | None | `TeamConfig` struct; loadable from TOML |

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-types/src/team.rs` | New — `TeamConfig`, `TeamMember`, `TeamRole` |
| `crates/agentos-bus/src/message.rs` | Add `RunTeam`, `TeamStatus` commands; extend `CreateAgentGroup` |
| `crates/agentos-kernel/src/commands/team.rs` | New — `cmd_run_team()`, `cmd_team_status()` |
| `crates/agentos-kernel/src/run_loop.rs` | Dispatch arms for team commands |
| `crates/agentos-cli/src/commands/team.rs` | New — `team create`, `team run`, `team list` subcommands |

---

## Detailed Tasks

### Task 1: Add `TeamConfig` types

**Files:**
- Create: `crates/agentos-types/src/team.rs`

- [ ] **Step 1: Create the file**

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TeamRole {
    Coordinator,
    Worker,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMember {
    pub agent_name: String,
    pub role: TeamRole,
    /// Extra prompt context added to this member's system prompt.
    #[serde(default)]
    pub role_description: String,
}

/// Declarative configuration for an agent team.
/// Can be loaded from a TOML file or constructed programmatically.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamConfig {
    pub name: String,
    pub goal: String,
    pub members: Vec<TeamMember>,
    /// Maximum number of coordinator↔worker rounds before the team is forced to conclude.
    #[serde(default = "default_max_rounds")]
    pub max_rounds: u32,
}

fn default_max_rounds() -> u32 { 10 }

impl TeamConfig {
    /// Load a team config from a TOML string.
    pub fn from_toml(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }

    /// Return the coordinator member, if any.
    pub fn coordinator(&self) -> Option<&TeamMember> {
        self.members.iter().find(|m| matches!(m.role, TeamRole::Coordinator))
    }

    /// Return all worker members.
    pub fn workers(&self) -> Vec<&TeamMember> {
        self.members
            .iter()
            .filter(|m| matches!(m.role, TeamRole::Worker))
            .collect()
    }
}
```

- [ ] **Step 2: Add `mod team` to `agentos-types/src/lib.rs`**

```rust
pub mod team;
pub use team::{TeamConfig, TeamMember, TeamRole};
```

- [ ] **Step 3: Write the test**

```rust
#[test]
fn test_team_config_from_toml() {
    let toml = r#"
        name = "research-team"
        goal = "Research and summarize the topic"

        [[members]]
        agent_name = "planner"
        role = "Coordinator"
        role_description = "Breaks the goal into subtasks"

        [[members]]
        agent_name = "researcher"
        role = "Worker"
        role_description = "Searches and retrieves information"
    "#;

    let config = TeamConfig::from_toml(toml).unwrap();
    assert_eq!(config.name, "research-team");
    assert!(config.coordinator().is_some());
    assert_eq!(config.workers().len(), 1);
    assert_eq!(config.max_rounds, 10); // default
}
```

- [ ] **Step 4: Run it**

```bash
cargo test -p agentos-types test_team_config_from_toml
```

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/agentos-types/src/team.rs crates/agentos-types/src/lib.rs
git commit -m "feat(types): add TeamConfig, TeamMember, TeamRole for agent teams"
```

---

### Task 2: Add `RunTeam` kernel command

**Files:**
- Modify: `crates/agentos-bus/src/message.rs`
- Create: `crates/agentos-kernel/src/commands/team.rs`
- Modify: `crates/agentos-kernel/src/run_loop.rs`

- [ ] **Step 1: Add `RunTeam` to `KernelCommand`**

```rust
/// Execute a named agent team against a goal.
RunTeam {
    /// Inline team config (JSON-encoded `TeamConfig`).
    config: String,
},

/// Get the current status of a running team task.
TeamStatus {
    team_task_id: TaskID,
},
```

Add to `KernelResponse`:

```rust
TeamStarted {
    coordinator_task_id: TaskID,
    worker_task_ids: Vec<TaskID>,
},
```

- [ ] **Step 2: Create `commands/team.rs`**

```rust
use crate::Kernel;
use agentos_bus::KernelResponse;
use agentos_types::{TeamConfig, TaskID};

impl Kernel {
    pub async fn cmd_run_team(&self, config_json: &str) -> KernelResponse {
        let config: TeamConfig = match serde_json::from_str(config_json) {
            Ok(c) => c,
            Err(e) => return KernelResponse::Error(format!("invalid team config: {}", e)),
        };

        let coordinator = match config.coordinator() {
            Some(c) => c.clone(),
            None => return KernelResponse::Error("team has no coordinator".to_string()),
        };

        // Build the coordinator's prompt: goal + worker roster.
        let worker_names: Vec<&str> = config.workers().iter().map(|w| w.agent_name.as_str()).collect();
        let coordinator_prompt = format!(
            "You are the coordinator for team '{}'. Goal: {}\n\n\
             Available workers: {}\n\
             Use spawn_agent to delegate subtasks. Aggregate results with await_agents. \
             Produce a final consolidated response when done.",
            config.name,
            config.goal,
            worker_names.join(", ")
        );

        // Spawn the coordinator task — it will spawn workers via the spawn_agent tool.
        let fake_parent = TaskID::new(); // root-level, no parent
        let resp = self
            .cmd_spawn_sub_agent(
                fake_parent,
                &coordinator.agent_name,
                &coordinator_prompt,
                &["read", "write", "spawn"],
                None,
            )
            .await;

        match resp {
            KernelResponse::SubAgentSpawned { child_task_id } => {
                KernelResponse::TeamStarted {
                    coordinator_task_id: child_task_id,
                    worker_task_ids: vec![], // workers spawned dynamically by coordinator
                }
            }
            other => other,
        }
    }
}
```

- [ ] **Step 3: Add dispatch in `run_loop.rs`**

```rust
KernelCommand::RunTeam { config } => {
    self.cmd_run_team(&config).await
}
```

- [ ] **Step 4: Register in `commands/mod.rs`**

```rust
pub mod team;
```

- [ ] **Step 5: Build**

```bash
cargo build -p agentos-kernel 2>&1 | grep "^error"
```

- [ ] **Step 6: Commit**

```bash
git add crates/agentos-bus/src/message.rs \
        crates/agentos-kernel/src/commands/team.rs \
        crates/agentos-kernel/src/commands/mod.rs \
        crates/agentos-kernel/src/run_loop.rs
git commit -m "feat(kernel): add RunTeam command — spawn coordinator that delegates to workers"
```

---

### Task 3: Add `team` CLI subcommands

**Files:**
- Create: `crates/agentos-cli/src/commands/team.rs`
- Modify: `crates/agentos-cli/src/commands/mod.rs` and root CLI

- [ ] **Step 1: Read how an existing CLI command is structured**

Read `crates/agentos-cli/src/commands/pipeline.rs` for the pattern.

- [ ] **Step 2: Create `team.rs`**

```rust
use clap::Subcommand;

#[derive(Subcommand)]
pub enum TeamCommand {
    /// Run a team defined in a TOML config file against a goal.
    Run {
        /// Path to the team TOML config file.
        #[arg(short, long)]
        config: String,
    },
    /// List active team runs (coordinator tasks).
    List,
}

pub async fn handle(cmd: TeamCommand, client: &dyn BusClient) -> anyhow::Result<()> {
    match cmd {
        TeamCommand::Run { config } => {
            let config_str = std::fs::read_to_string(&config)
                .map_err(|e| anyhow::anyhow!("failed to read config: {}", e))?;
            // Parse as TOML → re-encode as JSON for the bus message.
            let team_config: agentos_types::TeamConfig =
                toml::from_str(&config_str)?;
            let config_json = serde_json::to_string(&team_config)?;

            let resp = client
                .send(agentos_bus::KernelCommand::RunTeam { config: config_json })
                .await?;

            match resp {
                agentos_bus::KernelResponse::TeamStarted { coordinator_task_id, .. } => {
                    println!("Team started. Coordinator task: {}", coordinator_task_id);
                }
                agentos_bus::KernelResponse::Error(e) => {
                    eprintln!("Error: {}", e);
                    std::process::exit(1);
                }
                _ => {}
            }
        }
        TeamCommand::List => {
            let resp = client
                .send(agentos_bus::KernelCommand::ListTasks)
                .await?;
            // Filter and display team coordinator tasks.
            println!("{:?}", resp);
        }
    }
    Ok(())
}
```

- [ ] **Step 3: Register in the CLI root**

Add `team` as a subcommand group in the same way `pipeline` or `agent` is added.

- [ ] **Step 4: Build**

```bash
cargo build -p agentos-cli 2>&1 | grep "^error"
```

- [ ] **Step 5: Smoke test the CLI compiles**

```bash
cargo run -p agentos-cli -- team --help
```

Expected: prints `team run` and `team list` subcommands

- [ ] **Step 6: Run full suite**

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```

- [ ] **Step 7: Commit**

```bash
git add crates/agentos-cli/src/commands/team.rs crates/agentos-cli/src/commands/mod.rs \
        crates/agentos-cli/src/
git commit -m "feat(cli): add 'agentos team run/list' commands"
```

---

## Verification

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
agentos team --help
```

## Dependencies

- Requires: [[01-sub-agent-spawning]], [[02-context-handoff]], [[03-coordination-tools]]
- Blocks: Nothing — this is the final phase
