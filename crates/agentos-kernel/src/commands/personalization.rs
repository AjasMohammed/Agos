//! Personalization governance command handlers (Phase 6).
//!
//! Implements `status`, `export`, and `forget` for the proactive personalization
//! subsystem. All three operations are dispatched from `run_loop.rs` via
//! `KernelCommand::PersonalizationGovernance { action }`.
//!
//! **Right-to-forget invariant**: `forget` is best-effort — each store is wiped
//! in its own transaction and partial failures are surfaced via `details.partial`,
//! never silently swallowed.

use crate::kernel::Kernel;
use agentos_audit::{AuditEntry, AuditEventType, AuditSeverity};
use agentos_bus::{KernelResponse, PersonalizationAction};
use agentos_types::TraceID;

impl Kernel {
    /// Dispatch a `PersonalizationGovernance` command.
    pub(crate) async fn cmd_personalization(
        &self,
        action: PersonalizationAction,
    ) -> KernelResponse {
        match action {
            PersonalizationAction::Status => self.cmd_personalization_status().await,
            PersonalizationAction::Export => self.cmd_personalization_export().await,
            PersonalizationAction::Forget => self.cmd_personalization_forget().await,
        }
    }

    // ── Status ───────────────────────────────────────────────────────────────

    #[tracing::instrument(skip_all, fields(op = "personalization_status"))]
    async fn cmd_personalization_status(&self) -> KernelResponse {
        let cfg = &self.config.personalization;

        // Profile active row count — single COUNT(*) rather than fetching all rows.
        let profile_rows = self.user_profile_store.count_active().await.unwrap_or(0);

        // Interest row count — use COUNT(*) so zero-/negative-decayed rows are
        // included; top_interests() would silently drop them (W7 fix).
        let interest_rows = self.interest_model.count_interests().await.unwrap_or(0);

        // Recommendation row count.
        let recommendation_rows = self
            .recommendation_engine
            .store()
            .list(u32::MAX)
            .await
            .map(|v| v.len())
            .unwrap_or(0);

        tracing::info!(
            target = "metrics",
            profile_rows,
            interest_rows,
            recommendation_rows,
            enabled = cfg.enabled,
            "personalization.status"
        );

        KernelResponse::Success {
            data: Some(serde_json::json!({
                "enabled": cfg.enabled,
                "proactive_enabled": cfg.proactive_enabled,
                "profile_rows": profile_rows,
                "interest_rows": interest_rows,
                "recommendation_rows": recommendation_rows,
                "retention": {
                    "interest_decay_half_life_hours": cfg.interest_decay_half_life_hours,
                    "interest_min_score": cfg.interest_min_score,
                    "max_recommendations_per_day": cfg.max_recommendations_per_day,
                    "recommendation_dedup_cooldown_hours": cfg.recommendation_dedup_cooldown_hours,
                    "profile_pin_cap": cfg.profile_pin_cap,
                    "profile_token_budget": cfg.profile_token_budget,
                }
            })),
        }
    }

    // ── Export ───────────────────────────────────────────────────────────────

    #[tracing::instrument(skip_all, fields(op = "personalization_export"))]
    async fn cmd_personalization_export(&self) -> KernelResponse {
        // Dump all three stores.
        let profile_entries = self
            .user_profile_store
            .list(u32::MAX)
            .await
            .unwrap_or_default();

        // Use load_all_raw so zero-/negative-decayed rows are included in the
        // export — a complete dump, not a filtered view (W7 fix).
        let interest_entries = self
            .interest_model
            .load_all_interests()
            .await
            .unwrap_or_default();

        let recommendation_entries = self
            .recommendation_engine
            .store()
            .list(u32::MAX)
            .await
            .unwrap_or_default();

        let export_doc = serde_json::json!({
            "exported_at": chrono::Utc::now().to_rfc3339(),
            "profile": profile_entries,
            "interests": interest_entries,
            "recommendations": recommendation_entries,
        });

        let json_str = match serde_json::to_string_pretty(&export_doc) {
            Ok(s) => s,
            Err(e) => {
                return KernelResponse::Error {
                    message: format!("failed to serialize export: {e}"),
                }
            }
        };

        let export_bytes = json_str.len();

        self.audit
            .append(AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: TraceID::new(),
                event_type: AuditEventType::PersonalizationDataExported,
                agent_id: None,
                task_id: None,
                tool_id: None,
                details: serde_json::json!({
                    "profile_rows": profile_entries.len(),
                    "interest_rows": interest_entries.len(),
                    "recommendation_rows": recommendation_entries.len(),
                    "export_bytes": export_bytes,
                }),
                severity: AuditSeverity::Info,
                reversible: false,
                rollback_ref: None,
            })
            .ok();

        tracing::info!(
            target = "metrics",
            profile_rows = profile_entries.len(),
            interest_rows = interest_entries.len(),
            recommendation_rows = recommendation_entries.len(),
            export_bytes,
            "personalization.export"
        );

        KernelResponse::Success {
            data: Some(serde_json::json!({
                "json": json_str,
            })),
        }
    }

    // ── Forget ───────────────────────────────────────────────────────────────

    /// Atomic right-to-forget: clears all three personalization stores plus
    /// the accepted-preference context-memory entries. Best-effort — partial
    /// failures are logged and surfaced in the response (never silently swallowed).
    #[tracing::instrument(skip_all, fields(op = "personalization_forget"))]
    async fn cmd_personalization_forget(&self) -> KernelResponse {
        let mut partial = false;

        // 1. Profile store.
        let profile_cleared = match self.user_profile_store.clear_all().await {
            Ok(n) => {
                tracing::info!(cleared = n, "personalization_forget: profile cleared");
                n
            }
            Err(e) => {
                tracing::error!(error = %e, "personalization_forget: profile clear failed");
                partial = true;
                0
            }
        };

        // 2. Interest store — delegate to InterestModel::clear_interests().
        let interests_cleared = match self.interest_model.clear_interests().await {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(error = %e, "personalization_forget: interest store clear failed");
                partial = true;
                0
            }
        };
        let interests_note = if interests_cleared > 0 {
            format!("{interests_cleared} interest rows cleared")
        } else {
            "no interest rows".to_string()
        };

        // 3. Recommendations store.
        let recommendations_cleared = match self.recommendation_engine.store().clear_all().await {
            Ok(n) => {
                tracing::info!(
                    cleared = n,
                    "personalization_forget: recommendations cleared"
                );
                n
            }
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "personalization_forget: recommendations clear failed"
                );
                partial = true;
                0
            }
        };

        // 4. Context memory — delete accepted-preference history entries tagged with
        //    the two known tags written by the user-adaptation accept flow.
        let memory_cleared = match self
            .context_memory_store
            .delete_by_tags(&["user_pref_proposal_accept", "user_pref_proposal_accept_web"])
            .await
        {
            Ok(n) => {
                tracing::info!(
                    cleared = n,
                    "personalization_forget: context memory tags cleared"
                );
                n
            }
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "personalization_forget: context memory tag deletion failed"
                );
                partial = true;
                0
            }
        };

        // 5. Audit entry.
        self.audit
            .append(AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: TraceID::new(),
                event_type: AuditEventType::PersonalizationDataForgotten,
                agent_id: None,
                task_id: None,
                tool_id: None,
                details: serde_json::json!({
                    "profile_cleared": profile_cleared,
                    "interests_cleared": interests_cleared,
                    "recommendations_cleared": recommendations_cleared,
                    "memory_cleared": memory_cleared,
                    "partial": partial,
                }),
                severity: AuditSeverity::Info,
                reversible: false,
                rollback_ref: None,
            })
            .ok();

        tracing::info!(
            target = "metrics",
            profile_cleared,
            interests_cleared,
            recommendations_cleared,
            memory_cleared,
            partial,
            "personalization.forget"
        );

        if partial {
            KernelResponse::Error {
                message: format!(
                    "personalization forget partially completed \
                     (profile_cleared={profile_cleared}, \
                     interests_cleared={interests_cleared}, \
                     recommendations_cleared={recommendations_cleared}, \
                     memory_cleared={memory_cleared}): {interests_note}"
                ),
            }
        } else {
            KernelResponse::Success {
                data: Some(serde_json::json!({
                    "profile_cleared": profile_cleared,
                    "interests_cleared": interests_cleared,
                    "recommendations_cleared": recommendations_cleared,
                    "memory_cleared": memory_cleared,
                    "partial": partial,
                })),
            }
        }
    }
}
