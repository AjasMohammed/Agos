# Plan 05 — Advanced Permissions & Memory Architecture

## Goal

Extend the V1 permission system with **permission profiles** (reusable permission sets, like Unix groups) and **time-limited permissions** (auto-expiring grants). Upgrade the memory system from simple SQLite FTS5 to a proper **three-tier architecture**: working memory (in-memory), episodic memory (per-task SQLite), and vector-based semantic memory.

## Dependencies

- `agentos-types`, `agentos-capability` (existing)
- `agentos-kernel` (existing, modifications)
- `rusqlite` (existing)
- `chrono` (existing)

## Part A: Advanced Permission Features

### Permission Profiles

A permission profile is a named, reusable set of permissions — analogous to a Unix group:

```rust
// In agentos-capability/src/profiles.rs

use agentos_types::*;
use std::collections::HashMap;
use std::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionProfile {
    pub name: String,
    pub description: String,
    pub permissions: PermissionSet,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

pub struct ProfileManager {
    profiles: RwLock<HashMap<String, PermissionProfile>>,
}

impl ProfileManager {
    pub fn new() -> Self;

    /// Create a new named profile.
    pub fn create(
        &self,
        name: &str,
        description: &str,
        permissions: PermissionSet,
    ) -> Result<(), AgentOSError>;

    /// Delete a profile.
    pub fn delete(&self, name: &str) -> Result<(), AgentOSError>;

    /// Get a profile by name.
    pub fn get(&self, name: &str) -> Option<PermissionProfile>;

    /// List all profiles.
    pub fn list_all(&self) -> Vec<PermissionProfile>;

    /// Assign a profile to an agent — merges profile permissions into the agent's set.
    pub fn assign_to_agent(
        &self,
        profile_name: &str,
        capability_engine: &CapabilityEngine,
        agent_id: &AgentID,
    ) -> Result<(), AgentOSError>;
}
```

### CLI for Profiles

```bash
# Create a reusable permission profile
agentctl perm profile create "ops-agent" \
  --description "Standard permissions for operations agents" \
  --grant "network.logs:r,process.list:r,fs.app_logs:r,hardware.system:r"

# List profiles
agentctl perm profile list

# Assign profile to an agent
agentctl perm profile assign analyst ops-agent

# Delete a profile
agentctl perm profile delete ops-agent
```

### Time-Limited Permissions

Permissions can now have an optional expiry:

```rust
// Update agentos-types/src/capability.rs

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionEntry {
    pub resource: String,
    pub read: bool,
    pub write: bool,
    pub execute: bool,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,  // NEW: optional expiry
}
```

The `CapabilityEngine` is updated to check expiry during `validate_intent`:

```rust
// In validate_intent(), after checking the permission exists:
if let Some(expires_at) = entry.expires_at {
    if chrono::Utc::now() > expires_at {
        // Permission has expired — auto-revoke it
        return Err(AgentOSError::PermissionDenied {
            resource: resource.clone(),
            operation: "Permission expired".into(),
        });
    }
}
```

### CLI for Time-Limited Permissions

```bash
# Grant permission with expiry
agentctl perm grant analyst fs.system_logs:r --expires 2h
agentctl perm grant researcher hardware.gpu:rx --expires 24h

# Show permissions with expiry info
agentctl perm show analyst
# RESOURCE              R    W    X    EXPIRES
# network.logs          ✓    -    -    never
# fs.system_logs        ✓    -    -    1h 45m remaining
# hardware.gpu          -    -    -    (denied)
```

### New KernelCommand Variants

```rust
pub enum KernelCommand {
    // ... existing ...
    CreatePermProfile { name: String, description: String, permissions: Vec<String> },
    DeletePermProfile { name: String },
    ListPermProfiles,
    AssignPermProfile { agent_name: String, profile_name: String },
    GrantPermissionTimed { agent_name: String, permission: String, expires_secs: u64 },
}
```

---

## Part B: Three-Tier Memory Architecture

### Tier 1: Working Memory (Already Exists)

The current `ContextWindow` in-memory ring buffer. No changes needed — this is already functional from V1.

### Tier 2: Episodic Memory (New)

Per-task persistent memory that survives context window eviction. Every intent message, tool call, LLM response, and agent message for a task is stored in a SQLite database indexed by task ID.

```rust
// In agentos-kernel/src/episodic_memory.rs

use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub struct EpisodicMemory {
    db: Mutex<Connection>,
}

impl EpisodicMemory {
    /// Open or create the episodic memory database.
    pub fn open(data_dir: &Path) -> Result<Self, AgentOSError> {
        let db_path = data_dir.join("episodic_memory.db");
        let conn = Connection::open(&db_path)?;
        conn.execute_batch("
            CREATE TABLE IF NOT EXISTS episodes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                task_id TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                entry_type TEXT NOT NULL,  -- 'intent', 'tool_call', 'llm_response', 'agent_message'
                content TEXT NOT NULL,
                metadata TEXT,            -- JSON metadata
                timestamp TEXT NOT NULL,
                trace_id TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_episodes_task ON episodes(task_id);
            CREATE INDEX IF NOT EXISTS idx_episodes_agent ON episodes(agent_id);
            CREATE INDEX IF NOT EXISTS idx_episodes_timestamp ON episodes(timestamp);
        ")?;
        Ok(Self { db: Mutex::new(conn) })
    }

    /// Record an episode entry.
    pub fn record(
        &self,
        task_id: &TaskID,
        agent_id: &AgentID,
        entry_type: EpisodeType,
        content: &str,
        metadata: Option<serde_json::Value>,
        trace_id: &TraceID,
    ) -> Result<(), AgentOSError>;

    /// Query episodes for a specific task.
    pub fn query_by_task(
        &self,
        task_id: &TaskID,
        limit: u32,
    ) -> Result<Vec<Episode>, AgentOSError>;

    /// Query all episodes for an agent across tasks.
    pub fn query_by_agent(
        &self,
        agent_id: &AgentID,
        limit: u32,
    ) -> Result<Vec<Episode>, AgentOSError>;

    /// Search episodes by content (full-text).
    pub fn search(
        &self,
        query: &str,
        agent_id: Option<&AgentID>,
        limit: u32,
    ) -> Result<Vec<Episode>, AgentOSError>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EpisodeType {
    Intent,
    ToolCall,
    ToolResult,
    LLMResponse,
    AgentMessage,
    UserPrompt,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Episode {
    pub id: i64,
    pub task_id: TaskID,
    pub agent_id: AgentID,
    pub entry_type: EpisodeType,
    pub content: String,
    pub metadata: Option<serde_json::Value>,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub trace_id: TraceID,
}
```

### Kernel Integration

During task execution, the kernel automatically records episodes:

```rust
// In kernel.rs execute_task():

// After each LLM call:
self.episodic_memory.record(
    &task.id, &task.agent_id, EpisodeType::LLMResponse,
    &llm_result.text, None, &trace_id,
)?;

// After each tool call:
self.episodic_memory.record(
    &task.id, &task.agent_id, EpisodeType::ToolCall,
    &format!("{}({})", tool_name, payload), None, &trace_id,
)?;

// After each tool result:
self.episodic_memory.record(
    &task.id, &task.agent_id, EpisodeType::ToolResult,
    &result.to_string(), None, &trace_id,
)?;
```

### Tier 3: Semantic Memory (Upgrade from FTS5)

The V1 semantic memory uses SQLite FTS5 for keyword search. In V2, we add a **similarity scoring** layer on top — still using SQLite but with TF-IDF based ranking for better retrieval. True vector embeddings are deferred to Phase 3.

The upgrade is transparent to the `memory-search` tool — the API stays the same, but results are ranked better using FTS5's built-in `bm25()` scoring function:

```rust
// Updated query in memory_search.rs
let mut stmt = conn.prepare(
    "SELECT content, source, tags, created_at, bm25(memory) as score
     FROM memory
     WHERE memory MATCH ?1
     ORDER BY score
     LIMIT ?2"
)?;
```

Additionally, add a `scope` column so that memory entries can be scoped per-agent:

```sql
-- Migration for existing semantic_memory.db
ALTER TABLE memory ADD COLUMN agent_scope TEXT DEFAULT 'global';
```

Agents with `memory.semantic:r` can read global entries. Entries scoped to a specific agent require that agent's ID to match.

## Permission Resources for Memory

```
# New permission resources
memory.episodic:r    — read own task history (episodic tier)
memory.episodic:rw   — read/write episodic memory
memory.semantic:r    — read global semantic memory
memory.semantic:rw   — read/write semantic memory
```

## Tests

```rust
#[test]
fn test_permission_profile_create_and_assign() {
    let manager = ProfileManager::new();
    let mut perms = PermissionSet::new();
    perms.grant("network.logs", true, false, false, None);
    perms.grant("process.list", true, false, false, None);

    manager.create("ops", "Ops profile", perms.clone()).unwrap();
    let profile = manager.get("ops").unwrap();
    assert_eq!(profile.permissions.entries().len(), 2);
}

#[test]
fn test_time_limited_permission_expires() {
    let engine = CapabilityEngine::new();
    let agent_id = AgentID::new();
    let mut perms = PermissionSet::new();
    // Grant with expiry in the past
    let expired = chrono::Utc::now() - chrono::Duration::seconds(10);
    perms.grant("fs.system_logs", true, false, false, Some(expired));
    engine.register_agent(agent_id, perms);

    // Should fail — permission has expired
    let result = engine.validate_intent(/* ... */);
    assert!(result.is_err());
}

#[tokio::test]
async fn test_episodic_memory_record_and_query() {
    let dir = tempfile::TempDir::new().unwrap();
    let mem = EpisodicMemory::open(dir.path()).unwrap();
    let task_id = TaskID::new();
    let agent_id = AgentID::new();
    let trace_id = TraceID::new();

    mem.record(&task_id, &agent_id, EpisodeType::UserPrompt, "Hello", None, &trace_id).unwrap();
    mem.record(&task_id, &agent_id, EpisodeType::LLMResponse, "Hi there", None, &trace_id).unwrap();

    let episodes = mem.query_by_task(&task_id, 10).unwrap();
    assert_eq!(episodes.len(), 2);
}

#[tokio::test]
async fn test_semantic_memory_scoped_to_agent() {
    // Write a scoped memory entry, verify only that agent can find it
}
```

## Verification

```bash
cargo test -p agentos-capability    # profile + timed permission tests
cargo test -p agentos-kernel         # episodic memory tests
cargo test -p agentos-tools          # updated memory tool tests
```
