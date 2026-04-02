---
title: "Phase 2.1: Skills Abstraction"
tags:
  - kernel
  - skills
  - v3
  - plan
  - phase-2
date: 2026-03-30
status: planned
effort: 3d
priority: high
---

# Phase 2.1: Skills Abstraction

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Create `agentos-skills` crate with SKILL.toml manifest format, `SkillRegistry`, and CLI commands for skill install/remove/run/status.

**Architecture:** Skills are higher-level than tools. A skill = system prompt + tool set + trigger conditions (cron + events) + budget constraints. The `SkillRegistry` lives in the kernel, loads skills from `skills/core/` and `skills/user/` at boot, arms triggers via the existing `ScheduleManager` and `EventBus`, and spawns temporary agents on trigger.

**Tech Stack:** toml (manifest parsing), agentos-kernel (SkillRegistry integration), agentos-types (SkillManifest type)

---

## Why This Phase

AgentOS has 60+ tools but no concept of autonomous agents that run on schedules. OpenFang ships 7 "Hands" — pre-built autonomous capability packages. OpenClaw has 100+ skills. Without skills, AgentOS is a tool library, not an agent platform.

## Current → Target State

**Current:** Tools (single operations) + manual task execution via `agentctl task run`. No scheduled agent workflows. No skill manifest format.

**Target:** Skills (autonomous capabilities) that run on cron schedules and event triggers. `SKILL.toml` defines the skill. `SkillRegistry` manages lifecycle. `agentctl skill install/run/list/status` CLI commands.

## Files Changed

| File | Action | Purpose |
|------|--------|---------|
| `crates/agentos-skills/Cargo.toml` | Create | New crate manifest |
| `crates/agentos-skills/src/lib.rs` | Create | Crate root, re-exports |
| `crates/agentos-skills/src/manifest.rs` | Create | SKILL.toml parsing and validation |
| `crates/agentos-skills/src/registry.rs` | Create | SkillRegistry: load, install, remove, trigger |
| `crates/agentos-types/src/skill.rs` | Create | SkillManifest type definition |
| `crates/agentos-types/src/lib.rs` | Modify | Add `pub mod skill;` |
| `crates/agentos-bus/src/message.rs` | Modify | Add skill KernelCommand variants |
| `crates/agentos-kernel/src/commands/skill.rs` | Create | Skill command handler |
| `crates/agentos-kernel/src/commands/mod.rs` | Modify | Add skill module |
| `crates/agentos-kernel/src/kernel.rs` | Modify | Add SkillRegistry field |
| `crates/agentos-cli/src/commands/skill.rs` | Create | CLI skill subcommands |
| `Cargo.toml` (workspace) | Modify | Add agentos-skills member |
| `skills/core/` | Create | Directory for bundled skills |

## Dependencies

- **Requires:** Nothing — this is a root phase
- **Blocks:** Phase 2.2 (Pre-built Agents), Phase 1.3 (Marketplace)

---

## Detailed Tasks

### Task 1: SkillManifest Type

**Files:**
- Create: `crates/agentos-types/src/skill.rs`
- Modify: `crates/agentos-types/src/lib.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_skill_manifest_toml() {
        let toml_str = r#"
[skill]
name = "test-skill"
version = "0.1.0"
description = "A test skill"
author = "test"
trust_tier = "core"

[triggers]
schedule = "0 */6 * * *"
events = ["task_completed"]

[agent]
system_prompt_file = "prompt.md"
roles = ["general"]
default_provider = "ollama"
default_model = "llama3"

[tools]
required = ["memory-read", "notify-user"]
optional = ["http-client"]

[permissions]
required = ["memory:read", "notification:write"]

[budget]
max_cost_per_run = 0.50
max_tokens_per_run = 50000
"#;
        let manifest: SkillManifest = toml::from_str(toml_str).unwrap();
        assert_eq!(manifest.skill.name, "test-skill");
        assert_eq!(manifest.triggers.events, vec!["task_completed"]);
        assert_eq!(manifest.tools.required, vec!["memory-read", "notify-user"]);
        assert_eq!(manifest.budget.max_cost_per_run, 0.50);
    }
}
```

- [ ] **Step 2: Implement SkillManifest**

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillManifest {
    pub skill: SkillInfo,
    #[serde(default)]
    pub triggers: SkillTriggers,
    pub agent: SkillAgent,
    #[serde(default)]
    pub tools: SkillTools,
    #[serde(default)]
    pub permissions: SkillPermissions,
    #[serde(default)]
    pub budget: SkillBudget,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillInfo {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    #[serde(default = "default_trust_tier")]
    pub trust_tier: String,
    #[serde(default)]
    pub license: Option<String>,
}

fn default_trust_tier() -> String { "community".to_string() }

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillTriggers {
    #[serde(default)]
    pub schedule: Option<String>,
    #[serde(default)]
    pub events: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillAgent {
    pub system_prompt_file: String,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub default_provider: Option<String>,
    #[serde(default)]
    pub default_model: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillTools {
    #[serde(default)]
    pub required: Vec<String>,
    #[serde(default)]
    pub optional: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillPermissions {
    #[serde(default)]
    pub required: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillBudget {
    #[serde(default = "default_max_cost")]
    pub max_cost_per_run: f64,
    #[serde(default = "default_max_tokens")]
    pub max_tokens_per_run: u64,
}

fn default_max_cost() -> f64 { 1.0 }
fn default_max_tokens() -> u64 { 100000 }

impl Default for SkillBudget {
    fn default() -> Self {
        Self {
            max_cost_per_run: default_max_cost(),
            max_tokens_per_run: default_max_tokens(),
        }
    }
}
```

- [ ] **Step 3: Run test, verify pass**

Run: `cargo test -p agentos-types -- test_parse_skill_manifest`

- [ ] **Step 4: Commit**

```bash
git add crates/agentos-types/src/skill.rs crates/agentos-types/src/lib.rs
git commit -m "feat(types): add SkillManifest type with TOML deserialization"
```

### Task 2: Scaffold `agentos-skills` Crate

**Files:**
- Create: `crates/agentos-skills/Cargo.toml`
- Create: `crates/agentos-skills/src/lib.rs`
- Create: `crates/agentos-skills/src/manifest.rs`

- [ ] **Step 1: Write Cargo.toml, lib.rs, manifest.rs (loader)**

`manifest.rs` loads and validates `SKILL.toml` + `prompt.md` from a directory:
```rust
use agentos_types::skill::SkillManifest;
use agentos_types::AgentOSError;
use std::path::Path;

pub fn load_skill_from_dir(dir: &Path) -> Result<(SkillManifest, String), AgentOSError> {
    let manifest_path = dir.join("SKILL.toml");
    let prompt_path_field;
    let manifest_str = std::fs::read_to_string(&manifest_path)
        .map_err(|e| AgentOSError::ConfigError(format!("Cannot read SKILL.toml: {}", e)))?;
    let manifest: SkillManifest = toml::from_str(&manifest_str)
        .map_err(|e| AgentOSError::ConfigError(format!("Invalid SKILL.toml: {}", e)))?;

    let prompt_path = dir.join(&manifest.agent.system_prompt_file);
    let prompt = std::fs::read_to_string(&prompt_path)
        .map_err(|e| AgentOSError::ConfigError(format!("Cannot read prompt file: {}", e)))?;

    Ok((manifest, prompt))
}
```

- [ ] **Step 2: Verify compilation**

Run: `cargo build -p agentos-skills`

- [ ] **Step 3: Commit**

```bash
git add crates/agentos-skills/ Cargo.toml
git commit -m "feat(skills): scaffold crate with manifest loader"
```

### Task 3: SkillRegistry

**Files:**
- Create: `crates/agentos-skills/src/registry.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_test_skill(dir: &std::path::Path) {
        std::fs::write(dir.join("SKILL.toml"), r#"
[skill]
name = "test-skill"
version = "0.1.0"
description = "test"
author = "test"
trust_tier = "core"
[triggers]
schedule = "0 * * * *"
events = []
[agent]
system_prompt_file = "prompt.md"
[tools]
required = []
[permissions]
required = []
[budget]
max_cost_per_run = 1.0
max_tokens_per_run = 10000
"#).unwrap();
        std::fs::write(dir.join("prompt.md"), "You are a test agent.").unwrap();
    }

    #[test]
    fn test_load_skills_from_directory() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("test-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        write_test_skill(&skill_dir);

        let mut registry = SkillRegistry::new();
        let count = registry.load_from_dir(tmp.path()).unwrap();
        assert_eq!(count, 1);
        assert_eq!(registry.list().len(), 1);
        assert_eq!(registry.list()[0].skill.name, "test-skill");
    }

    #[test]
    fn test_remove_skill() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("test-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        write_test_skill(&skill_dir);

        let mut registry = SkillRegistry::new();
        registry.load_from_dir(tmp.path()).unwrap();
        assert!(registry.remove("test-skill").is_ok());
        assert!(registry.list().is_empty());
    }
}
```

- [ ] **Step 2: Implement SkillRegistry**

```rust
use crate::manifest::load_skill_from_dir;
use agentos_types::skill::SkillManifest;
use agentos_types::AgentOSError;
use std::collections::HashMap;
use std::path::Path;
use tracing::info;

pub struct InstalledSkill {
    pub manifest: SkillManifest,
    pub system_prompt: String,
}

pub struct SkillRegistry {
    skills: HashMap<String, InstalledSkill>,
}

impl SkillRegistry {
    pub fn new() -> Self {
        Self { skills: HashMap::new() }
    }

    pub fn load_from_dir(&mut self, base_dir: &Path) -> Result<usize, AgentOSError> {
        let mut count = 0;
        if !base_dir.exists() { return Ok(0); }
        for entry in std::fs::read_dir(base_dir)
            .map_err(|e| AgentOSError::ConfigError(e.to_string()))?
        {
            let entry = entry.map_err(|e| AgentOSError::ConfigError(e.to_string()))?;
            let path = entry.path();
            if path.is_dir() && path.join("SKILL.toml").exists() {
                match load_skill_from_dir(&path) {
                    Ok((manifest, prompt)) => {
                        let name = manifest.skill.name.clone();
                        self.skills.insert(name.clone(), InstalledSkill {
                            manifest,
                            system_prompt: prompt,
                        });
                        info!("Loaded skill: {}", name);
                        count += 1;
                    }
                    Err(e) => {
                        tracing::warn!("Failed to load skill from {:?}: {}", path, e);
                    }
                }
            }
        }
        Ok(count)
    }

    pub fn install(&mut self, manifest: SkillManifest, prompt: String) -> Result<(), AgentOSError> {
        let name = manifest.skill.name.clone();
        self.skills.insert(name, InstalledSkill { manifest, system_prompt: prompt });
        Ok(())
    }

    pub fn remove(&mut self, name: &str) -> Result<(), AgentOSError> {
        self.skills.remove(name).ok_or_else(|| {
            AgentOSError::ConfigError(format!("skill '{}' not found", name))
        })?;
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&InstalledSkill> {
        self.skills.get(name)
    }

    pub fn list(&self) -> Vec<&SkillManifest> {
        self.skills.values().map(|s| &s.manifest).collect()
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p agentos-skills`
Expected: All pass

- [ ] **Step 4: Commit**

```bash
git add crates/agentos-skills/src/registry.rs
git commit -m "feat(skills): add SkillRegistry with load, install, remove"
```

### Task 4: Kernel Integration and CLI Commands

**Files:**
- Modify: `crates/agentos-kernel/src/kernel.rs`
- Create: `crates/agentos-kernel/src/commands/skill.rs`
- Modify: `crates/agentos-bus/src/message.rs`
- Create: `crates/agentos-cli/src/commands/skill.rs`

- [ ] **Step 1: Add KernelCommand variants for skills**

In `message.rs`:
```rust
// Skills management
SkillInstall { path: String },
SkillRemove { name: String },
SkillList,
SkillRun { name: String, input: Option<String> },
SkillStatus { name: String },
```

Add corresponding `KernelResponse` variants:
```rust
SkillList(Vec<serde_json::Value>),
SkillRunResult { task_id: String },
SkillStatusInfo(serde_json::Value),
```

- [ ] **Step 2: Add SkillRegistry to Kernel**

In `kernel.rs`, add `skill_registry: Arc<RwLock<agentos_skills::registry::SkillRegistry>>` and load from `skills/core/` + `skills/user/` during boot.

- [ ] **Step 3: Write skill command handler**

In `commands/skill.rs`, handle each variant by calling SkillRegistry methods. For `SkillRun`, create a temporary agent with the skill's prompt and tools, then submit a task.

- [ ] **Step 4: Write CLI subcommands**

In `crates/agentos-cli/src/commands/skill.rs`:
```rust
#[derive(clap::Subcommand)]
pub enum SkillCmd {
    Install { path: String },
    Remove { name: String },
    List,
    Run { name: String, #[arg(long)] input: Option<String> },
    Status { name: String },
}
```

- [ ] **Step 5: Build and test**

Run: `cargo build --workspace && cargo test --workspace`

- [ ] **Step 6: Commit**

```bash
git add crates/agentos-kernel/ crates/agentos-bus/ crates/agentos-cli/ crates/agentos-skills/
git commit -m "feat(skills): wire SkillRegistry into kernel with CLI commands"
```

---

## Test Plan

| Test | Assertion |
|------|-----------|
| SKILL.toml parsing | All fields deserialize correctly |
| Load from directory | Finds and loads skills, counts correct |
| Missing SKILL.toml | Skipped with warning, no error |
| Install + remove | Registry count changes |
| Skill trigger → agent task | Correct system prompt and tools used |

## Verification

```bash
cargo build --workspace
cargo test -p agentos-skills
cargo test -p agentos-types -- test_parse_skill
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```
