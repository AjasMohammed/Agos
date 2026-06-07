use agentos_types::*;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::Mutex;

/// Convenience alias returned by [`AuditLog::load_health_debounce`].
pub type HealthDebounceMap = std::collections::HashMap<String, chrono::DateTime<chrono::Utc>>;

pub struct AuditLog {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuditEventType {
    // Task lifecycle
    TaskCreated,
    TaskStateChanged,
    TaskCompleted,
    TaskFailed,
    TaskTimeout,

    // Intent processing
    IntentReceived,
    IntentRouted,
    IntentCompleted,
    IntentFailed,

    // Capability / Permission
    PermissionGranted,
    PermissionRevoked,
    PermissionDenied,
    TokenIssued,
    TokenExpired,

    // REST API / control-plane auth (React control panel)
    /// An operator authenticated successfully at `POST /api/v1/auth/login`.
    ApiLoginSucceeded,
    /// A login attempt failed (bad credential). Rate-limited; never logs the credential.
    ApiLoginFailed,
    /// An API key was minted (via login or `POST /api/v1/keys`). Records the key id, never the secret.
    ApiKeyIssued,
    /// An API key was revoked (via `DELETE /api/v1/keys/{id}`).
    ApiKeyRevoked,

    // Tool events
    ToolInstalled,
    ToolRemoved,
    ToolExecutionStarted,
    ToolExecutionCompleted,
    ToolExecutionFailed,
    /// Emitted when an agent queries the manual/discovery surface.
    ManualQuery,
    /// Emitted when the kernel injects tool suggestions after a ToolNotFound error.
    ToolSuggested,

    // LLM events
    AgentConnected,
    AgentReconnected,
    AgentDisconnected,
    /// Emitted when an agent is permanently removed from the ecosystem: profile,
    /// pubkey, memory tiers, scratchpad, inboxes, memory blocks, checkpoints, and
    /// schedules are all wiped. Vault secrets and the audit log are preserved.
    AgentRemoved,
    /// Emitted when the pre-flight health check against an LLM backend fails at
    /// `agent connect` time and the agent registration is aborted.
    LLMConnectionFailed,
    LLMInferenceStarted,
    LLMInferenceCompleted,
    LLMInferenceError,

    // Secret events
    SecretCreated,
    SecretAccessed,
    SecretRevoked,
    SecretRotated,

    // System events
    KernelStarted,
    KernelShutdown,
    KernelSubsystemRestarted,
    /// Emitted when a kernel runtime configuration parameter is changed (e.g. log level).
    KernelConfigChanged,

    // agentd - Schedule
    ScheduledJobCreated,
    ScheduledJobFired,
    ScheduledJobPaused,
    ScheduledJobResumed,
    ScheduledJobDeleted,
    ScheduledToolFired,
    ScheduledToolFailed,

    // agentd - Timers
    TimerCreated,
    TimerFired,
    TimerCancelled,
    TimerActionFailed,

    // agentd - Background Tasks
    BackgroundTaskStarted,
    BackgroundTaskCompleted,
    BackgroundTaskFailed,
    BackgroundTaskKilled,

    // Budget enforcement
    BudgetWarning,
    BudgetExceeded,

    // Risk classification
    RiskEscalation,
    ActionForbidden,

    // Privileged host operations (host-package-install)
    /// Emitted when a host package was successfully installed via the
    /// `host-package-install` tool (privileged executor). Payload should
    /// include `package`, `version`, `manager`, `escalation_id`,
    /// `approver_user_id`, `agent_id`, `task_id`, and `duration_ms`.
    HostPackageInstalled,
    /// Emitted when a host package install attempt failed (mgr error,
    /// non-zero exit, allowlist rejection, missing escalator, etc.).
    HostPackageInstallDenied,
    /// Emitted when a host package install exceeded its time budget and
    /// was killed by the kernel.
    HostPackageInstallTimeout,

    // Escalation broadcast (approval-channel-fanout)
    /// Emitted by `ChannelBroadcastSink` when an approval prompt was NOT
    /// delivered to a paired channel sender — either because of dedupe
    /// (a recent equivalent escalation already broadcast) or per-sender
    /// rate limiting. Payload includes `escalation_id`, `reason`
    /// ("duplicate" or "rate_limited"), and the channel/sender when
    /// applicable. Operators can monitor this to know when a control-
    /// plane prompt was withheld.
    EscalationBroadcastSuppressed,

    // Checkpoint / Snapshot (Spec §5)
    SnapshotTaken,
    SnapshotRestored,
    SnapshotExpired,

    // Task checkpoint recovery
    CheckpointWritten,
    CheckpointRestored,
    CheckpointPruned,

    // Atomic task checkout (single-owner dispatch claim)
    TaskCheckedOut,
    TaskCheckoutReclaimed,

    // Cost attribution (Spec §4)
    CostAttribution,

    // Event trigger system
    EventEmitted,
    EventSubscriptionCreated,
    EventSubscriptionRemoved,
    EventDelivered,
    EventThrottled,
    EventFilterRejected,
    EventLoopDetected,
    EventTriggeredTask,
    EventTriggerFailed,
    /// Emitted when the event channel is full and an event had to be dropped.
    /// Written directly to the audit log (not via the event system) to avoid recursion.
    EventChannelFull,

    // Hardware Abstraction Layer (Spec §9)
    HardwareDeviceDetected,
    HardwareDeviceApproved,
    HardwareDeviceDenied,
    HardwareDeviceRevoked,
    DeviceAccessGranted,
    DeviceAccessDenied,
    DeviceApproved,
    DeviceQuarantined,
    DeviceAccessEscalated,
    PrintJobSubmitted,
    PrintJobCancelled,
    DisplayConfigApplied,
    DisplayConfigReverted,

    // Agent identity / pubkey events
    /// Emitted when a pubkey is successfully registered for an agent (first-time only).
    PubkeyRegistered,
    /// Emitted when pubkey re-registration is rejected because a different key is already set.
    PubkeyRegistrationDenied,

    // Agent feedback / tester findings
    TestFindingCaptured,

    /// Emitted when the kernel recovered one or more tool calls from JSON-in-markdown
    /// fences in an LLM response that returned no structured tool_use blocks. Common
    /// on small models (gemma, llama3, phi-3) without reliable native function calling.
    ToolCallRecovered,

    // Proxy token lifecycle
    /// Emitted when outstanding proxy tokens are invalidated because the
    /// underlying secret was rotated or deleted.
    ProxyTokensRevoked,

    // Audit integrity
    /// Emitted at kernel startup when the audit hash chain fails verification,
    /// indicating possible tampering with historical audit entries.
    AuditChainTampered,

    // Output sanitization
    /// Emitted when the chat output sanitizer detected a fenced ```json
    /// tool-intent block in an LLM's visible text and stripped it. When the
    /// adapter returned no native tool calls, the extracted intents are
    /// promoted into `result.tool_calls` so they actually execute. Useful as
    /// a signal of either a misbehaving adapter or a prompt-injection attempt
    /// that bypassed the structured tool-use channel.
    ToolIntentLeakedFromText,

    // User notification system (UNIS Phase 1)
    /// Emitted when a UserMessage is created and stored in the inbox.
    NotificationSent,
    /// Emitted when a notification is successfully delivered via a channel adapter.
    NotificationDelivered,
    /// Emitted when a notification is marked read by the user.
    NotificationRead,
    /// Emitted when the user responds to an interactive (Question) notification.
    UserResponseReceived,
    /// Emitted when a notification question times out and the auto_action fires.
    NotificationAutoActioned,

    // Bidirectional channel management (UNIS Phase 6)
    /// Emitted when a user connects a new bidirectional channel (Telegram, ntfy, …).
    ChannelConnected,
    /// Emitted when a user disconnects a bidirectional channel.
    ChannelDisconnected,
    /// Emitted when a message is received from a user via an inbound channel.
    InboundMessageReceived,
    /// Emitted when an agent successfully sends a message to a single connected
    /// channel via the `channel-send` tool. Distinct from `NotificationSent`,
    /// which fans out to all delivery adapters.
    ChannelMessageSent,

    // Agent context memory
    /// Emitted when an agent's context memory is created, updated, rolled back, or cleared.
    ContextMemoryUpdated,

    // MCP (Model Context Protocol)
    /// Emitted when an MCP tool call is executed (client mode).
    McpToolCall,
    /// Emitted when a potential injection attempt is detected in MCP output.
    McpInjectionDetected,

    // OAuth credential lifecycle
    /// Emitted when an OAuth credential is stored or updated in the vault.
    OAuthCredentialStored,
    /// Emitted when an OAuth credential is deleted from the vault.
    OAuthCredentialDeleted,
    /// Emitted when an OAuth access token is successfully refreshed.
    OAuthTokenRefreshed,
    /// Emitted when an OAuth token refresh fails (e.g., invalid_grant).
    OAuthTokenExpired,
    /// Emitted when an OAuth authorization code flow is initiated.
    OAuthFlowStarted,
    /// Emitted when an OAuth authorization code flow completes successfully.
    OAuthFlowCompleted,

    // Container runtime lifecycle
    /// Emitted when a container is provisioned for an agent.
    ContainerProvisioned,
    /// Emitted when a command is executed inside a container.
    ContainerExecRun,
    /// Emitted when a container is destroyed.
    ContainerDestroyed,
    /// Emitted when a container provision is rejected due to quota limits.
    ContainerQuotaExceeded,

    // Device twin & safety engine
    /// Emitted when an agent sets the desired state of an IoT device twin.
    DesiredStateSet,
    /// Emitted when a safety rule blocks an IoT actuator command.
    SafetyRuleViolation,
    /// Emitted when a device's reported state is updated from a sensor.
    ReportedStateUpdated,

    // Kernel-Mediated Capabilities (KMC)
    /// Emitted when an agent requests a managed capability.
    CapabilityRequested,
    /// Emitted when a managed capability is granted (either by policy or approval).
    CapabilityGranted,
    /// Emitted when a managed capability request is denied by policy.
    CapabilityDenied,
    /// Emitted when a managed capability action executes successfully.
    CapabilityExecuted,
    /// Emitted when a managed capability action fails during execution.
    CapabilityFailed,

    // KMC — Managed Environments (env.*)
    /// Emitted when an agent workspace is created.
    EnvironmentCreated,
    /// Emitted when a package is installed into an agent workspace.
    PackageInstalled,
    /// Emitted when a package is removed from an agent workspace.
    PackageRemoved,
    /// Emitted when an agent workspace is destroyed.
    EnvironmentDestroyed,

    // KMC — Managed Processes (proc.*)
    // Prefixed with "Managed" to distinguish from host-process lifecycle events
    // that may be emitted by the HAL process driver or sandbox executor.
    /// Emitted when a managed process is spawned by an agent.
    ManagedProcessSpawned,
    /// Emitted when a signal is sent to a managed process.
    ManagedProcessSignaled,
    /// Emitted when a managed process terminates (normally or by signal).
    ManagedProcessTerminated,

    // KMC — Managed Networking (net.*)
    /// Emitted when a network request is executed through the kernel proxy.
    NetworkRequestExecuted,
    /// Emitted when a network destination is blocked by policy.
    NetworkDestinationBlocked,

    // KMC — Managed Storage (storage.*)
    /// Emitted when a storage zone is created for an agent.
    StorageZoneCreated,
    /// Emitted when a storage zone grant is revoked.
    StorageZoneRevoked,

    // KMC — Managed Builds (build.*)
    /// Emitted when a build command is executed in a managed workspace.
    BuildExecuted,
    /// Emitted when a build command fails.
    BuildFailed,

    // User-preference adaptation proposals
    /// Emitted when the post-task user-adaptation hook inserts a new
    /// preference proposal into the review queue.
    ProposalCreated,
    /// Emitted when an operator accepts a pending preference proposal
    /// (followed by `ContextMemoryUpdated` for the actual write).
    ProposalAccepted,
    /// Emitted when an operator rejects a pending preference proposal.
    ProposalRejected,
    /// Emitted when a pending preference proposal is auto-expired by the
    /// TimeoutChecker sweep after exceeding the TTL.
    ProposalExpired,

    // Proactive personalization — structured user profile
    /// Emitted when an accepted proposal (or explicit edit) inserts a new
    /// structured profile entry into `user_profile.db`.
    ProfileEntryAdded,
    /// Emitted when an existing profile entry is updated (value/confidence/
    /// category/pin/status) or refreshed via promotion of a re-accepted pref.
    ProfileEntryUpdated,
    /// Emitted when a profile entry is forgotten (hard-deleted).
    ProfileEntryRemoved,

    // User filesystem grants
    /// Emitted when an operator grants a host directory to one agent or
    /// globally. Payload: `path`, `agent_id` (or `null` for global), `mode`
    /// ("rwx" string), `source` ("cli"|"web"|"bus"|"config"), `granted_by`.
    WorkspaceGranted,
    /// Emitted when an operator revokes a previously-granted host directory.
    WorkspaceRevoked,

    // Proactive personalization — feedback loop (Phase 5)
    /// A personalization signal raised an interest weight, profile pin_rank,
    /// or restated-preference confidence.
    PersonalizationReinforced,
    /// A personalization signal lowered an interest weight (e.g. dismissal),
    /// or the decay pass reduced a profile pin_rank.
    PersonalizationDecayed,
    /// A stale profile entry was archived out of the L0 pinned block.
    PersonalizationArchived,

    // Proactive personalization — recommendation engine (Phase 4)
    /// Emitted when a proactive tip is generated and persisted to
    /// `recommendations.db` before delivery. Payload: id, topics, confidence.
    RecommendationGenerated,
    /// Emitted when a generated tip is successfully delivered via the
    /// notification router. Payload: id, delivery_channel.
    RecommendationDelivered,
    /// Emitted when a cycle is skipped before persisting anything (rate-limit,
    /// dedup, confidence floor, no signal). Payload: reason, dedup_hash (opt).
    RecommendationSkipped,

    // Proactive personalization — governance (Phase 6)
    /// Emitted when the operator runs `agentos personalization export`.
    /// Payload includes counts per store and the export size (bytes).
    PersonalizationDataExported,
    /// Emitted when the operator runs `agentos personalization forget`.
    /// Payload includes per-store cleared counts and a `partial` flag when
    /// one or more stores could not be fully cleared.
    PersonalizationDataForgotten,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuditSeverity {
    Info,
    Warn,
    Error,
    Security,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub trace_id: TraceID,
    pub event_type: AuditEventType,
    pub agent_id: Option<AgentID>,
    pub task_id: Option<TaskID>,
    pub tool_id: Option<ToolID>,
    pub details: serde_json::Value,
    pub severity: AuditSeverity,
    /// Whether the action that produced this entry is reversible via rollback.
    #[serde(default)]
    pub reversible: bool,
    /// Reference to a snapshot that can be used to roll back this action.
    #[serde(default)]
    pub rollback_ref: Option<String>,
}

/// Result of verifying the Merkle hash chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainVerification {
    pub entries_checked: u64,
    pub valid: bool,
    pub first_invalid_seq: Option<i64>,
    pub error: Option<String>,
}

/// The genesis hash for the first entry in the chain (no predecessor).
const GENESIS_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

struct EntryHashInput<'a> {
    seq: i64,
    prev_hash: &'a str,
    timestamp: &'a str,
    trace_id: &'a str,
    event_type: &'a str,
    agent_id: &'a str,
    task_id: &'a str,
    tool_id: &'a str,
    details: &'a str,
    severity: &'a str,
    reversible: bool,
    rollback_ref: &'a str,
}

/// Maximum allowed size (bytes) for the `details` JSON payload of a single audit entry.
const MAX_DETAILS_BYTES: usize = 64 * 1024; // 64 KiB

impl AuditLog {
    pub fn open(path: &Path) -> Result<Self, AgentOSError> {
        let conn = Connection::open(path)
            .map_err(|e| AgentOSError::VaultError(format!("AuditLog DB open failed: {}", e)))?;

        conn.execute_batch(
            "
            PRAGMA journal_mode=WAL;

            CREATE TABLE IF NOT EXISTS audit_log (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp    TEXT NOT NULL,
                trace_id     TEXT NOT NULL,
                event_type   TEXT NOT NULL,
                agent_id     TEXT,
                task_id      TEXT,
                tool_id      TEXT,
                details      TEXT NOT NULL,
                severity     TEXT NOT NULL DEFAULT 'info',
                reversible   INTEGER NOT NULL DEFAULT 0,
                rollback_ref TEXT,
                prev_hash    TEXT NOT NULL DEFAULT '0000000000000000000000000000000000000000000000000000000000000000',
                entry_hash   TEXT NOT NULL DEFAULT ''
            );

            CREATE INDEX IF NOT EXISTS idx_audit_timestamp ON audit_log(timestamp);
            CREATE INDEX IF NOT EXISTS idx_audit_trace_id ON audit_log(trace_id);
            CREATE INDEX IF NOT EXISTS idx_audit_event_type ON audit_log(event_type);
            CREATE INDEX IF NOT EXISTS idx_audit_agent_id ON audit_log(agent_id);
            CREATE INDEX IF NOT EXISTS idx_audit_task_id ON audit_log(task_id, id);

            CREATE TABLE IF NOT EXISTS health_debounce (
                key             TEXT PRIMARY KEY,
                last_emitted_at TEXT NOT NULL
            );
            ",
        )
        .map_err(|e| AgentOSError::VaultError(format!("AuditLog init failed: {}", e)))?;

        // Migrate existing tables that may be missing columns
        let has_prev_hash: bool = conn
            .prepare("SELECT prev_hash FROM audit_log LIMIT 0")
            .is_ok();
        if !has_prev_hash {
            conn.execute_batch(
                "ALTER TABLE audit_log ADD COLUMN prev_hash TEXT NOT NULL DEFAULT '0000000000000000000000000000000000000000000000000000000000000000';
                 ALTER TABLE audit_log ADD COLUMN entry_hash TEXT NOT NULL DEFAULT '';",
            )
            .map_err(|e| {
                AgentOSError::VaultError(format!("AuditLog migration failed: {}", e))
            })?;
        }
        let has_reversible: bool = conn
            .prepare("SELECT reversible FROM audit_log LIMIT 0")
            .is_ok();
        if !has_reversible {
            conn.execute_batch(
                "ALTER TABLE audit_log ADD COLUMN reversible INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE audit_log ADD COLUMN rollback_ref TEXT;",
            )
            .map_err(|e| {
                AgentOSError::VaultError(format!("AuditLog migration (reversible) failed: {}", e))
            })?;
        }

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Compute SHA-256 hash for an audit entry, creating the Merkle chain link.
    fn compute_entry_hash(input: EntryHashInput<'_>) -> String {
        let mut hasher = Sha256::new();
        hasher.update(input.seq.to_string().as_bytes());
        hasher.update(b"|");
        hasher.update(input.prev_hash.as_bytes());
        hasher.update(b"|");
        hasher.update(input.timestamp.as_bytes());
        hasher.update(b"|");
        hasher.update(input.trace_id.as_bytes());
        hasher.update(b"|");
        hasher.update(input.event_type.as_bytes());
        hasher.update(b"|");
        hasher.update(input.agent_id.as_bytes());
        hasher.update(b"|");
        hasher.update(input.task_id.as_bytes());
        hasher.update(b"|");
        hasher.update(input.tool_id.as_bytes());
        hasher.update(b"|");
        hasher.update(input.details.as_bytes());
        hasher.update(b"|");
        hasher.update(input.severity.as_bytes());
        hasher.update(b"|");
        hasher.update(if input.reversible { b"1" } else { b"0" });
        hasher.update(b"|");
        hasher.update(input.rollback_ref.as_bytes());
        let result = hasher.finalize();
        hex::encode(result)
    }

    pub fn append(&self, entry: AuditEntry) -> Result<(), AgentOSError> {
        let mut conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());

        let details = serde_json::to_string(&entry.details).map_err(|e| {
            AgentOSError::Serialization(format!("AuditEntry serialize failed: {}", e))
        })?;

        if details.len() > MAX_DETAILS_BYTES {
            return Err(AgentOSError::VaultError(format!(
                "Audit entry details too large: {} bytes (max {})",
                details.len(),
                MAX_DETAILS_BYTES
            )));
        }

        let event_type_str = serde_json::to_string(&entry.event_type).map_err(|e| {
            AgentOSError::Serialization(format!("AuditEventType serialize failed: {}", e))
        })?;
        let event_type_str = event_type_str.trim_matches('"');

        let severity_str = serde_json::to_string(&entry.severity).map_err(|e| {
            AgentOSError::Serialization(format!("AuditSeverity serialize failed: {}", e))
        })?;
        let severity_str = severity_str.trim_matches('"');

        let timestamp_str = entry.timestamp.to_rfc3339();
        let trace_id_str = entry.trace_id.to_string();
        let agent_id_str = entry.agent_id.map(|id| id.to_string()).unwrap_or_default();
        let task_id_str = entry.task_id.map(|id| id.to_string()).unwrap_or_default();
        let tool_id_str = entry.tool_id.map(|id| id.to_string());

        // Wrap the read + insert in an explicit transaction to guarantee that
        // the prev_hash and predicted next_seq remain consistent with the actual
        // inserted row. Without this, a rolled-back or interleaved operation could
        // cause the hash chain to reference a sequence number that differs from the
        // SQLite AUTOINCREMENT value.
        let tx = conn
            .transaction()
            .map_err(|e| AgentOSError::VaultError(format!("AuditLog begin txn: {}", e)))?;

        // Get the previous entry's hash (or genesis hash if this is the first entry)
        let prev_hash: String = tx
            .query_row(
                "SELECT entry_hash FROM audit_log ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap_or_else(|_| GENESIS_HASH.to_string());

        // Use the next sequence number
        let next_seq: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(id), 0) + 1 FROM audit_log",
                [],
                |row| row.get(0),
            )
            .map_err(|e| AgentOSError::VaultError(format!("Failed to get next seq: {}", e)))?;

        let tool_id_for_hash = tool_id_str.clone().unwrap_or_default();
        let rollback_ref_str = entry.rollback_ref.clone().unwrap_or_default();
        let entry_hash = Self::compute_entry_hash(EntryHashInput {
            seq: next_seq,
            prev_hash: &prev_hash,
            timestamp: &timestamp_str,
            trace_id: &trace_id_str,
            event_type: event_type_str,
            agent_id: &agent_id_str,
            task_id: &task_id_str,
            tool_id: &tool_id_for_hash,
            details: &details,
            severity: severity_str,
            reversible: entry.reversible,
            rollback_ref: &rollback_ref_str,
        });

        tx.execute(
            "INSERT INTO audit_log (timestamp, trace_id, event_type, agent_id, task_id, tool_id, details, severity, reversible, rollback_ref, prev_hash, entry_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                timestamp_str,
                trace_id_str,
                event_type_str,
                if agent_id_str.is_empty() { None } else { Some(&agent_id_str) },
                if task_id_str.is_empty() { None } else { Some(&task_id_str) },
                tool_id_str,
                details,
                severity_str,
                entry.reversible as i32,
                entry.rollback_ref,
                prev_hash,
                entry_hash,
            ],
        )
        .map_err(|e| AgentOSError::VaultError(format!("AuditLog append failed: {}", e)))?;

        tx.commit()
            .map_err(|e| AgentOSError::VaultError(format!("AuditLog commit failed: {}", e)))?;

        Ok(())
    }

    /// Return the sequence id (inclusive lower bound) such that at most `n` entries
    /// with `id >= returned_id` exist in the log. Used by the kernel to compute the
    /// starting point for incremental chain verification at boot.
    ///
    /// Returns `None` when the log has fewer than `n` entries (verify the full chain)
    /// or when `n == 0` (caller requested a full-chain verify).
    pub fn seq_for_last_n_entries(&self, n: u64) -> Result<Option<i64>, AgentOSError> {
        if n == 0 {
            return Ok(None);
        }
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        // OFFSET n-1 gives the Nth-from-last row; if fewer than n rows exist,
        // QueryReturnedNoRows is returned and we fall back to a full-chain verify.
        match conn.query_row(
            "SELECT id FROM audit_log ORDER BY id DESC LIMIT 1 OFFSET ?1",
            rusqlite::params![n - 1],
            |row| row.get::<_, i64>(0),
        ) {
            Ok(id) => Ok(Some(id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(AgentOSError::VaultError(format!(
                "Failed to compute last-N seq start: {}",
                e
            ))),
        }
    }

    /// Verify the integrity of the Merkle hash chain.
    pub fn verify_chain(&self, from_seq: Option<i64>) -> Result<ChainVerification, AgentOSError> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let from = from_seq.unwrap_or(1);

        let mut stmt = conn
            .prepare(
                "SELECT id, timestamp, trace_id, event_type, \
                 COALESCE(agent_id, ''), COALESCE(task_id, ''), \
                 COALESCE(tool_id, ''), \
                 details, severity, COALESCE(reversible, 0), COALESCE(rollback_ref, ''), \
                 prev_hash, entry_hash \
                 FROM audit_log WHERE id >= ?1 ORDER BY id ASC",
            )
            .map_err(|e| AgentOSError::VaultError(e.to_string()))?;

        let rows = stmt
            .query_map(params![from], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, i32>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, String>(12)?,
                ))
            })
            .map_err(|e| AgentOSError::VaultError(e.to_string()))?;

        let mut entries_checked: u64 = 0;
        let mut expected_prev_hash: Option<String> = None;

        // If starting from a non-genesis position, get the nearest preceding entry's hash.
        // Using `WHERE id < from ORDER BY id DESC LIMIT 1` rather than `WHERE id = from - 1`
        // so this works correctly when ids have gaps (e.g. after prune_old_entries removes
        // the oldest rows and the pruned id is immediately before `from`).
        if from > 1 {
            expected_prev_hash = match conn.query_row(
                "SELECT entry_hash FROM audit_log WHERE id < ?1 ORDER BY id DESC LIMIT 1",
                params![from],
                |row| row.get::<_, String>(0),
            ) {
                Ok(hash) => Some(hash),
                // Predecessor row doesn't exist (pruned or empty window start) — skip
                // the chain-linkage check for the first entry in the verified window.
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(e) => {
                    return Err(AgentOSError::VaultError(format!(
                        "Cannot verify audit chain: failed to retrieve predecessor hash before id {}: {}",
                        from, e
                    )));
                }
            };
        }

        for row_result in rows {
            let (
                seq,
                timestamp,
                trace_id,
                event_type,
                agent_id,
                task_id,
                tool_id,
                details,
                severity,
                reversible_int,
                rollback_ref,
                prev_hash,
                stored_hash,
            ) = row_result.map_err(|e| AgentOSError::VaultError(e.to_string()))?;

            entries_checked += 1;

            // Check prev_hash linkage
            if let Some(ref expected) = expected_prev_hash {
                if prev_hash != *expected {
                    return Ok(ChainVerification {
                        entries_checked,
                        valid: false,
                        first_invalid_seq: Some(seq),
                        error: Some(format!(
                            "prev_hash mismatch at seq {}: expected {}, got {}",
                            seq, expected, prev_hash
                        )),
                    });
                }
            }

            // Recompute and verify entry_hash
            let recomputed = Self::compute_entry_hash(EntryHashInput {
                seq,
                prev_hash: &prev_hash,
                timestamp: &timestamp,
                trace_id: &trace_id,
                event_type: &event_type,
                agent_id: &agent_id,
                task_id: &task_id,
                tool_id: &tool_id,
                details: &details,
                severity: &severity,
                reversible: reversible_int != 0,
                rollback_ref: &rollback_ref,
            });

            if recomputed != stored_hash {
                return Ok(ChainVerification {
                    entries_checked,
                    valid: false,
                    first_invalid_seq: Some(seq),
                    error: Some(format!(
                        "entry_hash mismatch at seq {}: recomputed {}, stored {}",
                        seq, recomputed, stored_hash
                    )),
                });
            }

            expected_prev_hash = Some(stored_hash);
        }

        Ok(ChainVerification {
            entries_checked,
            valid: true,
            first_invalid_seq: None,
            error: None,
        })
    }

    pub fn query_recent(&self, limit: u32) -> Result<Vec<AuditEntry>, AgentOSError> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare("SELECT timestamp, trace_id, event_type, agent_id, task_id, tool_id, details, severity, reversible, rollback_ref FROM audit_log ORDER BY id DESC LIMIT ?1")
            .map_err(|e| AgentOSError::VaultError(e.to_string()))?;

        Self::execute_query(&mut stmt, params![limit])
    }

    /// Query audit entries for a specific task inserted after the given row ID.
    /// Returns entries ordered by row ID ascending (oldest first), limited to `limit` rows.
    /// Each entry is paired with its SQLite row ID for monotonic tracking.
    pub fn query_since_for_task(
        &self,
        task_id: &TaskID,
        after_row_id: i64,
        limit: u32,
    ) -> Result<Vec<(i64, AuditEntry)>, AgentOSError> {
        fn parse_err(msg: String) -> rusqlite::Error {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, msg)),
            )
        }

        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn
            .prepare(
                "SELECT id, timestamp, trace_id, event_type, agent_id, task_id, \
                 tool_id, details, severity, reversible, rollback_ref \
                 FROM audit_log \
                 WHERE task_id = ?1 AND id > ?2 \
                 ORDER BY id ASC \
                 LIMIT ?3",
            )
            .map_err(|e| AgentOSError::VaultError(e.to_string()))?;

        let task_id_str = task_id.to_string();
        let rows = stmt
            .query_map(rusqlite::params![task_id_str, after_row_id, limit], |row| {
                let row_id: i64 = row.get(0)?;

                let timestamp_str: String = row.get(1)?;
                let timestamp = chrono::DateTime::parse_from_rfc3339(&timestamp_str)
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .map_err(|e| parse_err(format!("Invalid timestamp: {}", e)))?;

                let trace_id_str: String = row.get(2)?;
                let trace_id_uuid = uuid::Uuid::parse_str(&trace_id_str)
                    .map_err(|e| parse_err(format!("Invalid trace_id UUID: {}", e)))?;
                let trace_id = TraceID::from_uuid(trace_id_uuid);

                let event_type_str: String = row.get(3)?;
                let event_type: AuditEventType =
                    serde_json::from_value(serde_json::Value::String(event_type_str.clone()))
                        .map_err(|e| {
                            parse_err(format!("Invalid event_type '{}': {}", event_type_str, e))
                        })?;

                let agent_id_str: Option<String> = row.get(4)?;
                let agent_id = agent_id_str
                    .map(|s| {
                        uuid::Uuid::parse_str(&s)
                            .map(AgentID::from_uuid)
                            .map_err(|e| parse_err(format!("Invalid agent_id UUID: {}", e)))
                    })
                    .transpose()?;

                let task_id_col_str: Option<String> = row.get(5)?;
                let task_id_col = task_id_col_str
                    .map(|s| {
                        uuid::Uuid::parse_str(&s)
                            .map(TaskID::from_uuid)
                            .map_err(|e| parse_err(format!("Invalid task_id UUID: {}", e)))
                    })
                    .transpose()?;

                let tool_id_str: Option<String> = row.get(6)?;
                let tool_id = tool_id_str
                    .map(|s| {
                        uuid::Uuid::parse_str(&s)
                            .map(ToolID::from_uuid)
                            .map_err(|e| parse_err(format!("Invalid tool_id UUID: {}", e)))
                    })
                    .transpose()?;

                let details_str: String = row.get(7)?;
                let details: serde_json::Value = serde_json::from_str(&details_str)
                    .map_err(|e| parse_err(format!("Invalid details JSON: {}", e)))?;

                let severity_str: String = row.get(8)?;
                let severity: AuditSeverity =
                    serde_json::from_value(serde_json::Value::String(severity_str.clone()))
                        .map_err(|e| {
                            parse_err(format!("Invalid severity '{}': {}", severity_str, e))
                        })?;

                let reversible_int: i32 = row.get(9).unwrap_or(0);
                let rollback_ref: Option<String> = row.get(10).unwrap_or(None);

                Ok((
                    row_id,
                    AuditEntry {
                        timestamp,
                        trace_id,
                        event_type,
                        agent_id,
                        task_id: task_id_col,
                        tool_id,
                        details,
                        severity,
                        reversible: reversible_int != 0,
                        rollback_ref,
                    },
                ))
            })
            .map_err(|e| AgentOSError::VaultError(e.to_string()))?;

        let mut entries = Vec::new();
        for row_result in rows {
            entries.push(row_result.map_err(|e| AgentOSError::VaultError(e.to_string()))?);
        }
        Ok(entries)
    }

    pub fn query_recent_for_agent(
        &self,
        agent_id: &AgentID,
        limit: u32,
    ) -> Result<Vec<AuditEntry>, AgentOSError> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn
            .prepare("SELECT timestamp, trace_id, event_type, agent_id, task_id, tool_id, details, severity, reversible, rollback_ref FROM audit_log WHERE agent_id = ?1 ORDER BY id DESC LIMIT ?2")
            .map_err(|e| AgentOSError::VaultError(e.to_string()))?;

        Self::execute_query(&mut stmt, params![agent_id.to_string(), limit])
    }

    pub fn query_by_trace(&self, trace_id: &TraceID) -> Result<Vec<AuditEntry>, AgentOSError> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare("SELECT timestamp, trace_id, event_type, agent_id, task_id, tool_id, details, severity, reversible, rollback_ref FROM audit_log WHERE trace_id = ?1 ORDER BY id ASC")
            .map_err(|e| AgentOSError::VaultError(e.to_string()))?;

        Self::execute_query(&mut stmt, params![trace_id.to_string()])
    }

    pub fn query_by_type(
        &self,
        event_type: AuditEventType,
        limit: u32,
    ) -> Result<Vec<AuditEntry>, AgentOSError> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());

        let event_type_str = serde_json::to_string(&event_type).map_err(|e| {
            AgentOSError::Serialization(format!("AuditEventType serialize failed: {}", e))
        })?;
        let event_type_str = event_type_str.trim_matches('"');

        let mut stmt = conn.prepare("SELECT timestamp, trace_id, event_type, agent_id, task_id, tool_id, details, severity, reversible, rollback_ref FROM audit_log WHERE event_type = ?1 ORDER BY id DESC LIMIT ?2")
            .map_err(|e| AgentOSError::VaultError(e.to_string()))?;

        Self::execute_query(&mut stmt, params![event_type_str, limit])
    }

    pub fn query_by_time_range(
        &self,
        from: chrono::DateTime<chrono::Utc>,
        to: chrono::DateTime<chrono::Utc>,
        limit: u32,
    ) -> Result<Vec<AuditEntry>, AgentOSError> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare("SELECT timestamp, trace_id, event_type, agent_id, task_id, tool_id, details, severity, reversible, rollback_ref FROM audit_log WHERE timestamp >= ?1 AND timestamp <= ?2 ORDER BY id DESC LIMIT ?3")
            .map_err(|e| AgentOSError::VaultError(e.to_string()))?;

        Self::execute_query(
            &mut stmt,
            params![from.to_rfc3339(), to.to_rfc3339(), limit],
        )
    }

    /// Export the audit chain as JSONL (one JSON object per line).
    /// Each entry includes its sequence, hashes, and all fields.
    pub fn export_chain_json(&self, limit: Option<u32>) -> Result<String, AgentOSError> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let limit_val = limit.unwrap_or(100_000);
        let mut stmt = conn
            .prepare(
                "SELECT id, timestamp, trace_id, event_type, \
                 COALESCE(agent_id, ''), COALESCE(task_id, ''), \
                 COALESCE(tool_id, ''), details, severity, \
                 COALESCE(reversible, 0), COALESCE(rollback_ref, ''), \
                 prev_hash, entry_hash \
                 FROM audit_log ORDER BY id ASC LIMIT ?1",
            )
            .map_err(|e| AgentOSError::VaultError(format!("Export prepare error: {}", e)))?;

        let rows = stmt
            .query_map(params![limit_val], |row| {
                let details_str: String = row.get(7)?;
                // details is stored as JSON text; parse it back so it embeds as a
                // proper nested object rather than a double-encoded string.
                // If corrupt, preserve the raw string rather than silently discarding.
                let details_val: serde_json::Value = serde_json::from_str(&details_str)
                    .unwrap_or_else(|_| serde_json::Value::String(details_str.clone()));
                Ok(serde_json::json!({
                    "seq": row.get::<_, i64>(0)?,
                    "timestamp": row.get::<_, String>(1)?,
                    "trace_id": row.get::<_, String>(2)?,
                    "event_type": row.get::<_, String>(3)?,
                    "agent_id": row.get::<_, String>(4)?,
                    "task_id": row.get::<_, String>(5)?,
                    "tool_id": row.get::<_, String>(6)?,
                    "details": details_val,
                    "severity": row.get::<_, String>(8)?,
                    "reversible": row.get::<_, i32>(9)? != 0,
                    "rollback_ref": row.get::<_, String>(10)?,
                    "prev_hash": row.get::<_, String>(11)?,
                    "entry_hash": row.get::<_, String>(12)?,
                }))
            })
            .map_err(|e| AgentOSError::VaultError(format!("Export query error: {}", e)))?;

        let mut output = String::new();
        for row_result in rows {
            let value = row_result
                .map_err(|e| AgentOSError::VaultError(format!("Export row error: {}", e)))?;
            output.push_str(
                &serde_json::to_string(&value)
                    .map_err(|e| AgentOSError::Serialization(e.to_string()))?,
            );
            output.push('\n');
        }
        Ok(output)
    }

    pub fn count(&self) -> Result<u64, AgentOSError> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let count: u64 = conn
            .query_row("SELECT COUNT(*) FROM audit_log", [], |row| row.get(0))
            .map_err(|e| AgentOSError::VaultError(e.to_string()))?;

        Ok(count)
    }

    /// Prune the oldest entries so the total row count does not exceed `max_entries`.
    ///
    /// Deletes the `N` oldest rows (by ascending `id`) where `N = count - max_entries`.
    /// Returns the number of rows deleted. No-op if the count is already within limit.
    ///
    /// **Note:** `max_entries = 0` means unlimited — no rows are deleted. Use a positive
    /// value to enforce a cap. The `[run_loop]` TimeoutChecker guards this call with
    /// `if max_audit_entries > 0` for the same reason.
    ///
    /// **Chain integrity:** Pruning breaks the Merkle chain link before the oldest
    /// surviving entry. `verify_chain()` is only valid on the retained portion.
    pub fn prune_old_entries(&self, max_entries: u64) -> Result<u64, AgentOSError> {
        if max_entries == 0 {
            return Ok(0); // 0 = unlimited; caller should not prune
        }

        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());

        let current_count: u64 = conn
            .query_row("SELECT COUNT(*) FROM audit_log", [], |row| row.get(0))
            .map_err(|e| AgentOSError::VaultError(format!("prune count query failed: {}", e)))?;

        if current_count <= max_entries {
            return Ok(0);
        }

        let to_delete = current_count - max_entries;
        let deleted = conn
            .execute(
                "DELETE FROM audit_log WHERE id IN (
                     SELECT id FROM audit_log ORDER BY id ASC LIMIT ?1
                 )",
                rusqlite::params![to_delete],
            )
            .map_err(|e| AgentOSError::VaultError(format!("prune delete failed: {}", e)))?;

        Ok(deleted as u64)
    }

    /// Load all persisted health-monitor debounce timestamps.
    ///
    /// Prunes entries older than 24 h before loading (well past the 10-minute
    /// debounce window). Returns `(map, skipped_keys)` where `skipped_keys`
    /// lists any keys whose stored timestamp could not be parsed.
    pub fn load_health_debounce(&self) -> Result<(HealthDebounceMap, Vec<String>), AgentOSError> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());

        // Prune rows that are older than the debounce window — they can never
        // suppress a future emission and would accumulate forever for transient
        // devices (USB, VM network interfaces, sensors).
        let cutoff = (chrono::Utc::now() - chrono::Duration::hours(24)).to_rfc3339();
        conn.execute(
            "DELETE FROM health_debounce WHERE last_emitted_at < ?1",
            params![cutoff],
        )
        .map_err(|e| AgentOSError::VaultError(format!("health_debounce prune failed: {}", e)))?;

        let mut stmt = conn
            .prepare("SELECT key, last_emitted_at FROM health_debounce")
            .map_err(|e| AgentOSError::VaultError(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| AgentOSError::VaultError(e.to_string()))?;
        let mut map = std::collections::HashMap::new();
        let mut skipped = Vec::new();
        for row in rows {
            let (key, ts_str) = row.map_err(|e| AgentOSError::VaultError(e.to_string()))?;
            match chrono::DateTime::parse_from_rfc3339(&ts_str) {
                Ok(ts) => {
                    map.insert(key, ts.with_timezone(&chrono::Utc));
                }
                Err(_) => skipped.push(key),
            }
        }
        Ok((map, skipped))
    }

    /// Upsert a health-monitor debounce timestamp.
    pub fn save_health_debounce(
        &self,
        key: &str,
        last_emitted_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), AgentOSError> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "INSERT INTO health_debounce (key, last_emitted_at) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET last_emitted_at = excluded.last_emitted_at",
            params![key, last_emitted_at.to_rfc3339()],
        )
        .map_err(|e| AgentOSError::VaultError(format!("save_health_debounce failed: {}", e)))?;
        Ok(())
    }

    fn execute_query<P>(
        stmt: &mut rusqlite::Statement,
        params: P,
    ) -> Result<Vec<AuditEntry>, AgentOSError>
    where
        P: rusqlite::Params,
    {
        /// Helper to convert a parse error into a rusqlite error for use inside query_map closures.
        fn parse_err(msg: String) -> rusqlite::Error {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, msg)),
            )
        }

        let rows = stmt
            .query_map(params, |row| {
                let timestamp_str: String = row.get(0)?;
                let timestamp = chrono::DateTime::parse_from_rfc3339(&timestamp_str)
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .map_err(|e| parse_err(format!("Invalid timestamp: {}", e)))?;

                let trace_id_str: String = row.get(1)?;
                let trace_id_uuid = uuid::Uuid::parse_str(&trace_id_str)
                    .map_err(|e| parse_err(format!("Invalid trace_id UUID: {}", e)))?;
                let trace_id = TraceID::from_uuid(trace_id_uuid);

                let event_type_str: String = row.get(2)?;
                let event_type: AuditEventType =
                    serde_json::from_value(serde_json::Value::String(event_type_str.clone()))
                        .map_err(|e| {
                            parse_err(format!("Invalid event_type '{}': {}", event_type_str, e))
                        })?;

                let agent_id_str: Option<String> = row.get(3)?;
                let agent_id = agent_id_str
                    .map(|s| {
                        uuid::Uuid::parse_str(&s)
                            .map(AgentID::from_uuid)
                            .map_err(|e| parse_err(format!("Invalid agent_id UUID: {}", e)))
                    })
                    .transpose()?;

                let task_id_str: Option<String> = row.get(4)?;
                let task_id = task_id_str
                    .map(|s| {
                        uuid::Uuid::parse_str(&s)
                            .map(TaskID::from_uuid)
                            .map_err(|e| parse_err(format!("Invalid task_id UUID: {}", e)))
                    })
                    .transpose()?;

                let tool_id_str: Option<String> = row.get(5)?;
                let tool_id = tool_id_str
                    .map(|s| {
                        uuid::Uuid::parse_str(&s)
                            .map(ToolID::from_uuid)
                            .map_err(|e| parse_err(format!("Invalid tool_id UUID: {}", e)))
                    })
                    .transpose()?;

                let details_str: String = row.get(6)?;
                let details: serde_json::Value = serde_json::from_str(&details_str)
                    .map_err(|e| parse_err(format!("Invalid details JSON: {}", e)))?;

                let severity_str: String = row.get(7)?;
                let severity: AuditSeverity =
                    serde_json::from_value(serde_json::Value::String(severity_str.clone()))
                        .map_err(|e| {
                            parse_err(format!("Invalid severity '{}': {}", severity_str, e))
                        })?;

                let reversible_int: i32 = row.get(8).unwrap_or(0);
                let rollback_ref: Option<String> = row.get(9).unwrap_or(None);

                Ok(AuditEntry {
                    timestamp,
                    trace_id,
                    event_type,
                    agent_id,
                    task_id,
                    tool_id,
                    details,
                    severity,
                    reversible: reversible_int != 0,
                    rollback_ref,
                })
            })
            .map_err(|e| AgentOSError::VaultError(e.to_string()))?;

        let mut entries = Vec::new();
        for row_result in rows {
            entries.push(row_result.map_err(|e| AgentOSError::VaultError(e.to_string()))?);
        }

        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_append_and_query() {
        let tmp = NamedTempFile::new().unwrap();
        let log = AuditLog::open(tmp.path()).unwrap();

        let entry = AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
            event_type: AuditEventType::TaskCreated,
            agent_id: Some(AgentID::new()),
            task_id: Some(TaskID::new()),
            tool_id: None,
            details: serde_json::json!({"prompt": "Summarize logs"}),
            severity: AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        };

        log.append(entry.clone()).unwrap();
        let results = log.query_recent(10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].event_type, AuditEventType::TaskCreated);
    }

    #[test]
    fn test_query_by_trace() {
        let tmp = NamedTempFile::new().unwrap();
        let log = AuditLog::open(tmp.path()).unwrap();
        let trace = TraceID::new();

        for event in [
            AuditEventType::IntentReceived,
            AuditEventType::ToolExecutionStarted,
            AuditEventType::IntentCompleted,
        ] {
            log.append(AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: trace,
                event_type: event,
                agent_id: None,
                task_id: None,
                tool_id: None,
                details: serde_json::json!({}),
                severity: AuditSeverity::Info,
                reversible: false,
                rollback_ref: None,
            })
            .unwrap();
        }

        let results = log.query_by_trace(&trace).unwrap();
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn test_merkle_chain_valid() {
        let tmp = NamedTempFile::new().unwrap();
        let log = AuditLog::open(tmp.path()).unwrap();

        for i in 0..5 {
            log.append(AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: TraceID::new(),
                event_type: AuditEventType::TaskCreated,
                agent_id: Some(AgentID::new()),
                task_id: None,
                tool_id: None,
                details: serde_json::json!({"step": i}),
                severity: AuditSeverity::Info,
                reversible: false,
                rollback_ref: None,
            })
            .unwrap();
        }

        let verification = log.verify_chain(None).unwrap();
        assert!(verification.valid);
        assert_eq!(verification.entries_checked, 5);
    }

    #[test]
    fn test_merkle_chain_tamper_detected() {
        let tmp = NamedTempFile::new().unwrap();
        let log = AuditLog::open(tmp.path()).unwrap();

        for i in 0..3 {
            log.append(AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: TraceID::new(),
                event_type: AuditEventType::TaskCreated,
                agent_id: None,
                task_id: None,
                tool_id: None,
                details: serde_json::json!({"step": i}),
                severity: AuditSeverity::Info,
                reversible: false,
                rollback_ref: None,
            })
            .unwrap();
        }

        // Tamper with the second entry's details
        {
            let conn = log.conn.lock().unwrap();
            conn.execute(
                "UPDATE audit_log SET details = '{\"step\": 999}' WHERE id = 2",
                [],
            )
            .unwrap();
        }

        let verification = log.verify_chain(None).unwrap();
        assert!(!verification.valid);
        assert_eq!(verification.first_invalid_seq, Some(2));
        assert!(verification.error.unwrap().contains("entry_hash mismatch"));
    }

    #[test]
    fn test_merkle_chain_link_tamper_detected() {
        let tmp = NamedTempFile::new().unwrap();
        let log = AuditLog::open(tmp.path()).unwrap();

        for i in 0..3 {
            log.append(AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: TraceID::new(),
                event_type: AuditEventType::TaskCreated,
                agent_id: None,
                task_id: None,
                tool_id: None,
                details: serde_json::json!({"step": i}),
                severity: AuditSeverity::Info,
                reversible: false,
                rollback_ref: None,
            })
            .unwrap();
        }

        // Tamper with the prev_hash link in entry 3
        {
            let conn = log.conn.lock().unwrap();
            conn.execute("UPDATE audit_log SET prev_hash = 'aaaa' WHERE id = 3", [])
                .unwrap();
        }

        let verification = log.verify_chain(None).unwrap();
        assert!(!verification.valid);
        assert_eq!(verification.first_invalid_seq, Some(3));
    }

    #[test]
    fn test_verify_partial_chain() {
        let tmp = NamedTempFile::new().unwrap();
        let log = AuditLog::open(tmp.path()).unwrap();

        for i in 0..5 {
            log.append(AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: TraceID::new(),
                event_type: AuditEventType::TaskCreated,
                agent_id: None,
                task_id: None,
                tool_id: None,
                details: serde_json::json!({"step": i}),
                severity: AuditSeverity::Info,
                reversible: false,
                rollback_ref: None,
            })
            .unwrap();
        }

        let verification = log.verify_chain(Some(3)).unwrap();
        assert!(verification.valid);
        assert_eq!(verification.entries_checked, 3);
    }

    #[test]
    fn test_query_recent_for_agent_filters_only_target_agent() {
        let tmp = NamedTempFile::new().unwrap();
        let log = AuditLog::open(tmp.path()).unwrap();
        let target_agent = AgentID::new();
        let other_agent = AgentID::new();

        log.append(AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
            event_type: AuditEventType::TaskCreated,
            agent_id: Some(other_agent),
            task_id: None,
            tool_id: None,
            details: serde_json::json!({"scope": "other"}),
            severity: AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        })
        .unwrap();

        log.append(AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
            event_type: AuditEventType::TaskCompleted,
            agent_id: Some(target_agent),
            task_id: None,
            tool_id: None,
            details: serde_json::json!({"scope": "target"}),
            severity: AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        })
        .unwrap();

        let results = log.query_recent_for_agent(&target_agent, 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].agent_id, Some(target_agent));
        assert_eq!(results[0].event_type, AuditEventType::TaskCompleted);
    }

    #[test]
    fn test_query_recent_for_agent_respects_limit_and_order() {
        let tmp = NamedTempFile::new().unwrap();
        let log = AuditLog::open(tmp.path()).unwrap();
        let target_agent = AgentID::new();

        for idx in 0..3 {
            log.append(AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: TraceID::new(),
                event_type: AuditEventType::TaskCreated,
                agent_id: Some(target_agent),
                task_id: None,
                tool_id: None,
                details: serde_json::json!({"idx": idx}),
                severity: AuditSeverity::Info,
                reversible: false,
                rollback_ref: None,
            })
            .unwrap();
        }

        let results = log.query_recent_for_agent(&target_agent, 1).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].details["idx"], serde_json::json!(2));
    }

    #[test]
    fn test_query_recent_for_agent_empty_when_no_matches() {
        let tmp = NamedTempFile::new().unwrap();
        let log = AuditLog::open(tmp.path()).unwrap();
        let target_agent = AgentID::new();
        let other_agent = AgentID::new();

        log.append(AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
            event_type: AuditEventType::TaskCreated,
            agent_id: Some(other_agent),
            task_id: None,
            tool_id: None,
            details: serde_json::json!({"scope": "other"}),
            severity: AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        })
        .unwrap();

        let results = log.query_recent_for_agent(&target_agent, 5).unwrap();
        assert!(results.is_empty());
    }

    fn make_entry(task_id: Option<TaskID>, idx: usize) -> AuditEntry {
        AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
            event_type: AuditEventType::TaskCreated,
            agent_id: None,
            task_id,
            tool_id: None,
            details: serde_json::json!({ "idx": idx }),
            severity: AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        }
    }

    #[test]
    fn test_query_since_for_task_filters_by_task_id() {
        let tmp = NamedTempFile::new().unwrap();
        let log = AuditLog::open(tmp.path()).unwrap();
        let task_a = TaskID::new();
        let task_b = TaskID::new();

        for i in 0..10 {
            log.append(make_entry(Some(task_a), i)).unwrap();
        }
        for i in 0..5 {
            log.append(make_entry(Some(task_b), i)).unwrap();
        }

        let results = log.query_since_for_task(&task_a, 0, 100).unwrap();
        assert_eq!(results.len(), 10);
        assert!(results.iter().all(|(_, e)| e.task_id == Some(task_a)));
    }

    #[test]
    fn test_query_since_for_task_pagination() {
        let tmp = NamedTempFile::new().unwrap();
        let log = AuditLog::open(tmp.path()).unwrap();
        let task = TaskID::new();

        for i in 0..5 {
            log.append(make_entry(Some(task), i)).unwrap();
        }

        let first = log.query_since_for_task(&task, 0, 100).unwrap();
        assert_eq!(first.len(), 5);

        let max_id = first.last().unwrap().0;
        let second = log.query_since_for_task(&task, max_id, 100).unwrap();
        assert!(second.is_empty(), "no new entries after max_id");

        // Add 2 more entries and verify only those are returned.
        log.append(make_entry(Some(task), 5)).unwrap();
        log.append(make_entry(Some(task), 6)).unwrap();

        let third = log.query_since_for_task(&task, max_id, 100).unwrap();
        assert_eq!(third.len(), 2);
    }

    #[test]
    fn test_query_since_for_task_ids_are_monotonically_increasing() {
        let tmp = NamedTempFile::new().unwrap();
        let log = AuditLog::open(tmp.path()).unwrap();
        let task = TaskID::new();

        for i in 0..5 {
            log.append(make_entry(Some(task), i)).unwrap();
        }

        let results = log.query_since_for_task(&task, 0, 100).unwrap();
        assert_eq!(results.len(), 5);
        let ids: Vec<i64> = results.iter().map(|(id, _)| *id).collect();
        for window in ids.windows(2) {
            assert!(window[0] < window[1], "IDs must be strictly increasing");
        }
    }

    #[test]
    fn test_query_since_for_task_empty_when_no_entries() {
        let tmp = NamedTempFile::new().unwrap();
        let log = AuditLog::open(tmp.path()).unwrap();
        let task = TaskID::new();
        let other = TaskID::new();

        for i in 0..3 {
            log.append(make_entry(Some(other), i)).unwrap();
        }

        let results = log.query_since_for_task(&task, 0, 100).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_query_since_for_task_respects_limit() {
        let tmp = NamedTempFile::new().unwrap();
        let log = AuditLog::open(tmp.path()).unwrap();
        let task = TaskID::new();

        for i in 0..10 {
            log.append(make_entry(Some(task), i)).unwrap();
        }

        let results = log.query_since_for_task(&task, 0, 3).unwrap();
        assert_eq!(results.len(), 3);
        // Should return the first 3 entries (ascending order).
        assert_eq!(results[0].1.details["idx"], serde_json::json!(0));
        assert_eq!(results[2].1.details["idx"], serde_json::json!(2));
    }
}
