use crate::kernel::Kernel;
use crate::user_profile_store::{UpsertOutcome, MAX_KEY_LEN, UNPINNED_RANK};
use agentos_audit::{AuditEntry, AuditEventType, AuditSeverity};
use agentos_bus::KernelResponse;
use agentos_types::{
    ProfileCategory, ProfileEntry, ProfileEntryID, ProfileEntryStatus, ProfileSource, TraceID,
};

impl Kernel {
    pub(crate) async fn cmd_user_prefs_list_pending(&self, limit: u32) -> KernelResponse {
        match self.user_pref_proposal_store.list_pending(limit).await {
            Ok(rows) => KernelResponse::Success {
                data: Some(serde_json::json!({"proposals": rows})),
            },
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    pub(crate) async fn cmd_user_prefs_accept(&self, proposal_id: String) -> KernelResponse {
        let Some(p) = (match self.user_pref_proposal_store.get(&proposal_id).await {
            Ok(v) => v,
            Err(e) => {
                return KernelResponse::Error {
                    message: e.to_string(),
                };
            }
        }) else {
            return KernelResponse::Error {
                message: format!("proposal not found: {proposal_id}"),
            };
        };

        // Order matters: claim the proposal *first*. Only on a successful
        // pending → accepted transition do we apply the side effect (memory
        // write). This prevents double-writes on retry after a partial failure.
        match self.user_pref_proposal_store.accept(&proposal_id).await {
            Ok(true) => {
                if let Err(e) = self
                    .context_memory_store
                    .write(
                        &p.agent_id.to_string(),
                        &format!("- {}", p.content),
                        Some("user_pref_proposal_accept"),
                    )
                    .await
                {
                    // Memory write failed after the proposal was claimed.
                    // Surface the error to the operator; the proposal stays
                    // accepted (operator can manually retry the memory write
                    // or just paste the content).
                    return KernelResponse::Error {
                        message: format!("proposal accepted but context-memory write failed: {e}"),
                    };
                }

                self.audit
                    .append(AuditEntry {
                        timestamp: chrono::Utc::now(),
                        trace_id: TraceID::new(),
                        event_type: AuditEventType::ProposalAccepted,
                        agent_id: Some(p.agent_id),
                        task_id: Some(p.task_id),
                        tool_id: None,
                        details: serde_json::json!({
                            "proposal_id": proposal_id,
                            "confidence": p.confidence,
                            "kind": p.kind,
                        }),
                        severity: AuditSeverity::Info,
                        reversible: false,
                        rollback_ref: None,
                    })
                    .ok();

                // Promote the accepted preference into the structured profile
                // store. Best-effort: the proposal is already claimed and the
                // context-memory write succeeded, so a profile-store hiccup
                // must not reverse an accepted proposal — log and continue.
                self.promote_proposal_to_profile(&proposal_id, &p).await;

                KernelResponse::Success {
                    data: Some(serde_json::json!({"accepted": true})),
                }
            }
            Ok(false) => KernelResponse::Error {
                message: "proposal already reviewed".to_string(),
            },
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    pub(crate) async fn cmd_user_prefs_reject(&self, proposal_id: String) -> KernelResponse {
        // Snapshot the proposal up-front so we can record agent/task in the
        // audit entry — `reject()` only returns a bool.
        let proposal = match self.user_pref_proposal_store.get(&proposal_id).await {
            Ok(v) => v,
            Err(e) => {
                return KernelResponse::Error {
                    message: e.to_string(),
                };
            }
        };
        match self.user_pref_proposal_store.reject(&proposal_id).await {
            Ok(true) => {
                if let Some(p) = proposal {
                    self.audit
                        .append(AuditEntry {
                            timestamp: chrono::Utc::now(),
                            trace_id: TraceID::new(),
                            event_type: AuditEventType::ProposalRejected,
                            agent_id: Some(p.agent_id),
                            task_id: Some(p.task_id),
                            tool_id: None,
                            details: serde_json::json!({
                                "proposal_id": proposal_id,
                                "confidence": p.confidence,
                            }),
                            severity: AuditSeverity::Info,
                            reversible: false,
                            rollback_ref: None,
                        })
                        .ok();
                }
                KernelResponse::Success {
                    data: Some(serde_json::json!({"rejected": true})),
                }
            }
            Ok(false) => KernelResponse::Error {
                message: "proposal not found or already reviewed".to_string(),
            },
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    pub(crate) async fn cmd_user_prefs_stats(&self) -> KernelResponse {
        match self.user_pref_proposal_store.stats().await {
            Ok(stats) => KernelResponse::Success {
                data: Some(serde_json::json!({"stats": stats})),
            },
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    /// Promote an accepted preference proposal into the structured profile
    /// store as a durable [`ProfileEntry`]. Best-effort (logs on failure).
    ///
    /// Gated on `user_profile.enabled` — when the operator turns the profile
    /// store off, accepts still write to context memory (legacy behavior) but
    /// no structured entry is promoted.
    async fn promote_proposal_to_profile(
        &self,
        proposal_id: &str,
        p: &crate::user_pref_proposals::UserPrefProposal,
    ) {
        if !self.config.user_profile.enabled {
            return;
        }
        let category = classify_category(&p.content);
        let now = chrono::Utc::now();
        // Clamp to the store's *effective* floor (operator-configurable) so a
        // floor-clamped promotion can never be rejected by `upsert`.
        let floor = self.user_profile_store.min_confidence();
        let entry = ProfileEntry {
            id: ProfileEntryID::new(),
            category,
            key: derive_key(&p.content),
            value: p.content.clone(), // store truncates to cap
            confidence: p.confidence.max(floor),
            source: ProfileSource::FromProposal {
                proposal_id: proposal_id.to_string(),
            },
            pin_rank: UNPINNED_RANK, // accepted prefs start unpinned (L1)
            usage_count: 0,
            last_used: None,
            created_at: now,
            updated_at: now,
            status: ProfileEntryStatus::Active,
        };
        match self.user_profile_store.upsert(entry).await {
            Ok(outcome) => {
                let event_type = match outcome {
                    UpsertOutcome::Inserted => AuditEventType::ProfileEntryAdded,
                    UpsertOutcome::Updated => AuditEventType::ProfileEntryUpdated,
                };
                self.audit
                    .append(AuditEntry {
                        timestamp: chrono::Utc::now(),
                        trace_id: TraceID::new(),
                        event_type,
                        agent_id: Some(p.agent_id),
                        task_id: Some(p.task_id),
                        tool_id: None,
                        details: serde_json::json!({
                            "proposal_id": proposal_id,
                            "category": category.as_str(),
                        }),
                        severity: AuditSeverity::Info,
                        reversible: false,
                        rollback_ref: None,
                    })
                    .ok();
            }
            Err(e) => {
                tracing::warn!("profile promotion failed for {proposal_id}: {e}");
            }
        }
    }
}

/// Deterministic keyword classifier — no LLM call. Order matters: more specific
/// categories are checked before the generic fallbacks.
fn classify_category(content: &str) -> ProfileCategory {
    let c = content.to_lowercase();
    if c.contains("rust")
        || c.contains("python")
        || c.contains("typescript")
        || c.contains("framework")
        || c.contains("library")
        || c.contains("stack")
    {
        ProfileCategory::TechStack
    } else if c.contains("concise")
        || c.contains("verbose")
        || c.contains("tone")
        || c.contains("format")
        || c.contains("preamble")
        || c.contains("style")
        || c.contains("bullet")
    {
        ProfileCategory::CommunicationStyle
    } else if c.contains("never")
        || c.contains("always")
        || c.contains("must not")
        || c.contains("avoid")
        || c.contains("don't")
    {
        ProfileCategory::Constraint
    } else if c.contains("workflow")
        || c.contains("test")
        || c.contains("commit")
        || c.contains("pipeline")
        || c.contains("review")
    {
        ProfileCategory::Workflow
    } else {
        ProfileCategory::Other
    }
}

/// Slug from the first four words. Re-accepting a proposal with the *same*
/// leading words (and the same classified category) updates the existing
/// `(category, key)` row instead of duplicating. This is a deliberately narrow,
/// deterministic dedup: reworded or recategorized restatements still produce a
/// distinct row.
///
/// TODO(phase5): semantic dedup in the active-learning loop collapses
/// near-duplicate restatements that this lexical key cannot catch.
fn derive_key(content: &str) -> String {
    let slug: String = content
        .to_lowercase()
        .split_whitespace()
        .take(4)
        .collect::<Vec<_>>()
        .join("_")
        .chars()
        .filter(|ch| ch.is_alphanumeric() || *ch == '_')
        .take(MAX_KEY_LEN)
        .collect();
    if slug.is_empty() {
        "pref".to_string()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_category_maps_keywords() {
        assert_eq!(
            classify_category("use Rust always"),
            ProfileCategory::TechStack
        );
        assert_eq!(
            classify_category("be concise, no preamble"),
            ProfileCategory::CommunicationStyle
        );
        assert_eq!(
            classify_category("never delete prod data"),
            ProfileCategory::Constraint
        );
        assert_eq!(
            classify_category("run the review workflow"),
            ProfileCategory::Workflow
        );
        assert_eq!(
            classify_category("the weather is nice"),
            ProfileCategory::Other
        );
    }

    #[test]
    fn derive_key_is_stable_and_slug_safe() {
        let k1 = derive_key("User prefers terse answers always");
        let k2 = derive_key("user prefers terse answers, with detail");
        // First four words drive the slug → same key for re-statement.
        assert_eq!(k1, k2);
        assert!(k1.chars().all(|c| c.is_alphanumeric() || c == '_'));
        assert!(!k1.is_empty());
    }

    #[test]
    fn derive_key_handles_empty() {
        assert_eq!(derive_key("   "), "pref");
    }
}
