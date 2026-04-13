//! Dynamic Capability Negotiation — the `CapabilityBroker`.
//!
//! Allows agents to request capabilities they don't currently hold at runtime.
//! The broker checks policy (deny/allow/escalate) and either auto-grants,
//! creates an escalation for operator approval, or denies immediately.
//!
//! Grants are ephemeral: scoped to a specific resource, time-limited (TTL),
//! and automatically revoked on expiry.

use agentos_types::{AgentID, AgentOSError, TaskID};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// Ephemeral grant model
// ---------------------------------------------------------------------------

/// An ephemeral capability grant — scoped, time-limited, revocable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EphemeralGrant {
    /// Unique grant identifier.
    pub grant_id: String,
    /// The agent that holds this grant.
    pub agent_id: AgentID,
    /// The task that requested it.
    pub task_id: TaskID,
    /// The capability domain (e.g., "env", "net", "storage").
    pub domain: String,
    /// The specific action (e.g., "install", "http").
    pub action: String,
    /// The specific resource (e.g., package name, URL, path).
    pub resource: String,
    /// When this grant was issued.
    pub granted_at: chrono::DateTime<chrono::Utc>,
    /// When this grant expires.
    pub expires_at: chrono::DateTime<chrono::Utc>,
    /// How this grant was obtained.
    pub grant_source: GrantSource,
}

impl EphemeralGrant {
    pub fn is_expired(&self) -> bool {
        chrono::Utc::now() > self.expires_at
    }
}

/// How an ephemeral grant was obtained.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantSource {
    /// Auto-granted by policy (resource matched an allowlist).
    Policy { rule: String },
    /// Granted by operator approval via escalation.
    OperatorApproval { escalation_id: u64 },
}

// ---------------------------------------------------------------------------
// Policy effect
// ---------------------------------------------------------------------------

/// What happens when a capability is requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyEffect {
    /// Auto-grant without human approval.
    Allow,
    /// Always deny (no escalation, no override).
    Deny,
    /// Require human approval via escalation.
    Escalate,
}

// ---------------------------------------------------------------------------
// Broker
// ---------------------------------------------------------------------------

/// Default TTL for ephemeral grants (1 hour).
const DEFAULT_GRANT_TTL_SECS: i64 = 3600;

/// Maximum grants per agent.
const MAX_GRANTS_PER_AGENT: usize = 50;

/// Internal state for the broker, protected by a single lock.
struct BrokerInner {
    grants: HashMap<String, EphemeralGrant>,
    next_id: u64,
}

/// The capability broker — manages dynamic capability negotiation.
pub struct CapabilityBroker {
    /// All state behind a single lock to prevent nested lock deadlocks.
    inner: Arc<RwLock<BrokerInner>>,
    /// Grant TTL in seconds.
    grant_ttl_secs: i64,
}

impl CapabilityBroker {
    pub fn new(grant_ttl_secs: i64) -> Self {
        Self {
            inner: Arc::new(RwLock::new(BrokerInner {
                grants: HashMap::new(),
                next_id: 1,
            })),
            grant_ttl_secs,
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_GRANT_TTL_SECS)
    }

    /// Request a capability grant. Returns the grant if auto-allowed by policy.
    ///
    /// The broker evaluates the request against a simple three-tier policy:
    /// 1. DENY: resource matches a deny pattern → immediate denial
    /// 2. ALLOW: resource matches an allow pattern → auto-grant
    /// 3. ESCALATE: no match → requires operator approval (returns error)
    ///
    /// The policy evaluation is intentionally simple in this phase. Phase 8 adds
    /// a full policy engine with configurable rules.
    pub async fn request_capability(
        &self,
        agent_id: AgentID,
        task_id: TaskID,
        domain: &str,
        action: &str,
        resource: &str,
        effect: PolicyEffect,
    ) -> Result<EphemeralGrant, AgentOSError> {
        match effect {
            PolicyEffect::Deny => Err(AgentOSError::PermissionDenied {
                resource: format!("{domain}.{action}"),
                operation: format!("capability request denied by policy for resource '{resource}'"),
            }),
            PolicyEffect::Escalate => Err(AgentOSError::KernelError {
                reason: format!(
                    "capability {domain}.{action} for '{resource}' requires operator approval"
                ),
            }),
            PolicyEffect::Allow => {
                // Single lock for check + ID generation + insert (no nested locks).
                let mut inner = self.inner.write().await;
                let agent_count = inner
                    .grants
                    .values()
                    .filter(|g| g.agent_id == agent_id && !g.is_expired())
                    .count();
                if agent_count >= MAX_GRANTS_PER_AGENT {
                    return Err(AgentOSError::KernelError {
                        reason: format!(
                            "agent has reached maximum of {MAX_GRANTS_PER_AGENT} active grants"
                        ),
                    });
                }

                let grant_id = format!("grant-{}", inner.next_id);
                inner.next_id += 1;

                let now = chrono::Utc::now();
                let grant = EphemeralGrant {
                    grant_id: grant_id.clone(),
                    agent_id,
                    task_id,
                    domain: domain.to_string(),
                    action: action.to_string(),
                    resource: resource.to_string(),
                    granted_at: now,
                    expires_at: now + chrono::Duration::seconds(self.grant_ttl_secs),
                    grant_source: GrantSource::Policy {
                        rule: format!("auto-allow {domain}.{action}:{resource}"),
                    },
                };

                inner.grants.insert(grant_id, grant.clone());

                Ok(grant)
            }
        }
    }

    /// Check if an agent has an active grant for a specific capability.
    pub async fn has_grant(
        &self,
        agent_id: &AgentID,
        domain: &str,
        action: &str,
        resource: &str,
    ) -> bool {
        let inner = self.inner.read().await;
        inner.grants.values().any(|g| {
            g.agent_id == *agent_id
                && g.domain == domain
                && g.action == action
                && g.resource == resource
                && !g.is_expired()
        })
    }

    /// List all active grants for an agent.
    pub async fn list_grants(&self, agent_id: &AgentID) -> Vec<EphemeralGrant> {
        let inner = self.inner.read().await;
        inner
            .grants
            .values()
            .filter(|g| g.agent_id == *agent_id && !g.is_expired())
            .cloned()
            .collect()
    }

    /// Revoke a specific grant. Returns true if found and revoked.
    pub async fn revoke_grant(&self, grant_id: &str, agent_id: &AgentID) -> bool {
        let mut inner = self.inner.write().await;
        if let Some(grant) = inner.grants.get(grant_id) {
            if grant.agent_id == *agent_id {
                inner.grants.remove(grant_id);
                return true;
            }
        }
        false
    }

    /// Revoke all grants for an agent. Returns the count revoked.
    pub async fn revoke_all_for_agent(&self, agent_id: &AgentID) -> usize {
        let mut inner = self.inner.write().await;
        let before = inner.grants.len();
        inner.grants.retain(|_, g| g.agent_id != *agent_id);
        before - inner.grants.len()
    }

    /// Sweep expired grants. Returns the count removed.
    pub async fn sweep_expired(&self) -> usize {
        let mut inner = self.inner.write().await;
        let before = inner.grants.len();
        inner.grants.retain(|_, g| !g.is_expired());
        before - inner.grants.len()
    }

    /// Total number of active (non-expired) grants.
    pub async fn active_grant_count(&self) -> usize {
        let inner = self.inner.read().await;
        inner.grants.values().filter(|g| !g.is_expired()).count()
    }
}

impl Default for CapabilityBroker {
    fn default() -> Self {
        Self::with_defaults()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::{AgentID, TaskID};

    #[tokio::test]
    async fn auto_grant_allowed_capability() {
        let broker = CapabilityBroker::with_defaults();
        let agent = AgentID::new();
        let task = TaskID::new();

        let grant = broker
            .request_capability(agent, task, "env", "install", "flask", PolicyEffect::Allow)
            .await
            .unwrap();

        assert_eq!(grant.domain, "env");
        assert_eq!(grant.action, "install");
        assert_eq!(grant.resource, "flask");
        assert!(!grant.is_expired());
    }

    #[tokio::test]
    async fn deny_denied_capability() {
        let broker = CapabilityBroker::with_defaults();
        let agent = AgentID::new();
        let task = TaskID::new();

        let err = broker
            .request_capability(
                agent,
                task,
                "storage",
                "zone.create",
                "/etc/shadow",
                PolicyEffect::Deny,
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("denied by policy"));
    }

    #[tokio::test]
    async fn escalate_unknown_capability() {
        let broker = CapabilityBroker::with_defaults();
        let agent = AgentID::new();
        let task = TaskID::new();

        let err = broker
            .request_capability(
                agent,
                task,
                "net",
                "http",
                "internal-api.company.com",
                PolicyEffect::Escalate,
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("requires operator approval"));
    }

    #[tokio::test]
    async fn has_grant_after_request() {
        let broker = CapabilityBroker::with_defaults();
        let agent = AgentID::new();
        let task = TaskID::new();

        assert!(!broker.has_grant(&agent, "env", "install", "flask").await);

        broker
            .request_capability(agent, task, "env", "install", "flask", PolicyEffect::Allow)
            .await
            .unwrap();

        assert!(broker.has_grant(&agent, "env", "install", "flask").await);
    }

    #[tokio::test]
    async fn list_grants_for_agent() {
        let broker = CapabilityBroker::with_defaults();
        let agent = AgentID::new();
        let task = TaskID::new();

        broker
            .request_capability(agent, task, "env", "install", "flask", PolicyEffect::Allow)
            .await
            .unwrap();
        broker
            .request_capability(agent, task, "env", "install", "numpy", PolicyEffect::Allow)
            .await
            .unwrap();

        let grants = broker.list_grants(&agent).await;
        assert_eq!(grants.len(), 2);
    }

    #[tokio::test]
    async fn revoke_grant() {
        let broker = CapabilityBroker::with_defaults();
        let agent = AgentID::new();
        let task = TaskID::new();

        let grant = broker
            .request_capability(agent, task, "env", "install", "flask", PolicyEffect::Allow)
            .await
            .unwrap();

        assert!(broker.revoke_grant(&grant.grant_id, &agent).await);
        assert!(!broker.has_grant(&agent, "env", "install", "flask").await);
    }

    #[tokio::test]
    async fn revoke_all_for_agent() {
        let broker = CapabilityBroker::with_defaults();
        let agent = AgentID::new();
        let task = TaskID::new();

        broker
            .request_capability(agent, task, "env", "install", "flask", PolicyEffect::Allow)
            .await
            .unwrap();
        broker
            .request_capability(
                agent,
                task,
                "net",
                "http",
                "github.com",
                PolicyEffect::Allow,
            )
            .await
            .unwrap();

        let count = broker.revoke_all_for_agent(&agent).await;
        assert_eq!(count, 2);
        assert!(broker.list_grants(&agent).await.is_empty());
    }

    #[tokio::test]
    async fn agent_isolation() {
        let broker = CapabilityBroker::with_defaults();
        let agent_a = AgentID::new();
        let agent_b = AgentID::new();
        let task = TaskID::new();

        broker
            .request_capability(
                agent_a,
                task,
                "env",
                "install",
                "flask",
                PolicyEffect::Allow,
            )
            .await
            .unwrap();

        assert!(!broker.has_grant(&agent_b, "env", "install", "flask").await);
        assert!(broker.list_grants(&agent_b).await.is_empty());
    }

    #[tokio::test]
    async fn grant_expiry() {
        let broker = CapabilityBroker::new(0); // TTL=0 → expires immediately
        let agent = AgentID::new();
        let task = TaskID::new();

        broker
            .request_capability(agent, task, "env", "install", "flask", PolicyEffect::Allow)
            .await
            .unwrap();

        // Grant should be expired immediately
        assert!(!broker.has_grant(&agent, "env", "install", "flask").await);
    }

    #[tokio::test]
    async fn sweep_expired_grants() {
        let broker = CapabilityBroker::new(0); // All grants expire immediately
        let agent = AgentID::new();
        let task = TaskID::new();

        broker
            .request_capability(agent, task, "env", "install", "flask", PolicyEffect::Allow)
            .await
            .unwrap();
        broker
            .request_capability(agent, task, "env", "install", "numpy", PolicyEffect::Allow)
            .await
            .unwrap();

        let swept = broker.sweep_expired().await;
        assert_eq!(swept, 2);
        assert_eq!(broker.active_grant_count().await, 0);
    }

    #[tokio::test]
    async fn max_grants_enforced() {
        let broker = CapabilityBroker::with_defaults();
        let agent = AgentID::new();
        let task = TaskID::new();

        // Fill up to MAX_GRANTS_PER_AGENT
        for i in 0..MAX_GRANTS_PER_AGENT {
            broker
                .request_capability(
                    agent,
                    task,
                    "env",
                    "install",
                    &format!("pkg-{i}"),
                    PolicyEffect::Allow,
                )
                .await
                .unwrap();
        }

        // One more should fail
        let err = broker
            .request_capability(
                agent,
                task,
                "env",
                "install",
                "one-too-many",
                PolicyEffect::Allow,
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("maximum"));
    }

    #[tokio::test]
    async fn other_agent_cannot_revoke() {
        let broker = CapabilityBroker::with_defaults();
        let agent_a = AgentID::new();
        let agent_b = AgentID::new();
        let task = TaskID::new();

        let grant = broker
            .request_capability(
                agent_a,
                task,
                "env",
                "install",
                "flask",
                PolicyEffect::Allow,
            )
            .await
            .unwrap();

        // Agent B can't revoke Agent A's grant
        assert!(!broker.revoke_grant(&grant.grant_id, &agent_b).await);

        // Grant still exists for Agent A
        assert!(broker.has_grant(&agent_a, "env", "install", "flask").await);
    }
}
