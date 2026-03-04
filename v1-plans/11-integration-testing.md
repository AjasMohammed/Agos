# Plan 11 — Integration Testing

## Goal

Validate that all Phase 1 components work together end-to-end. This plan defines the test scenarios, how to run them, and what success looks like.

## Test Prerequisites

1. **Rust toolchain**: `cargo` available and working
2. **Ollama**: Running locally on `http://localhost:11434` with `qwen3:1.7b` pulled
3. **No external services required** — everything runs locally

### Setup Ollama

```bash
# Install Ollama (if not installed)
curl -fsSL https://ollama.com/install.sh | sh

# Pull the test model
ollama pull llama3.2

# Verify it's running
curl http://localhost:11434/api/tags
```

---

## Test 1: Unit Test Suite (All Crates)

**What it tests:** Every crate's internal logic in isolation.

```bash
cargo test --workspace
```

**Expected output:** All tests pass. Zero failures.

**Covers:**

- `agentos-types`: ID generation, context window eviction, permission checking
- `agentos-audit`: Append and query operations on audit log
- `agentos-vault`: Encrypt/decrypt, passphrase verification, revoke, rotate
- `agentos-capability`: Token signing, validation, tamper detection, expiry
- `agentos-bus`: Client-server round-trip over Unix domain sockets
- `agentos-llm`: Context-to-messages conversion
- `agentos-tools`: File read/write, memory search/write, CSV/JSON/TOML parsing, path traversal protection
- `agentos-cli`: Argument parsing for all commands

---

## Test 2: Kernel Boot & Shutdown

**What it tests:** The kernel can start, initialize all subsystems, and shut down cleanly.

### Automated Test

```rust
// tests/integration/kernel_boot_test.rs

#[tokio::test]
async fn test_kernel_boots_and_shuts_down() {
    let temp_dir = tempfile::TempDir::new().unwrap();

    // Create minimal config pointing to temp dir
    let config = create_test_config(&temp_dir);
    let config_path = temp_dir.path().join("config.toml");
    std::fs::write(&config_path, toml::to_string(&config).unwrap()).unwrap();

    // Create required directories
    std::fs::create_dir_all(temp_dir.path().join("data")).unwrap();
    std::fs::create_dir_all(temp_dir.path().join("vault")).unwrap();
    std::fs::create_dir_all(temp_dir.path().join("tools/core")).unwrap();

    // Boot kernel
    let kernel = Arc::new(
        Kernel::boot(&config_path, "test-passphrase").await.unwrap()
    );

    // Verify audit log has KernelStarted event
    let logs = kernel.audit.query_recent(10).unwrap();
    assert!(logs.iter().any(|e| matches!(e.event_type, AuditEventType::KernelStarted)));

    // Verify tool registry loaded
    let tools = kernel.tool_registry.read().await;
    assert!(tools.list_all().len() >= 5, "Should have at least 5 core tools");

    // Clean shutdown
    // (kernel.shutdown() or just dropping should clean up)
}
```

### Manual Test

```bash
# Terminal 1: Start kernel
cargo run -p agentos-cli -- start
# Expected: "🚀 Booting AgentOS kernel..." followed by "✅ Kernel started"
# Should show tool count and bus socket path

# Terminal 2: Check status
cargo run -p agentos-cli -- status
# Expected: Shows uptime, 0 agents, 0 tasks, 5 tools
# If this works, the bus connection is working

# Terminal 1: Press Ctrl+C
# Expected: Clean shutdown, no panics
```

---

## Test 3: Agent Connection & Disconnection

**What it tests:** Connecting a local Ollama model, verifying it appears in the agent list, and disconnecting it.

### Manual Test (requires Ollama)

```bash
# Terminal 1: Start kernel
cargo run -p agentos-cli -- start

# Terminal 2: Connect Ollama agent
cargo run -p agentos-cli -- agent connect --provider ollama --model llama3.2 --name analyst
# Expected: "✅ Agent 'analyst' connected (ollama/llama3.2)"

# Verify agent appears in list
cargo run -p agentos-cli -- agent list
# Expected: Shows analyst with status Online

# Disconnect
cargo run -p agentos-cli -- agent disconnect analyst
# Expected: "✅ Agent 'analyst' disconnected"

# Verify removed
cargo run -p agentos-cli -- agent list
# Expected: No agents listed
```

---

## Test 4: Secrets CRUD

**What it tests:** Full lifecycle of a secret: set, list, get (internal), rotate, revoke.

### Automated Test

```rust
#[tokio::test]
async fn test_secrets_full_lifecycle() {
    // Boot kernel with test config...
    // Connect client via bus...

    // 1. Set a secret
    let response = client.send_command(KernelCommand::SetSecret {
        name: "TEST_KEY".into(),
        value: "super-secret-123".into(),
        scope: SecretScope::Global,
    }).await.unwrap();
    assert!(matches!(response, KernelResponse::Success { .. }));

    // 2. List secrets — value should NOT appear
    let response = client.send_command(KernelCommand::ListSecrets).await.unwrap();
    match response {
        KernelResponse::SecretList(secrets) => {
            assert_eq!(secrets.len(), 1);
            assert_eq!(secrets[0].name, "TEST_KEY");
            // No value field exists on SecretMetadata — this is by design
        }
        _ => panic!("Wrong response type"),
    }

    // 3. Rotate
    let response = client.send_command(KernelCommand::RotateSecret {
        name: "TEST_KEY".into(),
        new_value: "new-secret-456".into(),
    }).await.unwrap();
    assert!(matches!(response, KernelResponse::Success { .. }));

    // 4. Revoke
    let response = client.send_command(KernelCommand::RevokeSecret {
        name: "TEST_KEY".into(),
    }).await.unwrap();
    assert!(matches!(response, KernelResponse::Success { .. }));

    // 5. List should be empty
    let response = client.send_command(KernelCommand::ListSecrets).await.unwrap();
    match response {
        KernelResponse::SecretList(secrets) => assert_eq!(secrets.len(), 0),
        _ => panic!("Wrong response type"),
    }
}
```

### Manual Test

```bash
# Set a secret (value entered interactively)
cargo run -p agentos-cli -- secret set OPENAI_API_KEY
# Enter value: (type hidden, press Enter)
# Expected: "✅ Secret 'OPENAI_API_KEY' stored securely"

# List secrets
cargo run -p agentos-cli -- secret list
# Expected: Shows OPENAI_API_KEY with scope and metadata, NO value

# Revoke
cargo run -p agentos-cli -- secret revoke OPENAI_API_KEY
# Expected: "✅ Secret 'OPENAI_API_KEY' revoked"
```

---

## Test 5: Permission Grant & Enforcement

**What it tests:** Granting permissions, verifying they're shown correctly, and that the kernel enforces them.

### Manual Test

```bash
# Connect an agent
cargo run -p agentos-cli -- agent connect --provider ollama --model llama3.2 --name analyst

# Show permissions (should be empty or default)
cargo run -p agentos-cli -- perm show analyst
# Expected: Shows permission table, all denied

# Grant file read permission
cargo run -p agentos-cli -- perm grant analyst fs.user_data:r
# Expected: "✅ Permission granted"

# Show again
cargo run -p agentos-cli -- perm show analyst
# Expected: fs.user_data shows R=✓, W=-, X=-

# Try running a task that writes a file (should fail — no write permission)
cargo run -p agentos-cli -- task run --agent analyst "Write 'hello' to /output/test.txt"
# Expected: Permission denied for fs.user_data write

# Grant write permission
cargo run -p agentos-cli -- perm grant analyst fs.user_data:w
# Now the same task should succeed
```

---

## Test 6: End-to-End Task Execution (The Main Test)

**What it tests:** The full pipeline: user prompt → kernel → LLM → tool call → result → LLM → final answer.

### Prerequisites

- Ollama running with `llama3.2`
- A test file at the configured data directory

### Manual Test

```bash
# 1. Start kernel
cargo run -p agentos-cli -- start

# 2. Connect agent & grant permissions
cargo run -p agentos-cli -- agent connect --provider ollama --model llama3.2 --name analyst
cargo run -p agentos-cli -- perm grant analyst fs.user_data:rw
cargo run -p agentos-cli -- perm grant analyst memory.semantic:rw

# 3. Create a test file
mkdir -p /opt/agentos/data
echo "Q1 revenue was $2.5M. Q2 revenue was $3.1M. Q3 revenue was $2.8M." > /opt/agentos/data/revenue.txt

# 4. Run a task that uses the file-reader tool
cargo run -p agentos-cli -- task run --agent analyst "Read the file at /revenue.txt and summarize the revenue trends"

# Expected behavior:
#   - LLM receives the prompt
#   - LLM decides to use file-reader tool
#   - Kernel validates capability token → ✅ (fs.user_data:r is granted)
#   - file-reader reads /opt/agentos/data/revenue.txt
#   - Result pushed into LLM context
#   - LLM provides a summary like "Revenue peaked in Q2 at $3.1M..."
#   - Final answer displayed to user

# 5. Verify audit log captured the full trace
cargo run -p agentos-cli -- audit logs --last 20
# Expected: Shows TaskCreated, IntentReceived, ToolExecutionStarted,
#           ToolExecutionCompleted, LLMInferenceStarted, LLMInferenceCompleted,
#           TaskCompleted events

# 6. Run a memory task
cargo run -p agentos-cli -- task run --agent analyst "Remember that Q1 revenue was 2.5M and Q2 was 3.1M"
# Expected: LLM uses memory-write tool to store this information

# 7. Verify memory persistence
cargo run -p agentos-cli -- task run --agent analyst "What do you remember about revenue?"
# Expected: LLM uses memory-search tool, finds the stored information
```

---

## Test 7: Security — Path Traversal Protection

**What it tests:** Tools cannot read files outside the data directory.

### Automated Test

```rust
#[tokio::test]
async fn test_path_traversal_blocked() {
    let tool = FileReader::new();
    let dir = tempfile::TempDir::new().unwrap();

    // Attempt to read /etc/passwd via path traversal
    let result = tool.execute(
        serde_json::json!({"path": "../../etc/passwd"}),
        ToolExecutionContext {
            data_dir: dir.path().to_path_buf(),
            task_id: TaskID::new(),
            trace_id: TraceID::new(),
        },
    ).await;

    assert!(result.is_err());
}
```

---

## Test 8: Capability Token Tamper Detection

**What it tests:** A tampered capability token is rejected by the kernel.

### Automated Test

```rust
#[tokio::test]
async fn test_tampered_token_rejected() {
    let engine = CapabilityEngine::new();
    let agent_id = AgentID::new();
    engine.register_agent(agent_id, PermissionSet::new());

    // Issue a token with Read-only access
    let mut token = engine.issue_token(
        TaskID::new(), agent_id,
        BTreeSet::new(),
        BTreeSet::from([IntentTypeFlag::Read]),
        Duration::from_secs(300),
    ).unwrap();

    // Tamper: add Write access
    token.allowed_intents.insert(IntentTypeFlag::Write);

    // Should fail signature verification
    assert!(!engine.verify_signature(&token));
}
```

---

## Success Criteria

| Test           | Pass Criteria                                           |
| -------------- | ------------------------------------------------------- |
| Unit tests     | `cargo test --workspace` — 0 failures                   |
| Kernel boot    | Kernel starts, loads tools, opens bus socket            |
| Agent connect  | Ollama agent connects, appears in list                  |
| Secrets CRUD   | Set/list/rotate/revoke all work; values never exposed   |
| Permissions    | Grant/revoke work; kernel enforces on tool calls        |
| E2E task       | LLM reads a file via tool, provides intelligent summary |
| Path traversal | `../../etc/passwd` blocked in file-reader               |
| Token tamper   | Modified tokens rejected by signature check             |

## Running All Tests

```bash
# 1. All unit and automated integration tests
cargo test --workspace

# 2. Integration tests that require Ollama (marked #[ignore])
ollama serve &  # ensure Ollama is running
cargo test --workspace -- --ignored

# 3. Manual E2E tests — follow the steps in Test 6 above
```
