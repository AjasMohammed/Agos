//! The shared directory every conversation gets.
//!
//! Two agents in a conversation have no ground they both stand on: file tools
//! confine each to `data_dir/agents/<name>/` ([`agentos_tools::traits::agent_home_dir`])
//! and nothing bridges the two. On 2026-09-21 that cost a ten-turn deadlock —
//! one agent wrote a project into its own home, the other could not open it,
//! and both invented a permission grant that no tool can issue.
//!
//! This module mints `data_dir/convos/<convo_id>/shared/` and gives every
//! participant a [`StorageZone`] over it. Zones are the existing primitive for
//! dynamic, expiring, kernel-mediated directory access: every file tool already
//! honours them, and — unlike a workspace path — they need no `fs.workspace`
//! permission, because a zone is kernel space rather than the operator's host
//! filesystem. Access dies with the convo via the existing
//! [`crate::managed_storage::ZoneTable::sweep_expired`].

use std::path::{Path, PathBuf};

use agentos_audit::{AuditEntry, AuditEventType, AuditSeverity};
use agentos_types::TraceID;

use crate::managed_storage::{StorageZone, ZoneAccess, ZoneGrantSource};
use crate::Kernel;

/// `data_dir/convos/<convo_id>/shared` — the one definition.
///
/// The id is a UUID everywhere it is produced ([`crate::convo_store`]), but it
/// arrives here as a string from the bus, so anything that is not one path
/// segment of `[A-Za-z0-9-]` is rejected rather than joined: a `..` in the id
/// would walk the shared root out of `convos/`.
pub fn convo_shared_dir(data_dir: &Path, convo_id: &str) -> Option<PathBuf> {
    if convo_id.is_empty()
        || convo_id.len() > 64
        || !convo_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        tracing::warn!(convo_id, "convo workspace: refusing unsafe convo id");
        return None;
    }
    Some(data_dir.join("convos").join(convo_id).join("shared"))
}

impl Kernel {
    /// Create the conversation's shared directory and give every participant a
    /// read-write zone over it.
    ///
    /// Idempotent: re-entering a convo (`/continue`, a resumed DM session)
    /// refreshes the expiry instead of stacking zones. Returns the canonical
    /// path, or `None` when the directory could not be created or no
    /// participant name resolves — callers treat that as "no shared workspace
    /// this turn", never as a convo failure.
    pub async fn ensure_convo_workspace(
        &self,
        convo_id: &str,
        participants: &[String],
        expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Option<PathBuf> {
        let dir = convo_shared_dir(&self.data_dir, convo_id)?;
        if let Err(e) = tokio::fs::create_dir_all(&dir).await {
            tracing::warn!(path = %dir.display(), error = %e, "convo workspace: mkdir failed");
            return None;
        }
        // Zone membership is tested against canonical paths, so the zone has to
        // store one.
        let dir = match dir.canonicalize() {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(path = %dir.display(), error = %e, "convo workspace: canonicalize failed");
                return None;
            }
        };

        let ids: Vec<(String, agentos_types::AgentID)> = {
            let registry = self.agent_registry.read().await;
            participants
                .iter()
                .filter_map(|name| match registry.get_by_name(name) {
                    Some(a) => Some((name.clone(), a.id)),
                    None => {
                        // Loudly: the others still get the workspace and its
                        // path goes in their prompt, so a silently missing
                        // participant is a pair that can see the same directory
                        // named and only one of them able to open it.
                        tracing::warn!(
                            convo_id,
                            participant = name,
                            "convo workspace: participant does not resolve — it gets no zone"
                        );
                        None
                    }
                })
                .collect()
        };
        if ids.is_empty() {
            tracing::warn!(convo_id, "convo workspace: no participant resolved");
            return None;
        }

        for (name, agent_id) in ids {
            // Refresh rather than stack: a re-entered convo would otherwise
            // accumulate one zone per entry, all pointing at the same path.
            for existing in self.zone_table.list_for_agent(&agent_id).await {
                if existing.path == dir {
                    self.zone_table.remove(&existing.zone_id, &agent_id).await;
                }
            }
            let zone = StorageZone {
                zone_id: self.zone_table.next_zone_id().await,
                agent_id,
                path: dir.clone(),
                access: ZoneAccess::ReadWrite,
                created_at: chrono::Utc::now(),
                expires_at: Some(expires_at),
                granted_by: ZoneGrantSource::Convo {
                    convo_id: convo_id.to_string(),
                },
            };
            // Deliberately `insert`, not `insert_if_under_limit`: the per-agent
            // zone cap bounds what an agent grants *itself* through the storage
            // capability. A kernel-minted convo zone must not be refused
            // because the agent is at that cap — it would reintroduce the
            // deadlock this exists to remove.
            self.zone_table.insert(zone).await;
            self.audit_log(AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: TraceID::new(),
                event_type: AuditEventType::CapabilityGranted,
                agent_id: Some(agent_id),
                task_id: None,
                tool_id: None,
                details: serde_json::json!({
                    "kind": "convo_zone",
                    "convo_id": convo_id,
                    "agent": name,
                    "path": dir.to_string_lossy(),
                    "access": "rw",
                    "expires_at": expires_at.to_rfc3339(),
                }),
                severity: AuditSeverity::Info,
                reversible: true,
                rollback_ref: None,
            });
        }
        Some(dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_dir_rejects_unsafe_ids() {
        let data = Path::new("/tmp/agentos-test");
        assert!(convo_shared_dir(data, "../../etc").is_none());
        assert!(convo_shared_dir(data, "a/b").is_none());
        assert!(convo_shared_dir(data, "").is_none());
        assert!(convo_shared_dir(data, &"x".repeat(65)).is_none());
    }

    #[test]
    fn shared_dir_accepts_a_uuid() {
        let data = Path::new("/tmp/agentos-test");
        let id = "8a060bd4-bb4d-4806-bb64-ac8a883f9518";
        assert_eq!(
            convo_shared_dir(data, id).unwrap(),
            data.join("convos").join(id).join("shared")
        );
    }
}
