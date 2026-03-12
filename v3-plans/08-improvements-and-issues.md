# AgentOS — Improvements, Issues & Remediation Plan

> Comprehensive audit of the AgentOS codebase against the design document.
> Each issue includes: description, affected files, root cause, detailed fix, and estimated effort.

---

## Priority Legend

| Priority | Meaning |
|----------|---------|
| **HIGH** | Blocks production use, introduces security risk, or causes architectural debt that compounds over time. Fix before any public release. |
| **MEDIUM** | Degrades quality, performance, or developer experience. Fix before v0.2.0. |
| **LOW** | Nice-to-have improvements. Schedule for v0.3.0+. |

---

## HIGH Priority

---

### H-1: Split `kernel.rs` (2,730 lines) Into Focused Modules

**Problem:**
The entire kernel — boot sequence, run loop, 40+ command handlers, task execution, tool dispatch, agent management, pipeline integration — lives in a single 2,730-line file. This makes it nearly impossible to review, test, or modify individual subsystems without risk of collateral breakage. It is the single largest maintainability risk in the codebase.

**Affected files:**
- `crates/agentos-kernel/src/kernel.rs` (2,730 lines)

**Root cause:**
Organic growth during Phase 1 without periodic refactoring.

**Detailed fix:**

Split into the following module structure:

```
crates/agentos-kernel/src/
├── kernel.rs              # Kernel struct definition + boot() only (~150 lines)
├── run_loop.rs            # The main async run loop with 4 spawned tasks (~100 lines)
├── commands/
│   ├── mod.rs             # Re-exports
│   ├── agent.rs           # cmd_connect_agent, cmd_disconnect_agent, cmd_list_agents
│   ├── task.rs            # cmd_run_task, cmd_list_tasks, cmd_cancel_task, cmd_get_task_logs
│   ├── tool.rs            # cmd_install_tool, cmd_remove_tool, cmd_list_tools
│   ├── secret.rs          # cmd_set_secret, cmd_list_secrets, cmd_rotate_secret, cmd_revoke_secret
│   ├── permission.rs      # cmd_grant_permission, cmd_revoke_permission, cmd_show_permissions
│   ├── role.rs            # cmd_create_role, cmd_delete_role, cmd_list_roles, cmd_assign_role
│   ├── schedule.rs        # cmd_create_schedule, cmd_list_schedules, cmd_pause, cmd_resume, cmd_delete
│   ├── background.rs      # cmd_bg_run, cmd_bg_list, cmd_bg_logs, cmd_bg_kill
│   ├── pipeline.rs        # cmd_pipeline_install/list/run/status/logs/remove
│   └── system.rs          # cmd_get_status, cmd_get_audit_logs
├── task_executor.rs       # The inference loop that dequeues and runs tasks (~200 lines)
└── core_manifests.rs      # install_core_manifests() helper
```

**Step-by-step migration:**

1. Create `commands/mod.rs` with `pub mod agent; pub mod task; ...` etc.
2. For each `cmd_*` method in `kernel.rs`:
   - Move the method body into the appropriate `commands/*.rs` file as a free function that takes `&Kernel` (or the specific `Arc<Subsystem>` references it needs).
   - Replace the original method with a one-line delegation: `commands::agent::connect(self, name, provider, ...).await`
3. Extract the `tokio::select!` run loop into `run_loop.rs` as `pub async fn run(kernel: Arc<Kernel>)`.
4. Extract `execute_task()` and related inference logic into `task_executor.rs`.
5. Extract `install_core_manifests()` into `core_manifests.rs`.
6. Run `cargo test` after each extraction to verify nothing breaks.

**Effort:** 4-6 hours (mechanical refactor, no logic changes)

---

### H-2: Run Loop Has No Supervisor / Crash Recovery

**Problem:**
The kernel spawns 4 async tasks in its run loop (connection acceptor, task executor, timeout checker, agentd scheduler). If any one of them panics, the others continue running in a silently degraded state. There is no detection, restart, or graceful shutdown.

**Affected files:**
- `crates/agentos-kernel/src/kernel.rs` (run loop, ~lines 251-300)

**Root cause:**
Tasks are spawned with `tokio::spawn` and the handles are consumed by `tokio::select!` without panic propagation.

**Detailed fix:**

Wrap each spawned task in a supervisor that catches panics and triggers recovery:

```rust
// In run_loop.rs
use tokio::task::JoinSet;

use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Clone)]
struct SupervisedTask {
    name: &'static str,
    restart_count: u32,
    max_restarts: u32,
    last_restart: Option<Instant>,
    /// Reset restart_count if the task ran successfully for this long
    healthy_duration: Duration,
}

impl SupervisedTask {
    fn new(name: &'static str, max_restarts: u32) -> Self {
        Self {
            name,
            restart_count: 0,
            max_restarts,
            last_restart: None,
            healthy_duration: Duration::from_secs(60),
        }
    }

    /// Returns the backoff duration before next restart, or None if max exceeded.
    fn next_backoff(&mut self) -> Option<Duration> {
        if self.restart_count >= self.max_restarts {
            return None; // exceeded max restarts
        }
        let backoff = Duration::from_millis(100 * 2u64.pow(self.restart_count.min(10)));
        self.restart_count += 1;
        self.last_restart = Some(Instant::now());
        Some(backoff)
    }

    /// Reset restart_count if task ran long enough to be considered healthy.
    fn maybe_reset(&mut self) {
        if let Some(last) = self.last_restart {
            if last.elapsed() >= self.healthy_duration {
                self.restart_count = 0;
            }
        }
    }
}

type SpawnFn = Box<dyn Fn(Arc<Kernel>) -> tokio::task::JoinHandle<&'static str> + Send + Sync>;

pub async fn run(kernel: Arc<Kernel>) {
    // Each spawned task returns its name on completion so we can identify it.
    let mut handles: HashMap<&'static str, tokio::task::JoinHandle<&'static str>> = HashMap::new();
    let mut supervisors: HashMap<&'static str, SupervisedTask> = HashMap::new();

    let task_defs: Vec<(&'static str, SpawnFn)> = vec![
        ("acceptor", Box::new(|k| tokio::spawn(async move {
            run_acceptor(k).await;
            "acceptor"
        }))),
        ("task_executor", Box::new(|k| tokio::spawn(async move {
            run_task_executor(k).await;
            "task_executor"
        }))),
        ("timeout_checker", Box::new(|k| tokio::spawn(async move {
            run_timeout_checker(k).await;
            "timeout_checker"
        }))),
        ("agentd_scheduler", Box::new(|k| tokio::spawn(async move {
            run_agentd_scheduler(k).await;
            "agentd_scheduler"
        }))),
    ];

    // Initial spawn
    for (name, spawn_fn) in &task_defs {
        supervisors.insert(name, SupervisedTask::new(name, 5));
        handles.insert(name, spawn_fn(kernel.clone()));
    }

    loop {
        // Wait for any handle to finish
        let (finished_name, result) = tokio::select! {
            // Poll all handles — find the first one that completes
            result = async {
                // futures::future::select_all equivalent via polling
                loop {
                    for (name, handle) in handles.iter_mut() {
                        // Use a zero-duration check — in practice, use JoinSet
                    }
                    tokio::task::yield_now().await;
                }
                // unreachable in skeleton — see JoinSet alternative below
            } => result,
        };

        // Determine which task exited and respawn only that one
        let task_name: &'static str = match &result {
            Ok(name) => {
                tracing::warn!(task = name, "Kernel task exited unexpectedly");
                name
            }
            Err(join_error) => {
                // Extract task name from panic payload if possible
                tracing::error!("Kernel task panicked: {:?}", join_error);
                kernel.audit.log(AuditEvent::SystemError {
                    details: format!("Task panic: {join_error}"),
                }).ok();
                "unknown"
            }
        };

        // Look up supervisor state and apply backoff
        if let Some(sup) = supervisors.get_mut(task_name) {
            sup.maybe_reset();
            match sup.next_backoff() {
                Some(backoff) => {
                    tracing::info!(
                        task = task_name,
                        restart_count = sup.restart_count,
                        backoff_ms = backoff.as_millis() as u64,
                        "Restarting kernel task after backoff"
                    );
                    tokio::time::sleep(backoff).await;

                    // Respawn only the failed task
                    if let Some((_, spawn_fn)) = task_defs.iter().find(|(n, _)| *n == task_name) {
                        handles.insert(task_name, spawn_fn(kernel.clone()));
                    }
                }
                None => {
                    tracing::error!(
                        task = task_name,
                        max_restarts = sup.max_restarts,
                        "Kernel task exceeded max restarts, entering degraded mode"
                    );
                    kernel.audit.log(AuditEvent::SystemError {
                        details: format!(
                            "Task '{}' exceeded {} max restarts — kernel degraded",
                            task_name, sup.max_restarts
                        ),
                    }).ok();
                    handles.remove(task_name);
                }
            }
        }

        // If all tasks have permanently failed, shut down
        if handles.is_empty() {
            tracing::error!("All kernel tasks exhausted restarts, shutting down");
            break;
        }
    }
}
```

For a more targeted approach, use one `JoinSet` per task type and restart only the failed one:

```rust
enum TaskKind { Acceptor, Executor, TimeoutChecker, Scheduler }

struct SupervisedTask {
    kind: TaskKind,
    restart_count: u32,
    max_restarts: u32,  // e.g., 5 within 60 seconds
}
```

If a task exceeds `max_restarts`, emit a `KernelDegraded` audit event and optionally trigger a full shutdown so the container orchestrator (Docker/K8s) can restart the container cleanly.

**Effort:** 3-4 hours

---

### H-3: `IntentTarget` Missing `Agent` and `Hardware` Variants

**Problem:**
The design document specifies `IntentTarget` should have `Tool(ToolID)`, `Hardware(HardwareResource)`, and `Agent(AgentID)` variants. The actual implementation only has `Tool(ToolID)` and `Kernel`. This means agent-to-agent communication and hardware access bypass the intent system entirely, breaking the core architectural promise that "all communication flows through IntentMessage."

**Affected files:**
- `crates/agentos-types/src/intent.rs` (line ~30, `IntentTarget` enum)
- `crates/agentos-kernel/src/kernel.rs` (command handlers that route agent messages and HAL calls)
- `crates/agentos-capability/src/engine.rs` (`validate_intent()` — needs to handle new targets)

**Root cause:**
Phase 1 focused on tool execution. Agent messaging and HAL were built as separate command paths rather than routing through the intent system.

**Detailed fix:**

1. **Update the enum:**

```rust
// crates/agentos-types/src/intent.rs
pub enum IntentTarget {
    Tool(ToolID),
    Kernel,
    Agent(AgentID),           // NEW: direct agent-to-agent
    Hardware(HardwareResource), // NEW: HAL-mediated hardware access
    Broadcast,                 // NEW: all agents in a group
}

pub enum HardwareResource {
    System,
    Process,
    Network,
    Sensor(String),
    Gpio(String),
    Gpu(u32),
    Storage(String),
}
```

2. **Add `Message` and `Broadcast` to `IntentType`:**

```rust
pub enum IntentType {
    Read,
    Write,
    Execute,
    Query,
    Observe,
    Delegate,
    Message,    // NEW
    Broadcast,  // NEW
}

impl IntentType {
    /// Maps an intent type to the corresponding permission operation.
    pub fn to_perm_op(&self) -> PermOp {
        match self {
            IntentType::Read | IntentType::Query | IntentType::Observe => PermOp::Read,
            IntentType::Write => PermOp::Write,
            IntentType::Execute | IntentType::Delegate
            | IntentType::Message | IntentType::Broadcast => PermOp::Execute,
        }
    }
}
```

3. **Update `validate_intent()` in the capability engine:**

```rust
// crates/agentos-capability/src/engine.rs
pub fn validate_intent(&self, intent: &IntentMessage) -> Result<(), AgentOSError> {
    self.verify_signature(&intent.sender_token)?;
    self.check_expiry(&intent.sender_token)?;

    match &intent.target {
        IntentTarget::Tool(tool_id) => {
            self.check_tool_allowed(&intent.sender_token, tool_id)?;
        }
        IntentTarget::Agent(_) => {
            self.check_permission(&intent.sender_token, "agent.message", PermOp::Execute)?;
        }
        IntentTarget::Hardware(resource) => {
            let perm_resource = match resource {
                HardwareResource::System => "hardware.system",
                HardwareResource::Process => "process.list",
                HardwareResource::Gpu(id) => "hardware.gpu",
                // ... map each variant
            };
            let op = intent.intent_type.to_perm_op(); // Read→r, Write→w, Execute→x
            self.check_permission(&intent.sender_token, perm_resource, op)?;
        }
        IntentTarget::Broadcast => {
            self.check_permission(&intent.sender_token, "agent.broadcast", PermOp::Execute)?;
        }
        IntentTarget::Kernel => {} // kernel intents are always allowed for valid tokens
    }

    Ok(())
}
```

4. **Reroute agent message and HAL command handlers** in the kernel to construct `IntentMessage` envelopes and pass them through `validate_intent()` before executing.

**Effort:** 6-8 hours (touches 3 crates, needs test updates)

---

### H-4: Tool Output Sanitization Not Enforced

**Problem:**
The design document states: "tool outputs are never injected raw into LLM context — they are wrapped in typed delimiters and treated as untrusted data." In the actual code, `ToolRunner::execute()` returns raw `serde_json::Value` which is then serialized to string and injected into the context window. There is no delimiter wrapping or sanitization step. This is a prompt injection vector — a malicious tool result could contain text that looks like system instructions.

**Affected files:**
- `crates/agentos-tools/src/runner.rs` (lines 82-106, `execute()` method)
- `crates/agentos-kernel/src/kernel.rs` (where tool results are injected into context)

**Root cause:**
Phase 1 focused on getting tools working. Sanitization was deferred.

**Detailed fix:**

1. **Create a sanitization module:**

```rust
// crates/agentos-tools/src/sanitize.rs

/// Wraps tool output in typed delimiters that the LLM can distinguish
/// from system instructions. Also escapes any delimiter-like sequences
/// in the raw output to prevent injection.
pub fn sanitize_tool_output(tool_name: &str, raw_output: &serde_json::Value) -> String {
    let serialized = serde_json::to_string_pretty(raw_output)
        .unwrap_or_else(|_| format!("{:?}", raw_output));

    // Escape any existing delimiter-like patterns in the output
    let escaped = serialized
        .replace("[TOOL_RESULT", "[ESCAPED_TOOL_RESULT")
        .replace("[/TOOL_RESULT", "[/ESCAPED_TOOL_RESULT")
        .replace("[SYSTEM", "[ESCAPED_SYSTEM")
        .replace("[AGENT_DIRECTORY", "[ESCAPED_AGENT_DIRECTORY");

    format!(
        "[TOOL_RESULT: {}]\n{}\n[/TOOL_RESULT]",
        tool_name, escaped
    )
}

/// Validates that tool output doesn't exceed context budget.
/// Truncates by Unicode scalar values to avoid splitting multi-byte characters.
pub fn truncate_if_needed(output: &str, max_chars: usize) -> String {
    if output.chars().count() > max_chars {
        let truncated: String = output.chars().take(max_chars).collect();
        format!(
            "{}\n[TOOL_RESULT_TRUNCATED: output exceeded {} chars]",
            truncated, max_chars
        )
    } else {
        output.to_string()
    }
}
```

2. **Apply in the tool runner:**

```rust
// crates/agentos-tools/src/runner.rs
pub async fn execute(&self, tool_name: &str, payload: Value, ctx: &ToolExecutionContext) -> Result<String, AgentOSError> {
    let raw_result = self.tools.get(tool_name)
        .ok_or(AgentOSError::ToolNotFound(tool_name.to_string()))?
        .execute(payload, ctx).await?;

    let sanitized = sanitize::sanitize_tool_output(tool_name, &raw_result);
    let bounded = sanitize::truncate_if_needed(&sanitized, ctx.max_output_chars);
    Ok(bounded)
}
```

3. **Add context role enforcement** — ensure tool results are always inserted as `ContextRole::ToolResult`, never `ContextRole::System` or `ContextRole::User`.

**Effort:** 2-3 hours

---

### H-5: HMAC Signing Key Lost on Restart — Invalidates All Tokens

**Problem:**
`CapabilityEngine::new()` generates a random 256-bit signing key at boot. This key is not persisted. On container restart, all previously issued capability tokens (including those embedded in scheduled cron jobs and background tasks) become invalid because the HMAC signature can no longer be verified.

**Affected files:**
- `crates/agentos-capability/src/engine.rs` (line ~20, `new()` method)
- `crates/agentos-kernel/src/kernel.rs` (boot sequence where CapabilityEngine is initialized)

**Root cause:**
Phase 1 assumed ephemeral tokens. Scheduled tasks introduced persistent tokens that outlive a single boot cycle.

**Detailed fix:**

1. **Store the signing key in the vault:**

```rust
// crates/agentos-capability/src/engine.rs

impl CapabilityEngine {
    /// Boot: load existing key from vault or generate + persist a new one.
    ///
    /// # Bootstrap dependency ordering
    /// The SecretsVault must be fully initialized (encryption key derived from
    /// passphrase) *before* calling `CapabilityEngine::boot()`. The HMAC signing
    /// key is stored as an encrypted secret inside the vault. If the vault is not
    /// ready, this function will fail at the `vault.get_by_name` or `vault.set`
    /// call. Recovery: ensure `SecretsVault::open(passphrase)` completes before
    /// `CapabilityEngine::boot(&vault, &audit)` in the kernel boot sequence.
    pub async fn boot(vault: &SecretsVault, audit: &AuditLog) -> Result<Self, AgentOSError> {
        let key_name = "__internal_hmac_signing_key";

        match vault.get_by_name(SecretOwner::Kernel, key_name).await {
            Ok(existing_key) => {
                // Key exists — restore from vault
                let raw = existing_key.as_bytes();
                let key_bytes: [u8; 32] = raw.try_into().map_err(|_| {
                    AgentOSError::InternalError(format!(
                        "Corrupt HMAC signing key in vault (owner={:?}, name={:?}): \
                         expected 32 bytes, got {} bytes. \
                         The key may have been truncated or overwritten. \
                         Recovery: delete the secret '{}' from the vault and restart \
                         to generate a fresh key (this invalidates all existing tokens).",
                        SecretOwner::Kernel,
                        key_name,
                        raw.len(),
                        key_name,
                    ))
                })?;
                Ok(Self::with_key(key_bytes))
            }
            Err(_) => {
                // First boot — generate and persist
                let mut key = [0u8; 32];
                rand::thread_rng().fill(&mut key);

                const MAX_RETRIES: u32 = 3;
                let mut last_err = None;
                for attempt in 0..MAX_RETRIES {
                    match vault.set(
                        SecretOwner::Kernel,
                        SecretScope::KernelOnly,
                        key_name,
                        &key,
                    ).await {
                        Ok(()) => {
                            last_err = None;
                            break;
                        }
                        Err(e) => {
                            let backoff = Duration::from_millis(100 * 2u64.pow(attempt));
                            tracing::warn!(
                                attempt = attempt + 1,
                                max = MAX_RETRIES,
                                error = %e,
                                backoff_ms = backoff.as_millis() as u64,
                                "Failed to persist HMAC signing key, retrying"
                            );
                            audit.log_event(
                                AuditEventType::SystemError, None, None, None,
                                format!(
                                    "vault.set failed for HMAC key (attempt {}/{}): {}",
                                    attempt + 1, MAX_RETRIES, e
                                ),
                                AuditSeverity::Warn,
                            ).ok();
                            last_err = Some(e);
                            tokio::time::sleep(backoff).await;
                        }
                    }
                }

                if let Some(e) = last_err {
                    // All retries exhausted — use ephemeral key but warn loudly.
                    // The kernel will function but tokens will not survive restart.
                    audit.log_event(
                        AuditEventType::SystemError, None, None, None,
                        format!(
                            "HMAC key persistence failed after {} retries: {}. \
                             Using ephemeral in-memory key — tokens will be \
                             invalidated on restart. Check vault health and retry.",
                            MAX_RETRIES, e
                        ),
                        AuditSeverity::Security,
                    ).ok();
                    tracing::error!(
                        "HMAC signing key could NOT be persisted to vault. \
                         Running with ephemeral key. Error: {e}"
                    );
                    return Ok(Self::with_key(key));
                }

                audit.log_event(AuditEventType::SecretCreated, None, None, None,
                    "HMAC signing key generated and persisted".into(),
                    AuditSeverity::Security,
                ).ok();

                Ok(Self::with_key(key))
            }
        }
    }
}
```

2. **Update the kernel boot sequence** to use `CapabilityEngine::boot(&vault, &audit)` instead of `CapabilityEngine::new()`.

3. **Add a key rotation command** (`agentctl internal rotate-signing-key`) that generates a new key, re-signs all active scheduled tasks' tokens, and persists the new key.

**Effort:** 3-4 hours

---

### H-6: No Integration Test for Full Kernel Lifecycle

**Problem:**
Existing tests are unit-level (boot kernel, secrets operations, pipeline CRUD). There is no test that exercises the full lifecycle: boot kernel → connect agent → run task → tool executes → result returns → audit log populated. Cross-subsystem regressions go undetected.

**Affected files:**
- `crates/agentos-cli/tests/` (existing test files)

**Root cause:**
Integration testing requires a mock LLM backend, which wasn't prioritized in Phase 1.

**Detailed fix:**

1. **Create a mock LLM adapter:**

```rust
// crates/agentos-llm/src/mock.rs
pub struct MockLLMCore {
    pub responses: Vec<String>,
    pub call_count: AtomicUsize,
}

#[async_trait]
impl LLMCore for MockLLMCore {
    async fn infer(&self, context: &ContextWindow) -> Result<InferenceResult, AgentOSError> {
        let idx = self.call_count.fetch_add(1, Ordering::SeqCst);
        let response = self.responses.get(idx)
            .cloned()
            .unwrap_or_else(|| "Mock response".to_string());

        Ok(InferenceResult {
            content: response,
            usage: TokenUsage { input: 10, output: 5 },
        })
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &ModelCapabilities {
            context_window_tokens: 4096,
            supports_images: false,
            supports_tool_calling: true,
            supports_json_mode: true,
        }
    }

    async fn health_check(&self) -> bool { true }
    fn provider_name(&self) -> &str { "mock" }
    fn model_name(&self) -> &str { "mock-v1" }
}
```

2. **Create the integration test:**

```rust
// crates/agentos-cli/tests/integration_test.rs

#[tokio::test]
async fn test_full_lifecycle() {
    // 1. Boot kernel with temp directories
    let tmp = tempdir().unwrap();
    let config = create_test_config(&tmp);
    let kernel = Kernel::boot(&config, "test-passphrase").await.unwrap();

    // 2. Connect a mock agent
    let mock_llm = Arc::new(MockLLMCore::new(vec![
        "I'll read the file for you.".to_string(),
    ]));
    kernel.connect_agent("test-agent", mock_llm).await.unwrap();

    // 3. Run a task
    let task_id = kernel.run_task("test-agent", "Read file /data/test.txt").await.unwrap();

    // 4. Wait for completion with explicit timeout error
    let wait_result = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = kernel.get_task_status(task_id).await;
            if matches!(status, TaskState::Complete | TaskState::Failed) { break; }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }).await;
    assert!(
        wait_result.is_ok(),
        "Timed out after 5s waiting for task {} to reach Complete or Failed state",
        task_id,
    );

    // 5. Verify audit log has entries
    let logs = kernel.audit.recent(10).unwrap();
    assert!(logs.iter().any(|e| e.event_type == AuditEventType::TaskCreated));
    assert!(logs.iter().any(|e| e.event_type == AuditEventType::InferenceCompleted));

    // 6. Cleanup
    kernel.shutdown().await.unwrap();
}
```

3. **Add to CI** (see L-3 for CI setup).

**Effort:** 4-6 hours

---

### H-7: Permission Enforcement Gap in Tool Runner

**Problem:**
`ToolRunner::execute()` looks up the tool by name and calls `tool.execute(payload, context)` directly. While the kernel checks permissions *before* calling the runner, the runner itself does not validate that the `ToolExecutionContext` contains the required permissions for the tool being executed. If a code path ever bypasses the kernel's pre-check (e.g., pipeline step execution, background task), the tool runs without authorization.

**Affected files:**
- `crates/agentos-tools/src/runner.rs` (lines 82-106)
- `crates/agentos-tools/src/traits.rs` (`AgentTool` trait)

**Root cause:**
Defense-in-depth was not applied — permission checking exists at the kernel layer but not at the tool layer.

**Detailed fix:**

Add a permission check inside `ToolRunner::execute()`:

```rust
// crates/agentos-tools/src/runner.rs

pub async fn execute(
    &self,
    tool_name: &str,
    payload: Value,
    ctx: &ToolExecutionContext,
) -> Result<Value, AgentOSError> {
    let tool = self.tools.get(tool_name)
        .ok_or_else(|| AgentOSError::ToolNotFound(tool_name.to_string()))?;

    // Defense-in-depth: verify permissions even if kernel already checked
    let required = tool.get_required_permissions();
    for perm in &required {
        if !ctx.permissions.check(perm) {
            return Err(AgentOSError::PermissionDenied {
                resource: perm.clone(),
                operation: "execute".to_string(),
                agent: ctx.agent_id.to_string(),
            });
        }
    }

    let start = std::time::Instant::now();
    let result = tool.execute(payload, ctx).await?;
    tracing::debug!(tool = tool_name, elapsed_ms = start.elapsed().as_millis(), "tool executed");

    Ok(result)
}
```

This ensures that even if a future code path bypasses the kernel's intent validation, the tool itself refuses to run without proper authorization.

**Effort:** 1-2 hours

---

## MEDIUM Priority

---

### M-1: No Rate Limiting or Backpressure on Intent Bus

**Problem:**
If an agent (or a compromised tool) floods the intent bus with messages, nothing throttles it. The kernel will attempt to process every intent, potentially starving other agents and exhausting memory. The audit log will also grow unboundedly.

**Affected files:**
- `crates/agentos-bus/src/server.rs` (connection handler)
- `crates/agentos-kernel/src/kernel.rs` (command processing loop)

**Root cause:**
Phase 1 assumed cooperative agents. No adversarial load was considered.

**Detailed fix:**

1. **Add a per-agent rate limiter in the bus server:**

```rust
// crates/agentos-bus/src/rate_limit.rs

use std::collections::HashMap;
use std::time::{Duration, Instant};

pub struct RateLimiter {
    /// agent_id → (window_start, count_in_window)
    windows: HashMap<String, (Instant, u32)>,
    max_per_window: u32,
    window_duration: Duration,
}

impl RateLimiter {
    pub fn new(max_per_second: u32) -> Self {
        Self {
            windows: HashMap::new(),
            max_per_window: max_per_second,
            window_duration: Duration::from_secs(1),
        }
    }

    /// Returns Ok(()) if allowed, Err(wait_duration) if rate-limited.
    pub fn check(&mut self, agent_id: &str) -> Result<(), Duration> {
        let now = Instant::now();
        let entry = self.windows.entry(agent_id.to_string())
            .or_insert((now, 0));

        if now.duration_since(entry.0) > self.window_duration {
            // New window
            *entry = (now, 1);
            Ok(())
        } else if entry.1 < self.max_per_window {
            entry.1 += 1;
            Ok(())
        } else {
            let wait = self.window_duration - now.duration_since(entry.0);
            Err(wait)
        }
    }
}
```

2. **Apply in the connection handler:**

```rust
// In the per-connection read loop:
match rate_limiter.check(&agent_id) {
    Ok(()) => { /* process command */ }
    Err(wait) => {
        let response = KernelResponse::Error(format!(
            "Rate limited. Retry after {} ms", wait.as_millis()
        ));
        transport::send(&mut stream, &response).await?;
        audit.log_event(AuditEventType::PermissionDenied, Some(agent_id), None, None,
            "Rate limit exceeded".into(), AuditSeverity::Warn).ok();
    }
}
```

3. **Make the limit configurable in `default.toml`:**

```toml
[kernel]
max_intents_per_agent_per_second = 50
```

**Effort:** 3-4 hours

---

### M-2: Semantic Memory Search Is O(n) — Loads All Chunks

**Problem:**
`SemanticStore::search()` loads every chunk from the database into memory and computes cosine similarity against each one. For a semantic store with 100K+ chunks, this will be unacceptably slow and memory-intensive.

Additionally, the FTS5 index exists (with triggers keeping it in sync) but is never used during search — the "hybrid search" with Reciprocal Rank Fusion is effectively vector-only.

**Affected files:**
- `crates/agentos-memory/src/semantic.rs` (lines 186-326, `search()` method)

**Root cause:**
Phase 1 prioritized correctness over performance. The FTS5 integration was set up in the schema but the search query never uses it.

**Detailed fix:**

1. **Add a pre-filter using FTS5 to reduce the candidate set:**

```rust
pub async fn search(&self, query: &RecallQuery) -> Result<Vec<RecallResult>, AgentOSError> {
    let query_embedding = self.embedder.embed(&query.text)?;

    // Step 1: FTS5 pre-filter — get top 200 candidates by text relevance
    let fts_candidates: Vec<i64> = self.db.query(
        "SELECT rowid, rank FROM semantic_fts WHERE semantic_fts MATCH ?1 ORDER BY rank LIMIT 200",
        [&query.text],
    )?;

    // Step 2: Load only candidate chunk embeddings
    let chunks = if fts_candidates.is_empty() {
        // Fallback: load top N by recency if FTS finds nothing
        self.db.query(
            "SELECT c.id, c.memory_id, c.embedding FROM semantic_chunks c
             ORDER BY c.id DESC LIMIT 500",
            [],
        )?
    } else {
        // Build parameterized placeholders to avoid SQL injection
        let placeholders = fts_candidates.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT c.id, c.memory_id, c.embedding FROM semantic_chunks c
             WHERE c.memory_id IN ({})",
            placeholders
        );
        let params: Vec<&dyn rusqlite::ToSql> = fts_candidates
            .iter()
            .map(|id| id as &dyn rusqlite::ToSql)
            .collect();
        self.db.query(&sql, params.as_slice())?
    };

    // Step 3: Compute cosine similarity only on candidates
    let mut scored: Vec<(i64, f32)> = chunks.iter()
        .map(|chunk| {
            let sim = cosine_similarity(&query_embedding, &chunk.embedding);
            (chunk.memory_id, sim)
        })
        .collect();

    // Step 4: RRF fusion (now actually combining FTS rank + vector score)
    // ... existing dedup + sort logic ...
}
```

2. **Add a minimum relevance threshold:**

```rust
// Filter out low-relevance results before returning
let min_score = query.min_score.unwrap_or(0.3);
scored.retain(|&(_, score)| score >= min_score);
```

3. **For large-scale deployments (future):** Consider integrating an approximate nearest neighbor (ANN) index like `hnswlib` via FFI, or switch to a purpose-built vector DB (Qdrant, LanceDB). But the FTS5 pre-filter is the right immediate fix.

**Effort:** 4-6 hours

---

### M-3: No Streaming Inference in `LLMCore` Trait

**Problem:**
`LLMCore::infer()` returns a complete `InferenceResult` only after the entire generation finishes. For long outputs (1000+ tokens), this means the user and downstream tools see nothing for seconds. It also prevents early cancellation — if the LLM starts generating irrelevant output, you can't stop it until it's done.

**Affected files:**
- `crates/agentos-llm/src/traits.rs` (line 8, `infer()` signature)
- All adapter implementations: `ollama.rs`, `openai.rs`, `anthropic.rs`, `gemini.rs`, `custom.rs`

**Root cause:**
Phase 1 used the simplest possible interface.

**Detailed fix:**

1. **Add a streaming method to the trait (non-breaking — keep `infer()` as-is):**

```rust
// crates/agentos-llm/src/traits.rs
use tokio::sync::mpsc;

pub enum InferenceEvent {
    Token(String),            // A chunk of text
    Done(InferenceResult),    // Final result with usage stats
    Error(AgentOSError),
}

#[async_trait]
pub trait LLMCore: Send + Sync {
    /// Batch inference (existing — unchanged)
    async fn infer(&self, context: &ContextWindow) -> Result<InferenceResult, AgentOSError>;

    /// Streaming inference (new — default falls back to batch)
    async fn infer_stream(
        &self,
        context: &ContextWindow,
        tx: mpsc::Sender<InferenceEvent>,
    ) -> Result<(), AgentOSError> {
        // Default implementation: call infer() and send result as single event
        match self.infer(context).await {
            Ok(result) => {
                let _ = tx.send(InferenceEvent::Token(result.content.clone())).await;
                let _ = tx.send(InferenceEvent::Done(result)).await;
                Ok(())
            }
            Err(e) => {
                let _ = tx.send(InferenceEvent::Error(e.clone())).await;
                Err(e)
            }
        }
    }

    fn capabilities(&self) -> &ModelCapabilities;
    async fn health_check(&self) -> bool;
    fn provider_name(&self) -> &str;
    fn model_name(&self) -> &str;
}
```

2. **Define the streaming response types** matching the Ollama streaming schema:

```rust
// crates/agentos-llm/src/ollama.rs

/// A message fragment within a streaming chunk.
#[derive(Debug, Deserialize)]
struct OllamaMessage {
    /// The role of the message sender (e.g., "assistant").
    #[serde(default)]
    pub role: String,
    /// The content token(s) for this chunk. None when `done` is true.
    #[serde(default)]
    pub content: Option<String>,
}

/// A single chunk from the Ollama streaming API (`/api/chat` with `stream: true`).
#[derive(Debug, Deserialize)]
struct OllamaStreamChunk {
    /// The partial message for this chunk.
    pub message: OllamaMessage,
    /// Whether this is the final chunk in the stream.
    #[serde(default)]
    pub done: bool,
    /// Total generation duration in nanoseconds (only present on final chunk).
    #[serde(default)]
    pub total_duration: Option<u64>,
    /// Number of tokens evaluated from the prompt (only present on final chunk).
    #[serde(default)]
    pub prompt_eval_count: Option<u64>,
    /// Number of tokens generated (only present on final chunk).
    #[serde(default)]
    pub eval_count: Option<u64>,
}
```

3. **Implement native streaming for each adapter.** Example for Ollama:

```rust
// crates/agentos-llm/src/ollama.rs
async fn infer_stream(
    &self,
    context: &ContextWindow,
    tx: mpsc::Sender<InferenceEvent>,
) -> Result<(), AgentOSError> {
    let request = self.build_request(context, true)?; // stream: true
    let mut response = self.client.post(&self.url).json(&request).send().await?;

    let mut full_content = String::new();
    let mut final_chunk: Option<OllamaStreamChunk> = None;

    while let Some(chunk) = response.chunk().await? {
        let data: OllamaStreamChunk = serde_json::from_slice(&chunk)?;
        if let Some(text) = &data.message.content {
            full_content.push_str(text);
            let _ = tx.send(InferenceEvent::Token(text.clone())).await;
        }
        let is_done = data.done;
        if is_done {
            final_chunk = Some(data);
            break;
        }
    }

    let usage = TokenUsage {
        input: final_chunk.as_ref().and_then(|c| c.prompt_eval_count).unwrap_or(0),
        output: final_chunk.as_ref().and_then(|c| c.eval_count).unwrap_or(0),
    };

    let _ = tx.send(InferenceEvent::Done(InferenceResult {
        content: full_content,
        usage,
    })).await;

    Ok(())
}
```

3. **Update the task executor** to use `infer_stream()` when available, forwarding tokens to the context window incrementally.

**Effort:** 6-8 hours (one adapter at a time)

---

### M-4: Pipeline Engine Has No Step-Level Error Handling

**Problem:**
If step 3 of a 5-step pipeline fails, the entire pipeline stops immediately. There is no way to define `on_failure` behavior per step (skip, use default value, run a fallback step). The retry logic exists but only retries the same step — it cannot redirect to an alternative.

**Affected files:**
- `crates/agentos-pipeline/src/engine.rs` (lines 39-126, `run()` method)
- `crates/agentos-pipeline/src/definition.rs` (pipeline YAML schema)

**Root cause:**
Phase 1 implemented the happy path. Error recovery was deferred.

**Detailed fix:**

1. **Extend the pipeline YAML schema with `on_failure`:**

```yaml
# In definition.rs, add to StepDefinition:
steps:
  - id: fetch-data
    action:
      agent: researcher
      task: "Fetch data from {source}"
    output_var: raw_data
    on_failure: skip           # skip | fail | use_default | fallback
    default_value: "No data available"  # used with use_default

  - id: analyze
    action:
      agent: analyst
      task: "Analyze: {raw_data}"
    output_var: analysis
    on_failure:
      fallback_step: simple-analyze  # run this step instead

  - id: simple-analyze
    action:
      agent: summarizer
      task: "Basic summary: {raw_data}"
    output_var: analysis
    skip_unless_fallback: true  # only runs if triggered as fallback
```

2. **Update the `StepDefinition` struct:**

```rust
// crates/agentos-pipeline/src/definition.rs

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StepDefinition {
    pub id: String,
    pub action: StepAction,
    pub output_var: Option<String>,
    pub depends_on: Vec<String>,
    pub timeout_seconds: Option<u64>,
    pub retry_on_failure: Option<u32>,

    // NEW fields
    #[serde(default)]
    pub on_failure: OnFailure,
    pub default_value: Option<String>,
    pub fallback_step: Option<String>,
    #[serde(default)]
    pub skip_unless_fallback: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OnFailure {
    #[default]
    Fail,        // Stop the pipeline (current behavior)
    Skip,        // Skip this step, continue with next
    UseDefault,  // Use default_value as output_var
    Fallback,    // Run fallback_step instead
}
```

3. **Update the execution loop in `engine.rs`:**

```rust
// In run(), after execute_step() returns an error:
Err(e) => match &step.on_failure {
    OnFailure::Fail => {
        run.status = PipelineRunStatus::Failed;
        return Ok(run);
    }
    OnFailure::Skip => {
        step_result.status = StepStatus::Skipped;
        step_result.error = Some(format!("Skipped due to error: {e}"));
        run.step_results.push(step_result);
        continue;
    }
    OnFailure::UseDefault => {
        let default = step.default_value.clone().unwrap_or_default();
        if let Some(var) = &step.output_var {
            context.insert(var.clone(), default);
        }
        step_result.status = StepStatus::Complete;
        step_result.error = Some(format!("Used default due to error: {e}"));
        run.step_results.push(step_result);
        continue;
    }
    OnFailure::Fallback => {
        if let Some(fallback_id) = &step.fallback_step {
            // Cycle detection: track visited step IDs to prevent A->B->A loops
            let mut visited: HashSet<String> = HashSet::new();
            visited.insert(step.id.clone());

            let mut current_fallback_id = fallback_id.clone();
            let mut fallback_succeeded = false;

            loop {
                // Check for cycles
                if visited.contains(&current_fallback_id) {
                    step_result.status = StepStatus::Failed;
                    step_result.error = Some(format!(
                        "Fallback cycle detected: step '{}' already visited in chain {:?}",
                        current_fallback_id, visited
                    ));
                    run.status = PipelineRunStatus::Failed;
                    run.step_results.push(step_result);
                    return Ok(run);
                }
                visited.insert(current_fallback_id.clone());

                // Resolve the fallback step
                let fallback_step = pipeline.steps.iter()
                    .find(|s| s.id == current_fallback_id)
                    .ok_or(AgentOSError::PipelineError(
                        format!("Fallback step '{}' not found", current_fallback_id)
                    ))?
                    .clone();

                // Execute fallback with the same context
                let mut fb_result = StepResult::new(&fallback_step.id);
                match execute_step(&fallback_step, &mut context, kernel).await {
                    Ok(output) => {
                        // Apply fallback outputs to context
                        if let Some(var) = &fallback_step.output_var {
                            context.insert(var.clone(), output);
                        }
                        fb_result.status = StepStatus::Complete;
                        run.step_results.push(fb_result);
                        // Mark original step as resolved via fallback
                        step_result.status = StepStatus::Complete;
                        step_result.error = Some(format!(
                            "Original failed ({e}), resolved by fallback '{}'",
                            fallback_step.id
                        ));
                        fallback_succeeded = true;
                        break;
                    }
                    Err(fb_err) => {
                        fb_result.status = StepStatus::Failed;
                        fb_result.error = Some(format!("Fallback failed: {fb_err}"));
                        run.step_results.push(fb_result);

                        // If fallback itself has a fallback, chain to it
                        if let (OnFailure::Fallback, Some(next_id)) =
                            (&fallback_step.on_failure, &fallback_step.fallback_step)
                        {
                            current_fallback_id = next_id.clone();
                            continue;
                        }
                        // No more fallbacks — fail
                        break;
                    }
                }
            }

            if !fallback_succeeded {
                step_result.status = StepStatus::Failed;
                step_result.error = Some(format!(
                    "All fallbacks exhausted (original error: {e})"
                ));
                run.status = PipelineRunStatus::Failed;
            }
            run.step_results.push(step_result);
            if run.status == PipelineRunStatus::Failed {
                return Ok(run);
            }
            continue;
        } else {
            // on_failure = Fallback but no fallback_step specified — treat as Fail
            run.status = PipelineRunStatus::Failed;
            step_result.error = Some(format!(
                "on_failure=Fallback but no fallback_step defined (error: {e})"
            ));
            run.step_results.push(step_result);
            return Ok(run);
        }
    }
}
```

**Effort:** 4-6 hours

---

### M-5: `health_check()` Returns `bool` — Hides Error Details

**Problem:**
`LLMCore::health_check()` returns `bool`. When an adapter is unhealthy, the kernel knows "it's down" but not *why* — network timeout? auth failure? model not loaded? This makes debugging connection issues unnecessarily difficult.

**Affected files:**
- `crates/agentos-llm/src/traits.rs` (line 16)
- All adapter `health_check()` implementations

**Root cause:**
Simplicity in Phase 1.

**Detailed fix:**

```rust
// crates/agentos-llm/src/traits.rs

#[derive(Debug, Clone)]
pub enum HealthStatus {
    Healthy,
    Degraded { reason: String },  // Slow but responding
    Unhealthy { reason: String }, // Not responding
}

impl HealthStatus {
    pub fn is_healthy(&self) -> bool {
        matches!(self, HealthStatus::Healthy | HealthStatus::Degraded { .. })
    }
}

#[async_trait]
pub trait LLMCore: Send + Sync {
    // ...
    async fn health_check(&self) -> HealthStatus;
    // ...
}
```

Example adapter implementation:

```rust
// crates/agentos-llm/src/ollama.rs
async fn health_check(&self) -> HealthStatus {
    let start = Instant::now();
    match self.client.get(&format!("{}/api/tags", self.base_url))
        .timeout(Duration::from_secs(5))
        .send().await
    {
        Ok(resp) if resp.status().is_success() => {
            let latency = start.elapsed();
            if latency > Duration::from_secs(2) {
                HealthStatus::Degraded {
                    reason: format!("High latency: {}ms", latency.as_millis())
                }
            } else {
                HealthStatus::Healthy
            }
        }
        Ok(resp) => HealthStatus::Unhealthy {
            reason: format!("HTTP {}", resp.status())
        },
        Err(e) => HealthStatus::Unhealthy {
            reason: format!("Connection failed: {e}")
        },
    }
}
```

**Effort:** 2-3 hours

---

### M-6: Audit Logging Is Fire-and-Forget (Errors Silenced)

**Problem:**
Throughout the kernel, audit log calls use `.ok()` to silence errors:

```rust
self.audit.log_event(...).ok();  // Error swallowed
```

If the audit SQLite database is corrupted, full, or locked, the kernel continues operating with zero visibility. For a system where "immutable audit log" is a core security feature, silently losing audit entries defeats the purpose.

**Affected files:**
- `crates/agentos-kernel/src/kernel.rs` (every `.ok()` call on audit)
- `crates/agentos-audit/src/log.rs`

**Root cause:**
Audit failures shouldn't crash the kernel. The `.ok()` pattern was a pragmatic choice to avoid cascading failures.

**Detailed fix:**

Replace `.ok()` with a monitored error handler that tracks audit health:

```rust
// crates/agentos-audit/src/log.rs

pub struct AuditLog {
    db: Connection,
    error_count: AtomicU64,
    last_error: RwLock<Option<String>>,
}

impl AuditLog {
    pub fn log_event(&self, /* ... */) -> Result<(), AuditError> {
        match self.write_entry(/* ... */) {
            Ok(()) => Ok(()),
            Err(e) => {
                self.error_count.fetch_add(1, Ordering::Relaxed);
                *self.last_error.write().unwrap() = Some(e.to_string());

                // Log to stderr as last resort — never silently drop
                eprintln!("[AUDIT FAILURE] {e}");

                Err(e)
            }
        }
    }

    pub fn health(&self) -> AuditHealth {
        AuditHealth {
            errors: self.error_count.load(Ordering::Relaxed),
            last_error: self.last_error.read().unwrap().clone(),
        }
    }
}
```

In the kernel, replace `.ok()` with:

```rust
if let Err(e) = self.audit.log_event(/* ... */) {
    tracing::error!(error = %e, "Audit log write failed");
    // Optionally: if error_count > threshold, emit KernelDegraded status
}
```

**Effort:** 2-3 hours

---

### M-7: No Observability Beyond Audit Logs (No Metrics/Tracing)

**Problem:**
The only observability mechanism is the append-only audit log. There are no structured traces (spans for intent lifecycle), no Prometheus-compatible metrics (task queue depth, inference latency histogram, token usage counters), and no distributed tracing support. Production debugging requires manually querying SQLite.

**Affected files:**
- All crates (need instrumentation)
- `Cargo.toml` (new dependencies)

**Root cause:**
Phase 1 focused on functionality, not operational visibility.

**Detailed fix:**

1. **Add `tracing` instrumentation to hot paths:**

The project likely already uses `tracing` (via `tracing::debug!` calls in the tool runner). Expand coverage:

```rust
// crates/agentos-kernel/src/kernel.rs
use tracing::{instrument, info_span, Instrument};

#[instrument(skip(self, payload), fields(tool = %tool_name, agent = %agent_id))]
async fn execute_tool(&self, tool_name: &str, agent_id: &AgentID, payload: Value) -> Result<Value> {
    // ... existing logic ...
}
```

2. **Add Prometheus metrics via the `metrics` crate:**

```rust
// crates/agentos-kernel/src/metrics.rs

use metrics::{counter, gauge, histogram};

pub fn record_task_queued() {
    counter!("agentos_tasks_queued_total").increment(1);
    gauge!("agentos_task_queue_depth").increment(1.0);
}

pub fn record_task_completed(duration_ms: u64) {
    gauge!("agentos_task_queue_depth").decrement(1.0);
    counter!("agentos_tasks_completed_total").increment(1);
    histogram!("agentos_task_duration_ms").record(duration_ms as f64);
}

// Allowed label values to prevent cardinality explosion in metrics storage.
// Unknown values are replaced with "other" to keep label cardinality bounded.
const KNOWN_PROVIDERS: &[&str] = &["ollama", "openai", "anthropic", "gemini", "custom"];
const KNOWN_MODELS: &[&str] = &[
    "gpt-4", "gpt-4o", "gpt-3.5-turbo",
    "claude-3-opus", "claude-3-sonnet", "claude-3-haiku",
    "gemini-pro", "gemini-ultra",
    "llama3", "llama3.1", "mistral", "mixtral", "phi-3", "codellama",
];

/// Normalizes a label value against an allowlist; returns "other" if not recognized.
fn normalize_label<'a>(value: &'a str, allowed: &[&str]) -> &'a str {
    if allowed.contains(&value) { value } else { "other" }
}

pub fn record_inference(provider: &str, model: &str, input_tokens: u64, output_tokens: u64, latency_ms: u64) {
    let provider = normalize_label(provider, KNOWN_PROVIDERS);
    let model = normalize_label(model, KNOWN_MODELS);
    counter!("agentos_inference_total", "provider" => provider.to_string(), "model" => model.to_string()).increment(1);
    counter!("agentos_tokens_input_total", "provider" => provider.to_string()).increment(input_tokens);
    counter!("agentos_tokens_output_total", "provider" => provider.to_string()).increment(output_tokens);
    histogram!("agentos_inference_latency_ms", "provider" => provider.to_string()).record(latency_ms as f64);
}

// Allowed tool names — extend as new core tools are added.
// Community/custom tools are bucketed under "other".
const KNOWN_TOOLS: &[&str] = &[
    "file_read", "file_write", "file_list", "shell_exec",
    "web_search", "web_fetch", "memory_store", "memory_recall",
];

pub fn record_tool_execution(tool_name: &str, duration_ms: u64, success: bool) {
    let tool = normalize_label(tool_name, KNOWN_TOOLS);
    counter!("agentos_tool_executions_total", "tool" => tool.to_string(), "success" => success.to_string()).increment(1);
    histogram!("agentos_tool_duration_ms", "tool" => tool.to_string()).record(duration_ms as f64);
}
```

3. **Expose a `/metrics` HTTP endpoint** (can be added to the Web UI crate or as a standalone Axum handler on port 9090):

```rust
// crates/agentos-web/src/metrics_endpoint.rs
use axum::{routing::get, Router};
use metrics_exporter_prometheus::PrometheusBuilder;

pub fn metrics_router() -> Router {
    let handle = PrometheusBuilder::new()
        .install_recorder()
        .expect("failed to install Prometheus recorder");

    Router::new().route("/metrics", get(move || async move {
        handle.render()
    }))
}
```

4. **Add to `Cargo.toml`:**

```toml
[dependencies]
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
metrics = "0.24"
metrics-exporter-prometheus = "0.16"
```

**Effort:** 6-8 hours

---

### M-8: No Health/Readiness Endpoints for Container Orchestration

**Problem:**
Docker and Kubernetes need HTTP health check endpoints to manage container lifecycle. Without them, the orchestrator cannot distinguish a healthy kernel from one that has degraded silently (e.g., all LLMs disconnected, audit log full, vault locked).

**Affected files:**
- `crates/agentos-web/src/` (stub crate — needs basic implementation)
- `Dockerfile` (HEALTHCHECK instruction)

**Root cause:**
Web UI deferred to Phase 5. Health endpoints should have been Phase 1.

**Detailed fix:**

1. **Add a minimal Axum server in the kernel (not the web UI crate):**

```rust
// crates/agentos-kernel/src/health.rs
use axum::{routing::get, Router, Json};
use serde::Serialize;

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    uptime_seconds: u64,
    connected_agents: usize,
    active_tasks: usize,
    audit_healthy: bool,
    vault_healthy: bool,
}

pub fn health_router(kernel: Arc<Kernel>) -> Router {
    let k1 = kernel.clone();
    let k2 = kernel.clone();

    Router::new()
        .route("/healthz", get(move || {
            let k = k1.clone();
            async move {
                Json(serde_json::json!({ "status": "ok" }))
            }
        }))
        .route("/readyz", get(move || {
            let k = k2.clone();
            async move {
                let agents = k.agent_registry.read().await.count();
                if agents == 0 {
                    return (axum::http::StatusCode::SERVICE_UNAVAILABLE,
                        Json(serde_json::json!({ "status": "not ready", "reason": "no agents connected" })));
                }
                (axum::http::StatusCode::OK,
                    Json(serde_json::json!({ "status": "ready", "agents": agents })))
            }
        }))
}
```

2. **Start the health server during kernel boot** on a configurable port (default 9091):

```rust
// In kernel boot:
let health_addr = SocketAddr::from(([0, 0, 0, 0], config.kernel.health_port));
let health_router = health::health_router(kernel.clone());
tokio::spawn(axum::serve(TcpListener::bind(health_addr).await?, health_router).into_future());
```

3. **Add Docker HEALTHCHECK:**

```dockerfile
HEALTHCHECK --interval=30s --timeout=5s --retries=3 \
  CMD wget -qO- http://localhost:9091/healthz || exit 1
```

**Effort:** 2-3 hours

---

## LOW Priority

---

### L-1: `agentos-sdk` Proc Macro Not Started — Blocks Community Tools

**Problem:**
The design document showcases a `#[tool(...)]` proc macro for the Rust SDK:

```rust
#[tool(name = "web-search", version = "1.0.0", ...)]
async fn web_search(ctx: ToolContext, intent: WebSearchIntent) -> ToolResult<SearchResults> { ... }
```

This doesn't exist. Third-party developers must manually implement the `AgentTool` trait, write a TOML manifest, and register with the tool runner — significant friction that will prevent community adoption.

**Affected files:**
- New crate: `crates/agentos-sdk/`

**Root cause:**
Scheduled for Phase 5.

**Detailed fix:**

1. **Create the crate:**

```
crates/agentos-sdk/
├── Cargo.toml
├── src/
│   ├── lib.rs           # Re-exports
│   └── tool_macro.rs    # The proc macro
└── agentos-sdk-macros/  # Proc macro must be separate crate
    ├── Cargo.toml
    └── src/lib.rs
```

2. **Implement the proc macro:**

```rust
// crates/agentos-sdk/agentos-sdk-macros/src/lib.rs
use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, ItemFn, AttributeArgs};

#[proc_macro_attribute]
pub fn tool(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attrs = parse_macro_input!(attr as AttributeArgs);
    let func = parse_macro_input!(item as ItemFn);

    // Extract metadata from attributes
    let name = extract_str_attr(&attrs, "name");
    let version = extract_str_attr(&attrs, "version");
    let description = extract_str_attr(&attrs, "description");
    let capabilities = extract_list_attr(&attrs, "capabilities_required");

    let func_name = &func.sig.ident;
    let struct_name = to_pascal_case(&name);

    // Capture the function tokens once so they can be reused in the expansion
    // without moving `func` twice.
    let func_tokens = quote! { #func };

    let expanded = quote! {
        pub struct #struct_name;

        // Emit the original function so it remains callable by name.
        #func_tokens

        #[async_trait::async_trait]
        impl agentos_tools::traits::AgentTool for #struct_name {
            fn name(&self) -> &str { #name }
            fn description(&self) -> &str { #description }
            fn version(&self) -> &str { #version }

            fn get_required_permissions(&self) -> Vec<String> {
                vec![#(#capabilities.to_string()),*]
            }

            async fn execute(
                &self,
                payload: serde_json::Value,
                context: &agentos_tools::traits::ToolExecutionContext,
            ) -> Result<serde_json::Value, agentos_types::AgentOSError> {
                let result = #func_name(payload, context).await?;
                Ok(serde_json::to_value(result)?)
            }
        }
    };

    TokenStream::from(expanded)
}
```

3. **Publish as `agentos-sdk` on crates.io** once stable.

**Effort:** 8-12 hours (proc macros are tricky to get right)

---

### L-2: Missing HAL Drivers (Sensor, GPIO, Storage, GPU)

**Problem:**
The design document specifies 7 HAL drivers: System, Process, Network, LogReader, Sensor, GPIO, Storage, GPU. Only 4 are implemented (System, Process, Network, LogReader). The missing drivers mean AgentOS cannot fulfil its IoT/embedded/GPU use cases.

**Affected files:**
- `crates/agentos-hal/src/drivers/` (need new files)

**Root cause:**
Scheduled for Phase 3. System/Process/Network/LogReader were implemented first because they work on any Linux host. Sensor/GPIO require specific hardware. GPU requires CUDA/Vulkan detection.

**Detailed fix:**

**GPU Driver (highest value):**

```rust
// crates/agentos-hal/src/drivers/gpu.rs

use serde_json::{json, Value};

pub struct GpuDriver;

impl GpuDriver {
    pub fn new() -> Self { Self }
}

#[async_trait]
impl HalDriver for GpuDriver {
    fn name(&self) -> &str { "gpu" }
    fn resource_class(&self) -> &str { "hardware.gpu" }

    async fn execute(&self, action: &str, _params: &Value) -> Result<Value, AgentOSError> {
        match action {
            "list" => {
                // Use nvml-wrapper crate for NVIDIA, or fallback to sysfs
                #[cfg(feature = "nvidia")]
                {
                    let nvml = nvml_wrapper::Nvml::init()?;
                    let count = nvml.device_count()?;
                    let mut devices = Vec::new();
                    for i in 0..count {
                        let device = nvml.device_by_index(i)?;
                        let mem = device.memory_info()?;
                        devices.push(json!({
                            "index": i,
                            "name": device.name()?,
                            "vram_total_mb": mem.total / 1_048_576,
                            "vram_used_mb": mem.used / 1_048_576,
                            "vram_free_mb": mem.free / 1_048_576,
                            "temperature_c": device.temperature(nvml_wrapper::enum_wrappers::device::TemperatureSensor::Gpu)?,
                            "utilization_percent": device.utilization_rates()?.gpu,
                            "backend": "CUDA",
                        }));
                    }
                    Ok(json!({ "devices": devices }))
                }

                #[cfg(not(feature = "nvidia"))]
                {
                    // Fallback: check /sys/class/drm/
                    Ok(json!({ "devices": [], "note": "No GPU driver feature enabled" }))
                }
            }
            "allocate" => {
                // VRAM allocation tracking (in-memory, not actual GPU allocation)
                todo!("GPU allocation manager")
            }
            _ => Err(AgentOSError::InvalidAction(action.to_string())),
        }
    }
}
```

**Sensor Driver (for IoT):**

```rust
// crates/agentos-hal/src/drivers/sensor.rs
// Reads from sysfs thermal zones, hwmon, or I2C devices

pub struct SensorDriver;

#[async_trait]
impl HalDriver for SensorDriver {
    fn name(&self) -> &str { "sensor" }
    fn resource_class(&self) -> &str { "hardware.sensors" }

    async fn execute(&self, action: &str, params: &Value) -> Result<Value, AgentOSError> {
        match action {
            "read_temperature" => {
                // Read from /sys/class/thermal/thermal_zone*/temp
                let mut readings = Vec::new();
                for entry in std::fs::read_dir("/sys/class/thermal/")? {
                    let entry = entry?;
                    if entry.file_name().to_string_lossy().starts_with("thermal_zone") {
                        let temp_path = entry.path().join("temp");
                        if let Ok(temp_str) = std::fs::read_to_string(&temp_path) {
                            let millidegrees: f64 = temp_str.trim().parse().unwrap_or(0.0);
                            readings.push(json!({
                                "zone": entry.file_name().to_string_lossy(),
                                "celsius": millidegrees / 1000.0,
                            }));
                        }
                    }
                }
                Ok(json!({ "temperatures": readings }))
            }
            _ => Err(AgentOSError::InvalidAction(action.to_string())),
        }
    }
}
```

**GPIO Driver:**

```rust
// crates/agentos-hal/src/drivers/gpio.rs
// Uses /sys/class/gpio or the gpiod crate

pub struct GpioDriver;

#[async_trait]
impl HalDriver for GpioDriver {
    fn name(&self) -> &str { "gpio" }
    fn resource_class(&self) -> &str { "hardware.gpio" }

    async fn execute(&self, action: &str, params: &Value) -> Result<Value, AgentOSError> {
        match action {
            "read" => {
                let pin: u32 = params["pin"].as_u64().unwrap_or(0) as u32;
                // Read via libgpiod or sysfs
                let value = std::fs::read_to_string(
                    format!("/sys/class/gpio/gpio{}/value", pin)
                ).map_err(|e| AgentOSError::HardwareError(e.to_string()))?;
                Ok(json!({ "pin": pin, "value": value.trim().parse::<u8>().unwrap_or(0) }))
            }
            "write" => {
                let pin: u32 = params["pin"].as_u64().unwrap_or(0) as u32;
                let value: u8 = params["value"].as_u64().unwrap_or(0) as u8;
                std::fs::write(
                    format!("/sys/class/gpio/gpio{}/value", pin),
                    value.to_string(),
                ).map_err(|e| AgentOSError::HardwareError(e.to_string()))?;
                Ok(json!({ "pin": pin, "value": value, "status": "written" }))
            }
            _ => Err(AgentOSError::InvalidAction(action.to_string())),
        }
    }
}
```

**Register all new drivers in the kernel boot sequence:**

```rust
// In kernel.rs boot()
hal.register_driver(Box::new(GpuDriver::new()));
hal.register_driver(Box::new(SensorDriver::new()));
hal.register_driver(Box::new(GpioDriver::new()));
```

**Effort:** 6-10 hours total (GPU is most complex due to NVML FFI)

---

### L-3: No CI/CD Pipeline

**Problem:**
There is no GitHub Actions workflow. Changes to any of the 16 crates can break other crates silently. No automated linting, testing, or build verification.

**Affected files:**
- New: `.github/workflows/ci.yml`

**Root cause:**
Not yet set up.

**Detailed fix:**

```yaml
# .github/workflows/ci.yml
name: CI

on:
  push:
    branches: [main]
  pull_request:
    branches: [main]

env:
  CARGO_TERM_COLOR: always
  RUSTFLAGS: "-D warnings"

jobs:
  check:
    name: Check & Lint
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@v2

      - name: Check formatting
        run: cargo fmt --all -- --check

      - name: Clippy
        run: cargo clippy --workspace --all-targets -- -D warnings

  test:
    name: Test
    runs-on: ubuntu-latest
    needs: check
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2

      - name: Run tests
        run: cargo test --workspace

  build:
    name: Build Release
    runs-on: ubuntu-latest
    needs: test
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2

      - name: Build release
        run: cargo build --release --workspace

  docker:
    name: Docker Build
    runs-on: ubuntu-latest
    needs: build
    steps:
      - uses: actions/checkout@v4
      - name: Build Docker image
        run: docker build -t agentos:ci .

  security:
    name: Security Audit
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - name: Install cargo-audit
        run: cargo install cargo-audit
      - name: Audit dependencies
        run: cargo audit --deny warnings
```

**Effort:** 1-2 hours

---

### L-4: Schema Validation for `SemanticPayload` Deferred

**Problem:**
`SemanticPayload` contains a `schema: String` field and a `data: serde_json::Value` field. The schema field is informational only — no actual validation happens. A tool expecting `FileReadIntent` can receive arbitrary JSON. The comment in the code says "Phase 2+ — validate against registered schemas."

**Affected files:**
- `crates/agentos-types/src/intent.rs` (SemanticPayload)
- `crates/agentos-kernel/src/kernel.rs` (where intents are dispatched)

**Root cause:**
Intentionally deferred. Phase 1 uses unvalidated JSON.

**Detailed fix:**

1. **Add a schema registry to the kernel:**

```rust
// crates/agentos-types/src/schema.rs
use jsonschema::JSONSchema;
use std::collections::HashMap;

pub struct SchemaRegistry {
    schemas: HashMap<String, JSONSchema>,
}

impl SchemaRegistry {
    pub fn new() -> Self {
        Self { schemas: HashMap::new() }
    }

    pub fn register(&mut self, name: &str, schema_json: &serde_json::Value) -> Result<(), String> {
        let compiled = JSONSchema::compile(schema_json)
            .map_err(|e| format!("Invalid schema '{}': {}", name, e))?;
        self.schemas.insert(name.to_string(), compiled);
        Ok(())
    }

    pub fn validate(&self, schema_name: &str, data: &serde_json::Value) -> Result<(), Vec<String>> {
        let schema = self.schemas.get(schema_name)
            .ok_or_else(|| vec![format!("Unknown schema: {}", schema_name)])?;

        let result = schema.validate(data);
        match result {
            Ok(()) => Ok(()),
            Err(errors) => Err(errors.map(|e| e.to_string()).collect()),
        }
    }
}
```

2. **Load tool schemas from manifests during tool registration:**

```toml
# In tool manifest:
[intent_schema]
input = "FileReadIntent"
input_json_schema = """
{
  "type": "object",
  "required": ["path"],
  "properties": {
    "path": { "type": "string" },
    "encoding": { "type": "string", "default": "utf-8" }
  }
}
"""
```

3. **Validate in the intent dispatch path:**

```rust
// Before tool execution:
if let Some(errors) = schema_registry.validate(&intent.payload.schema, &intent.payload.data).err() {
    return Ok(IntentResult {
        status: IntentResultStatus::SchemaValidationError,
        error: Some(format!("Payload validation failed: {}", errors.join(", "))),
        ..Default::default()
    });
}
```

4. **Add `jsonschema` dependency:**

```toml
# Cargo.toml
jsonschema = "0.18"
```

**Effort:** 4-6 hours

---

### L-5: No TLS on Unix Domain Socket

**Problem:**
The intent bus uses a Unix domain socket (`/tmp/agentos.sock`). This is fine for single-container deployments where all processes are local. However, if AgentOS ever exposes the bus over TCP (for AgentOS Cloud or multi-container setups), there is no encryption layer. Agent credentials and intent payloads would travel in plaintext.

**Affected files:**
- `crates/agentos-bus/src/server.rs`
- `crates/agentos-bus/src/client.rs`

**Root cause:**
Unix sockets are inherently secure within a single container (filesystem permissions gate access). Network transport was not yet needed.

**Detailed fix:**

This is a forward-looking improvement. The recommended approach:

1. **Keep Unix sockets for local communication** (no change needed).

2. **Add a TCP listener behind `rustls` for remote connections:**

```rust
// crates/agentos-bus/src/tls_server.rs
use tokio_rustls::TlsAcceptor;
use rustls::{ServerConfig, Certificate, PrivateKey};

pub struct TlsBusServer {
    acceptor: TlsAcceptor,
    listener: TcpListener,
}

impl TlsBusServer {
    pub async fn bind(addr: SocketAddr, cert_path: &Path, key_path: &Path) -> Result<Self> {
        let certs = load_certs(cert_path)?;
        let key = load_key(key_path)?;

        let config = ServerConfig::builder()
            .with_safe_defaults()
            .with_no_client_auth()   // or mutual TLS for agent auth
            .with_single_cert(certs, key)?;

        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind(addr).await?;

        Ok(Self { acceptor, listener })
    }
}
```

3. **Configuration in `default.toml`:**

```toml
[bus]
socket_path = "/tmp/agentos.sock"      # local (always enabled)
tcp_listen = "0.0.0.0:9090"            # remote (optional)
tls_cert = "/opt/agentos/certs/server.crt"
tls_key = "/opt/agentos/certs/server.key"
```

4. **Dependencies:**

```toml
tokio-rustls = "0.26"
rustls = "0.23"
rustls-pemfile = "2"
```

**Effort:** 4-6 hours (when TCP transport is actually needed)

---

### L-6: Pipeline Template Variables Use Simple Regex — No Escaping

**Problem:**
The pipeline engine renders template variables using the regex `\{([a-zA-Z_][a-zA-Z0-9_]*)\}`. Unresolved variables are left as-is in the output string. This has two issues:

1. If a step output naturally contains `{some_text}`, it will be treated as a variable reference and replaced (or left as a confusing literal if unresolved).
2. No way to escape braces (e.g., `\{literal\}` or `{{literal}}`).

**Affected files:**
- `crates/agentos-pipeline/src/engine.rs` (lines 224-234, `render_template()`)

**Root cause:**
Simplest possible implementation for Phase 1.

**Detailed fix:**

Switch to `{{variable}}` syntax (double braces) which avoids conflicts with JSON and most natural language:

```rust
fn render_template(template: &str, context: &HashMap<String, String>) -> String {
    // Use double-brace syntax: {{variable_name}}
    let re = Regex::new(r"\{\{([a-zA-Z_][a-zA-Z0-9_]*)\}\}").unwrap();

    re.replace_all(template, |caps: &regex::Captures| {
        let var_name = &caps[1];
        context.get(var_name)
            .cloned()
            .unwrap_or_else(|| {
                tracing::warn!(var = var_name, "Unresolved pipeline variable");
                format!("{{{{UNRESOLVED:{}}}}}", var_name)
            })
    }).to_string()
}
```

Update existing pipeline YAML fixtures to use `{{var}}` instead of `{var}`.

**Effort:** 1-2 hours

---

### L-7: Context Window Overflow Strategy Not Configurable

**Problem:**
The design document mentions "context overflow via summarization or eviction strategies." The `ContextManager` has a configurable window size but uses a simple strategy (likely FIFO eviction). There is no summarization pass, no importance scoring, and no user-configurable strategy selection.

**Affected files:**
- `crates/agentos-kernel/src/context.rs`

**Root cause:**
Phase 1 focused on getting context windows working. Smart overflow requires an LLM call (for summarization), which adds complexity and cost.

**Detailed fix:**

1. **Define overflow strategies:**

```rust
// crates/agentos-kernel/src/context.rs

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverflowStrategy {
    /// Drop oldest entries (current behavior)
    FifoEviction,

    /// Summarize oldest entries into a single compressed entry
    Summarize {
        /// Use this LLM to generate summaries
        summarizer_agent: Option<String>,
        /// Compress when context exceeds this percentage of max
        trigger_threshold_percent: u8,  // e.g., 80
        /// Target size after compression (percentage of max)
        target_percent: u8,             // e.g., 50
    },

    /// Keep system prompt + most recent N entries, drop middle
    SlidingWindow {
        /// Always keep the first N entries (system context)
        keep_head: usize,
        /// Always keep the last N entries (recent context)
        keep_tail: usize,
    },

    /// Score entries by importance and drop lowest-scored
    ImportanceScored {
        /// Tool results > agent messages > user prompts > system events
        weights: HashMap<String, f32>,
    },
}
```

2. **Implement in the context manager's overflow handler:**

```rust
impl ContextManager {
    pub async fn handle_overflow(
        &self,
        window: &mut ContextWindow,
        strategy: &OverflowStrategy,
        llm: Option<&dyn LLMCore>,
    ) {
        match strategy {
            OverflowStrategy::FifoEviction => {
                while window.token_count() > window.max_tokens {
                    window.entries.remove(0);
                }
            }
            OverflowStrategy::Summarize { trigger_threshold_percent, target_percent, .. } => {
                let threshold = window.max_tokens * (*trigger_threshold_percent as usize) / 100;
                if window.token_count() < threshold { return; }

                let target = window.max_tokens * (*target_percent as usize) / 100;

                // Collect oldest entries until removing them would bring us under target
                let mut entries_size = 0usize;
                let mut num_to_summarize = 0usize;
                for entry in window.entries.iter() {
                    if window.token_count() - entries_size <= target {
                        break;
                    }
                    entries_size += entry.token_count();
                    num_to_summarize += 1;
                }

                if num_to_summarize == 0 { return; }

                let entries_to_summarize = &window.entries[..num_to_summarize];
                let original_content: String = entries_to_summarize
                    .iter()
                    .map(|e| e.content.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                let original_size = original_content.len();

                if let Some(llm) = llm {
                    const MAX_SUMMARIZE_ATTEMPTS: u32 = 3;
                    let mut summary_content: Option<String> = None;

                    for attempt in 0..MAX_SUMMARIZE_ATTEMPTS {
                        let max_summary_len = original_size / (attempt as usize + 2);
                        let summary_prompt = format!(
                            "Summarize the following conversation history into key facts \
                             and decisions. Keep the summary under {} characters:\n{}",
                            max_summary_len, original_content
                        );

                        match llm.infer(&ContextWindow::single(summary_prompt)).await {
                            Ok(result) => {
                                if result.content.len() < original_size {
                                    summary_content = Some(result.content);
                                    break;
                                }
                                // Summary is not smaller — try with tighter constraint
                                tracing::warn!(
                                    attempt = attempt + 1,
                                    summary_len = result.content.len(),
                                    original_len = original_size,
                                    "Summary not smaller than original, retrying with tighter limit"
                                );
                                // On last attempt, forcibly truncate
                                if attempt == MAX_SUMMARIZE_ATTEMPTS - 1 {
                                    let truncated: String = result.content
                                        .chars()
                                        .take(original_size / 2)
                                        .collect();
                                    summary_content = Some(truncated);
                                }
                            }
                            Err(e) => {
                                tracing::error!("Summarization LLM call failed: {e}");
                                break;
                            }
                        }
                    }

                    // Only replace entries if we got a valid, smaller summary
                    if let Some(summary) = summary_content {
                        window.entries.drain(0..num_to_summarize);
                        window.entries.insert(0, ContextEntry {
                            role: ContextRole::System,
                            content: format!(
                                "[CONTEXT SUMMARY]\n{}\n[/CONTEXT SUMMARY]",
                                summary
                            ),
                            timestamp: chrono::Utc::now(),
                        });
                    }
                }
            }
            // ... other strategies
        }
    }
}
```

3. **Make configurable in `default.toml`:**

```toml
[kernel]
context_overflow_strategy = "sliding_window"

[kernel.sliding_window]
keep_head = 5
keep_tail = 20
```

**Effort:** 4-6 hours

---

## Summary Table

| ID | Issue | Priority | Effort | Crates Affected |
|----|-------|----------|--------|-----------------|
| H-1 | Split kernel.rs into modules | HIGH | 4-6h | agentos-kernel |
| H-2 | Run loop crash recovery / supervisor | HIGH | 3-4h | agentos-kernel |
| H-3 | IntentTarget missing Agent/Hardware | HIGH | 6-8h | agentos-types, agentos-kernel, agentos-capability |
| H-4 | Tool output sanitization | HIGH | 2-3h | agentos-tools, agentos-kernel |
| H-5 | HMAC key lost on restart | HIGH | 3-4h | agentos-capability, agentos-kernel |
| H-6 | No integration test | HIGH | 4-6h | agentos-cli (tests), agentos-llm |
| H-7 | Permission enforcement gap in tool runner | HIGH | 1-2h | agentos-tools |
| M-1 | No rate limiting on intent bus | MEDIUM | 3-4h | agentos-bus, agentos-kernel |
| M-2 | Semantic search is O(n) | MEDIUM | 4-6h | agentos-memory |
| M-3 | No streaming inference | MEDIUM | 6-8h | agentos-llm (all adapters) |
| M-4 | Pipeline step-level error handling | MEDIUM | 4-6h | agentos-pipeline |
| M-5 | health_check() hides errors | MEDIUM | 2-3h | agentos-llm |
| M-6 | Audit log errors silenced | MEDIUM | 2-3h | agentos-audit, agentos-kernel |
| M-7 | No metrics/tracing | MEDIUM | 6-8h | all crates |
| M-8 | No health/readiness endpoints | MEDIUM | 2-3h | agentos-kernel |
| L-1 | agentos-sdk proc macro | LOW | 8-12h | new crate |
| L-2 | Missing HAL drivers (GPU, Sensor, GPIO) | LOW | 6-10h | agentos-hal |
| L-3 | No CI/CD pipeline | LOW | 1-2h | .github/workflows/ |
| L-4 | Schema validation for SemanticPayload | LOW | 4-6h | agentos-types, agentos-kernel |
| L-5 | No TLS on bus (future-proofing) | LOW | 4-6h | agentos-bus |
| L-6 | Pipeline template variable escaping | LOW | 1-2h | agentos-pipeline |
| L-7 | Context overflow strategy not configurable | LOW | 4-6h | agentos-kernel |

---

## Recommended Execution Order

```
Sprint 1 (Week 1-2):  H-1, H-2, H-4, H-7       — Structural health + security basics
Sprint 2 (Week 3-4):  H-5, H-3, H-6             — Token persistence + intent completeness + testing
Sprint 3 (Week 5-6):  M-6, M-5, M-8, L-3        — Observability foundation + CI
Sprint 4 (Week 7-8):  M-1, M-2, M-4             — Performance + resilience
Sprint 5 (Week 9-10): M-3, M-7                   — Streaming + full observability
Sprint 6 (Week 11+):  L-1, L-2, L-4, L-5, L-6, L-7 — SDK + HAL + polish
```

---

> **Total estimated effort:** 75-110 hours across all issues.
> **Critical path (HIGH only):** 24-33 hours — achievable in 2 focused weeks.
