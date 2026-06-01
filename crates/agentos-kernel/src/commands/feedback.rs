//! Phase 5 — feedback-loop CLI handlers.
//!
//! Exposes `RecommendationList`, `RecommendationAccept`, and
//! `RecommendationDismiss` as `KernelCommand` implementations on `Kernel`.

use crate::kernel::Kernel;
use crate::personalization_feedback::FeedbackSignal;
use agentos_bus::KernelResponse;

impl Kernel {
    /// List recent recommendations (newest first).
    pub(crate) async fn cmd_recommendation_list(&self, limit: u32) -> KernelResponse {
        match self.recommendation_engine.store().list(limit).await {
            Ok(recs) => KernelResponse::Success {
                data: Some(serde_json::json!({ "recommendations": recs })),
            },
            Err(e) => KernelResponse::Error {
                message: e.to_string(),
            },
        }
    }

    /// Accept a recommendation: record feedback in `recommendations.db` and
    /// reinforce the originating interest topic via the `FeedbackProcessor`.
    pub(crate) async fn cmd_recommendation_accept(&self, id: String) -> KernelResponse {
        // Load the recommendation to get its basis topics.
        let rec = match self.recommendation_engine.store().get(&id).await {
            Ok(Some(r)) => r,
            Ok(None) => {
                return KernelResponse::Error {
                    message: format!("recommendation not found: {id}"),
                };
            }
            Err(e) => {
                return KernelResponse::Error {
                    message: e.to_string(),
                };
            }
        };

        // Record the acceptance in the store.
        if let Err(e) = self.recommendation_engine.feedback(&id, true).await {
            return KernelResponse::Error {
                message: format!("record feedback failed: {e}"),
            };
        }

        // Extract basis topics from the stored JSON array and apply feedback signals.
        let topics = parse_basis_topics(&rec.basis);
        let fp = std::sync::Arc::clone(&self.feedback_processor);
        for topic in topics {
            if let Err(e) = fp
                .apply(FeedbackSignal::RecommendationAccepted {
                    interest_topic: topic,
                })
                .await
            {
                tracing::warn!(error = %e, rec_id = %id, "feedback reinforce failed (accept)");
            }
        }

        KernelResponse::Success {
            data: Some(serde_json::json!({ "accepted": true })),
        }
    }

    /// Dismiss a recommendation: record feedback and apply the interest-weight
    /// penalty via the `FeedbackProcessor`.
    pub(crate) async fn cmd_recommendation_dismiss(&self, id: String) -> KernelResponse {
        // Load the recommendation to get its basis topics and dedup_hash.
        let rec = match self.recommendation_engine.store().get(&id).await {
            Ok(Some(r)) => r,
            Ok(None) => {
                return KernelResponse::Error {
                    message: format!("recommendation not found: {id}"),
                };
            }
            Err(e) => {
                return KernelResponse::Error {
                    message: e.to_string(),
                };
            }
        };

        // Record the dismissal in the store.
        if let Err(e) = self.recommendation_engine.feedback(&id, false).await {
            return KernelResponse::Error {
                message: format!("record feedback failed: {e}"),
            };
        }

        // Apply interest-weight penalties and suppress the dedup_hash.
        let topics = parse_basis_topics(&rec.basis);
        let dedup_hash = rec.dedup_hash.clone();
        let fp = std::sync::Arc::clone(&self.feedback_processor);
        for topic in topics {
            if let Err(e) = fp
                .apply(FeedbackSignal::RecommendationDismissed {
                    interest_topic: topic,
                    dedup_hash: dedup_hash.clone(),
                })
                .await
            {
                tracing::warn!(error = %e, rec_id = %id, "feedback reinforce failed (dismiss)");
            }
        }

        KernelResponse::Success {
            data: Some(serde_json::json!({ "dismissed": true })),
        }
    }
}

/// Parse the `basis` JSON string (`"[\"topic1\",\"topic2\"]"`) into a `Vec<String>`.
/// Returns an empty vec on parse failure (best-effort; feedback still records the status).
fn parse_basis_topics(basis: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(basis).unwrap_or_default()
}
