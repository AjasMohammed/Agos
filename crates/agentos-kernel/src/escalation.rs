use crate::kernel_action::EscalationReason;
use crate::state_store::KernelStateStore;
use agentos_types::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// What should happen automatically when an escalation expires without human resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AutoAction {
    /// Default: deny the action (existing behavior).
    Deny,
    /// Soft-approval: approve the action automatically if no human intervenes.
    Approve,
}

/// Default escalation timeout in seconds (5 minutes per Spec §12).
const DEFAULT_ESCALATION_TIMEOUT_SECS: i64 = 300;

/// A pending escalation awaiting human review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingEscalation {
    pub id: u64,
    pub task_id: TaskID,
    pub agent_id: AgentID,
    pub reason: EscalationReason,
    pub context_summary: String,
    pub decision_point: String,
    pub options: Vec<String>,
    pub urgency: String,
    pub blocking: bool,
    pub trace_id: TraceID,
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Escalation expires and auto-denies after this time (Spec §12).
    pub expires_at: chrono::DateTime<chrono::Utc>,
    /// What happens automatically on expiry: Deny (default) or Approve (soft-approval).
    #[serde(default = "default_auto_action")]
    pub auto_action: AutoAction,
    /// Optional structured metadata used by specialized workflows such as HAL approvals.
    #[serde(default = "default_metadata")]
    pub metadata: serde_json::Value,
    pub resolved: bool,
    pub resolution: Option<String>,
    pub resolved_at: Option<chrono::DateTime<chrono::Utc>>,
}

fn default_auto_action() -> AutoAction {
    AutoAction::Deny
}

fn default_metadata() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

/// Manages escalation requests from agents to human operators.
///
/// Stores pending escalations in memory (optionally backed by SQLite persistence).
/// Provides list/resolve operations for the CLI (`agentctl escalation list/resolve`).
///
/// Escalations auto-deny after `DEFAULT_ESCALATION_TIMEOUT_SECS` (5 minutes)
/// if not resolved by a human operator (Spec §12: "Auto-action on expiry: deny").
pub struct EscalationManager {
    escalations: RwLock<Vec<PendingEscalation>>,
    next_id: RwLock<u64>,
    /// Configurable timeout in seconds. Defaults to 300 (5 minutes).
    timeout_secs: i64,
    /// Optional webhook URL: receives HTTP POST on escalation creation.
    notify_url: RwLock<Option<String>>,
    /// Optional persistence backend for escalation durability across restarts.
    state_store: Option<Arc<KernelStateStore>>,
}

impl EscalationManager {
    pub fn new() -> Self {
        Self::with_state_store(None)
    }

    pub fn with_state_store(state_store: Option<Arc<KernelStateStore>>) -> Self {
        Self {
            escalations: RwLock::new(Vec::new()),
            next_id: RwLock::new(1),
            timeout_secs: DEFAULT_ESCALATION_TIMEOUT_SECS,
            notify_url: RwLock::new(None),
            state_store,
        }
    }

    async fn persist_escalation(&self, escalation: PendingEscalation) {
        let escalation_id = escalation.id;
        if let Some(store) = &self.state_store {
            if let Err(e) = store.upsert_escalation(escalation).await {
                tracing::error!(
                    escalation_id,
                    error = %e,
                    "Failed to persist escalation state"
                );
            }
        }
    }

    /// Restore unresolved escalations from SQLite at boot.
    pub async fn restore_from_store(&self) -> anyhow::Result<usize> {
        let Some(store) = &self.state_store else {
            return Ok(0);
        };

        let unresolved = store.load_unresolved_escalations().await?;
        let restored = unresolved.len();

        let mut next_id = store.next_escalation_id().await?;
        if let Some(max_loaded) = unresolved.iter().map(|e| e.id).max() {
            next_id = next_id.max(max_loaded.saturating_add(1));
        }
        if next_id == 0 {
            next_id = 1;
        }

        *self.escalations.write().await = unresolved;
        *self.next_id.write().await = next_id;

        Ok(restored)
    }

    /// Set a webhook URL that receives HTTP POST notifications on escalation creation.
    pub async fn set_notify_url(&self, url: Option<String>) {
        *self.notify_url.write().await = url;
    }

    /// Create a new escalation entry.
    ///
    /// If `auto_action` is `Some(AutoAction::Approve)`, the escalation becomes a
    /// "soft-approval" — it auto-approves on expiry instead of auto-denying.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_escalation(
        &self,
        task_id: TaskID,
        agent_id: AgentID,
        reason: EscalationReason,
        context_summary: String,
        decision_point: String,
        options: Vec<String>,
        urgency: String,
        blocking: bool,
        trace_id: TraceID,
        auto_action: Option<AutoAction>,
    ) -> u64 {
        let mut next_id = self.next_id.write().await;
        let id = *next_id;
        *next_id += 1;

        let now = chrono::Utc::now();
        let expires_at = now + chrono::Duration::seconds(self.timeout_secs);
        let urgency_clone = urgency.clone();

        let escalation = PendingEscalation {
            id,
            task_id,
            agent_id,
            reason,
            context_summary,
            decision_point,
            options,
            urgency,
            blocking,
            trace_id,
            created_at: now,
            expires_at,
            auto_action: auto_action.unwrap_or(AutoAction::Deny),
            metadata: default_metadata(),
            resolved: false,
            resolution: None,
            resolved_at: None,
        };

        self.escalations.write().await.push(escalation.clone());
        self.persist_escalation(escalation).await;
        tracing::info!(
            escalation_id = id,
            task_id = %task_id,
            expires_at = %expires_at.to_rfc3339(),
            "New escalation created"
        );

        // Fire-and-forget webhook notification if configured.
        // The URL is validated before use to prevent SSRF attacks.
        if let Some(url) = self.notify_url.read().await.clone() {
            match crate::network_safety::validate_webhook_url_str(&url) {
                Ok(()) => {
                    let payload = serde_json::json!({
                        "escalation_id": id,
                        "task_id": task_id.to_string(),
                        "agent_id": agent_id.to_string(),
                        "urgency": urgency_clone,
                        "blocking": blocking,
                        "expires_at": expires_at.to_rfc3339(),
                    });
                    tokio::spawn(async move {
                        let client = reqwest::Client::new();
                        if let Err(e) = client.post(&url).json(&payload).send().await {
                            tracing::warn!(
                                escalation_id = id,
                                error = %e,
                                "Failed to send escalation webhook notification"
                            );
                        }
                    });
                }
                Err(reason) => {
                    tracing::warn!(
                        escalation_id = id,
                        url = %url,
                        reason = %reason,
                        "Escalation webhook URL rejected — SSRF guard blocked the request"
                    );
                }
            }
        }

        id
    }

    /// List all pending (unresolved) escalations.
    pub async fn list_pending(&self) -> Vec<PendingEscalation> {
        self.escalations
            .read()
            .await
            .iter()
            .filter(|e| !e.resolved)
            .cloned()
            .collect()
    }

    /// List all escalations (including resolved).
    pub async fn list_all(&self) -> Vec<PendingEscalation> {
        self.escalations.read().await.clone()
    }

    /// Get a specific escalation by ID.
    pub async fn get(&self, id: u64) -> Option<PendingEscalation> {
        self.escalations
            .read()
            .await
            .iter()
            .find(|e| e.id == id)
            .cloned()
    }

    /// Resolve an escalation with a human decision.
    /// Returns the task_id, agent_id, and whether it was blocking.
    pub async fn resolve(&self, id: u64, resolution: String) -> Option<(TaskID, AgentID, bool)> {
        let mut to_persist = None;
        let mut escalations = self.escalations.write().await;
        let result = if let Some(esc) = escalations.iter_mut().find(|e| e.id == id && !e.resolved) {
            esc.resolved = true;
            esc.resolution = Some(resolution);
            esc.resolved_at = Some(chrono::Utc::now());
            let task_id = esc.task_id;
            let agent_id = esc.agent_id;
            let blocking = esc.blocking;
            to_persist = Some(esc.clone());
            tracing::info!(
                escalation_id = id,
                task_id = %task_id,
                "Escalation resolved"
            );
            Some((task_id, agent_id, blocking))
        } else {
            None
        };
        drop(escalations);

        if let Some(escalation) = to_persist {
            self.persist_escalation(escalation).await;
        }

        result
    }

    /// Get escalations for a specific task.
    pub async fn for_task(&self, task_id: &TaskID) -> Vec<PendingEscalation> {
        self.escalations
            .read()
            .await
            .iter()
            .filter(|e| e.task_id == *task_id)
            .cloned()
            .collect()
    }

    /// Count pending escalations by urgency level.
    pub async fn pending_counts(&self) -> HashMap<String, usize> {
        let escalations = self.escalations.read().await;
        let mut counts = HashMap::new();
        for esc in escalations.iter().filter(|e| !e.resolved) {
            *counts.entry(esc.urgency.clone()).or_insert(0) += 1;
        }
        counts
    }

    /// Sweep expired escalations. Respects the `auto_action` field:
    /// - `AutoAction::Deny` → auto-deny (original behavior)
    /// - `AutoAction::Approve` → soft-approval (auto-approve on expiry)
    ///
    /// Returns `(id, task_id, agent_id, blocking, auto_action)` for each expired escalation.
    pub async fn sweep_expired(&self) -> Vec<(u64, TaskID, AgentID, bool, AutoAction)> {
        let now = chrono::Utc::now();
        let mut escalations = self.escalations.write().await;
        let mut expired = Vec::new();
        let mut to_persist = Vec::new();

        for esc in escalations.iter_mut() {
            if !esc.resolved && now >= esc.expires_at {
                esc.resolved = true;
                esc.resolved_at = Some(now);

                match esc.auto_action {
                    AutoAction::Approve => {
                        esc.resolution =
                            Some("Auto-approved: soft-approval window expired".to_string());
                        tracing::info!(
                            escalation_id = esc.id,
                            task_id = %esc.task_id,
                            "Escalation auto-approved (soft-approval)"
                        );
                    }
                    AutoAction::Deny => {
                        esc.resolution = Some("Auto-denied: escalation expired".to_string());
                        tracing::warn!(
                            escalation_id = esc.id,
                            task_id = %esc.task_id,
                            "Escalation auto-denied due to expiry"
                        );
                    }
                }

                expired.push((
                    esc.id,
                    esc.task_id,
                    esc.agent_id,
                    esc.blocking,
                    esc.auto_action,
                ));
                to_persist.push(esc.clone());
            }
        }
        drop(escalations);

        for escalation in to_persist {
            self.persist_escalation(escalation).await;
        }

        expired
    }

    /// Create a soft-approval escalation with a 30-second auto-approve window.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_soft_approval(
        &self,
        task_id: TaskID,
        agent_id: AgentID,
        reason: EscalationReason,
        context_summary: String,
        decision_point: String,
        options: Vec<String>,
        trace_id: TraceID,
    ) -> u64 {
        let mut next_id = self.next_id.write().await;
        let id = *next_id;
        *next_id += 1;

        let now = chrono::Utc::now();
        let expires_at = now + chrono::Duration::seconds(30); // 30s soft-approval window

        let escalation = PendingEscalation {
            id,
            task_id,
            agent_id,
            reason,
            context_summary,
            decision_point,
            options,
            urgency: "normal".to_string(),
            blocking: false, // soft-approvals are non-blocking
            trace_id,
            created_at: now,
            expires_at,
            auto_action: AutoAction::Approve,
            metadata: default_metadata(),
            resolved: false,
            resolution: None,
            resolved_at: None,
        };

        self.escalations.write().await.push(escalation.clone());
        self.persist_escalation(escalation).await;
        tracing::info!(
            escalation_id = id,
            task_id = %task_id,
            expires_at = %expires_at.to_rfc3339(),
            "Soft-approval escalation created (auto-approves in 30s)"
        );

        id
    }

    pub async fn create_device_access_escalation(
        &self,
        task_id: TaskID,
        agent_id: AgentID,
        device_id: &str,
        operation: &str,
        trace_id: TraceID,
    ) -> (u64, bool) {
        if let Some(existing) = self.find_pending_device_access(device_id, &agent_id).await {
            return (existing.id, false);
        }

        let mut next_id = self.next_id.write().await;
        let id = *next_id;
        *next_id += 1;

        let now = chrono::Utc::now();
        let expires_at = now + chrono::Duration::seconds(self.timeout_secs);
        let escalation = PendingEscalation {
            id,
            task_id,
            agent_id,
            reason: EscalationReason::AuthorizationRequired,
            context_summary: format!(
                "Agent requested access to hardware device '{}' for '{}' operation.",
                device_id, operation
            ),
            decision_point: format!("Approve HAL access to device '{}'", device_id),
            options: vec!["approve".to_string(), "deny".to_string()],
            urgency: "normal".to_string(),
            blocking: true,
            trace_id,
            created_at: now,
            expires_at,
            auto_action: AutoAction::Deny,
            metadata: serde_json::json!({
                "kind": "device_access",
                "device_id": device_id,
                "operation": operation,
            }),
            resolved: false,
            resolution: None,
            resolved_at: None,
        };

        self.escalations.write().await.push(escalation.clone());
        self.persist_escalation(escalation).await;
        tracing::info!(
            escalation_id = id,
            task_id = %task_id,
            device_id = %device_id,
            "HAL device access escalation created"
        );

        (id, true)
    }

    pub async fn find_pending_device_access(
        &self,
        device_id: &str,
        agent_id: &AgentID,
    ) -> Option<PendingEscalation> {
        self.escalations
            .read()
            .await
            .iter()
            .find(|escalation| {
                !escalation.resolved
                    && escalation.agent_id == *agent_id
                    && escalation
                        .metadata
                        .get("kind")
                        .and_then(serde_json::Value::as_str)
                        == Some("device_access")
                    && escalation
                        .metadata
                        .get("device_id")
                        .and_then(serde_json::Value::as_str)
                        == Some(device_id)
            })
            .cloned()
    }

    pub async fn auto_resolve_device_escalation(
        &self,
        device_id: &str,
        agent_id: Option<&AgentID>,
        approved: bool,
    ) -> usize {
        let mut escalations = self.escalations.write().await;
        let now = chrono::Utc::now();
        let resolution = if approved {
            "Approved by operator"
        } else {
            "Denied by operator"
        };
        let mut updated = Vec::new();

        for escalation in escalations.iter_mut() {
            let is_device_access = escalation
                .metadata
                .get("kind")
                .and_then(serde_json::Value::as_str)
                == Some("device_access");
            let same_device = escalation
                .metadata
                .get("device_id")
                .and_then(serde_json::Value::as_str)
                == Some(device_id);
            let same_agent = agent_id
                .map(|expected| escalation.agent_id == *expected)
                .unwrap_or(true);

            if !escalation.resolved && is_device_access && same_device && same_agent {
                escalation.resolved = true;
                escalation.resolution = Some(resolution.to_string());
                escalation.resolved_at = Some(now);
                updated.push(escalation.clone());
            }
        }
        drop(escalations);

        let count = updated.len();
        for escalation in updated {
            self.persist_escalation(escalation).await;
        }

        count
    }
}

impl Default for EscalationManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_create_and_list_escalation() {
        let manager = EscalationManager::new();
        let task_id = TaskID::new();
        let agent_id = AgentID::new();

        let id = manager
            .create_escalation(
                task_id,
                agent_id,
                EscalationReason::Uncertainty,
                "Agent unsure about file deletion".to_string(),
                "Should I delete /data/old_reports?".to_string(),
                vec!["Yes, delete".to_string(), "No, keep".to_string()],
                "normal".to_string(),
                true,
                TraceID::new(),
                None,
            )
            .await;

        assert_eq!(id, 1);
        let pending = manager.list_pending().await;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].task_id, task_id);
        assert!(pending[0].blocking);
    }

    #[tokio::test]
    async fn test_resolve_escalation() {
        let manager = EscalationManager::new();
        let task_id = TaskID::new();

        let id = manager
            .create_escalation(
                task_id,
                AgentID::new(),
                EscalationReason::AuthorizationRequired,
                "summary".to_string(),
                "decision".to_string(),
                vec![],
                "high".to_string(),
                true,
                TraceID::new(),
                None,
            )
            .await;

        let result = manager.resolve(id, "Approved by admin".to_string()).await;
        assert!(result.is_some());
        let (resolved_task_id, resolved_agent_id, blocking) = result.unwrap();
        assert_eq!(resolved_task_id, task_id);
        assert_eq!(resolved_agent_id, manager.list_all().await[0].agent_id);
        assert!(blocking);

        // Should no longer appear in pending
        assert!(manager.list_pending().await.is_empty());
        // But should still be in all
        assert_eq!(manager.list_all().await.len(), 1);
    }

    #[tokio::test]
    async fn test_resolve_nonexistent_returns_none() {
        let manager = EscalationManager::new();
        assert!(manager.resolve(999, "nope".to_string()).await.is_none());
    }

    #[tokio::test]
    async fn test_pending_counts() {
        let manager = EscalationManager::new();

        for urgency in &["normal", "normal", "high", "critical"] {
            manager
                .create_escalation(
                    TaskID::new(),
                    AgentID::new(),
                    EscalationReason::Uncertainty,
                    "s".to_string(),
                    "d".to_string(),
                    vec![],
                    urgency.to_string(),
                    false,
                    TraceID::new(),
                    None,
                )
                .await;
        }

        let counts = manager.pending_counts().await;
        assert_eq!(counts.get("normal"), Some(&2));
        assert_eq!(counts.get("high"), Some(&1));
        assert_eq!(counts.get("critical"), Some(&1));
    }

    #[tokio::test]
    async fn test_sweep_expired_auto_denies() {
        let manager = EscalationManager {
            escalations: RwLock::new(Vec::new()),
            next_id: RwLock::new(1),
            timeout_secs: 0, // expire immediately
            notify_url: RwLock::new(None),
            state_store: None,
        };

        let task_id = TaskID::new();
        manager
            .create_escalation(
                task_id,
                AgentID::new(),
                EscalationReason::AuthorizationRequired,
                "test".to_string(),
                "test".to_string(),
                vec![],
                "high".to_string(),
                true,
                TraceID::new(),
                None,
            )
            .await;

        // Sweep should auto-deny the expired escalation
        let expired = manager.sweep_expired().await;
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].0, 1); // id
        assert_eq!(expired[0].1, task_id);
        assert_eq!(expired[0].2, manager.list_all().await[0].agent_id);
        assert!(expired[0].3); // blocking
        assert_eq!(expired[0].4, AutoAction::Deny);

        // Should no longer appear in pending
        assert!(manager.list_pending().await.is_empty());

        // Resolution should indicate auto-deny
        let all = manager.list_all().await;
        assert!(all[0].resolution.as_ref().unwrap().contains("Auto-denied"));
    }

    #[tokio::test]
    async fn test_sweep_expired_auto_approves() {
        let manager = EscalationManager {
            escalations: RwLock::new(Vec::new()),
            next_id: RwLock::new(1),
            timeout_secs: 0, // expire immediately
            notify_url: RwLock::new(None),
            state_store: None,
        };

        let task_id = TaskID::new();
        manager
            .create_escalation(
                task_id,
                AgentID::new(),
                EscalationReason::AuthorizationRequired,
                "test".to_string(),
                "test".to_string(),
                vec![],
                "normal".to_string(),
                true,
                TraceID::new(),
                Some(AutoAction::Approve),
            )
            .await;

        let expired = manager.sweep_expired().await;
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].0, 1); // id
        assert_eq!(expired[0].1, task_id);
        assert_eq!(expired[0].2, manager.list_all().await[0].agent_id);
        assert!(expired[0].3); // blocking
        assert_eq!(expired[0].4, AutoAction::Approve);

        assert!(manager.list_pending().await.is_empty());

        let all = manager.list_all().await;
        assert!(all[0]
            .resolution
            .as_ref()
            .unwrap()
            .contains("Auto-approved"));
    }

    #[tokio::test]
    async fn test_restore_from_store_recovers_unresolved_escalations() {
        let dir = tempdir().expect("temp dir");
        let db_path = dir.path().join("kernel_state.db");
        let store = Arc::new(
            KernelStateStore::open(db_path)
                .await
                .expect("state store should open"),
        );

        let manager = EscalationManager::with_state_store(Some(store.clone()));
        let task_id = TaskID::new();
        let agent_id = AgentID::new();

        let unresolved_id = manager
            .create_escalation(
                task_id,
                agent_id,
                EscalationReason::AuthorizationRequired,
                "needs review".to_string(),
                "approve?".to_string(),
                vec!["yes".to_string(), "no".to_string()],
                "high".to_string(),
                true,
                TraceID::new(),
                None,
            )
            .await;

        let resolved_id = manager
            .create_escalation(
                TaskID::new(),
                AgentID::new(),
                EscalationReason::Uncertainty,
                "second".to_string(),
                "resolve".to_string(),
                vec![],
                "normal".to_string(),
                false,
                TraceID::new(),
                None,
            )
            .await;
        manager
            .resolve(resolved_id, "approved".to_string())
            .await
            .expect("resolution should succeed");

        let restored_manager = EscalationManager::with_state_store(Some(store));
        let restored = restored_manager
            .restore_from_store()
            .await
            .expect("restore should succeed");
        assert_eq!(restored, 1, "only unresolved escalation should be restored");

        let pending = restored_manager.list_pending().await;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, unresolved_id);

        // Ensure next ID continues after previously persisted rows.
        let next = restored_manager
            .create_escalation(
                TaskID::new(),
                AgentID::new(),
                EscalationReason::Uncertainty,
                "new".to_string(),
                "new".to_string(),
                vec![],
                "normal".to_string(),
                false,
                TraceID::new(),
                None,
            )
            .await;
        assert!(next > resolved_id);
    }

    #[tokio::test]
    async fn test_create_device_access_escalation_deduplicates_by_device_and_agent() {
        let manager = EscalationManager::new();
        let task_id = TaskID::new();
        let agent_id = AgentID::new();

        let (first_id, created_first) = manager
            .create_device_access_escalation(task_id, agent_id, "gpu:0", "read", TraceID::new())
            .await;
        let (second_id, created_second) = manager
            .create_device_access_escalation(task_id, agent_id, "gpu:0", "read", TraceID::new())
            .await;

        assert!(created_first);
        assert!(!created_second);
        assert_eq!(first_id, second_id);
        assert_eq!(manager.list_pending().await.len(), 1);
        assert_eq!(
            manager.list_pending().await[0].metadata["kind"].as_str(),
            Some("device_access")
        );
    }

    #[tokio::test]
    async fn test_auto_resolve_device_escalation_matches_device_and_agent() {
        let manager = EscalationManager::new();
        let allowed_agent = AgentID::new();
        let other_agent = AgentID::new();

        manager
            .create_device_access_escalation(
                TaskID::new(),
                allowed_agent,
                "sensor:thermal_zone0",
                "read",
                TraceID::new(),
            )
            .await;
        manager
            .create_device_access_escalation(
                TaskID::new(),
                other_agent,
                "sensor:thermal_zone0",
                "read",
                TraceID::new(),
            )
            .await;

        let resolved = manager
            .auto_resolve_device_escalation("sensor:thermal_zone0", Some(&allowed_agent), true)
            .await;

        assert_eq!(resolved, 1);
        let pending = manager.list_pending().await;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].agent_id, other_agent);
    }
}
