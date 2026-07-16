use super::Hook;
use crate::context::ContextManager;
use crate::scheduler::TaskScheduler;
use crate::user_pref_proposals::{
    heuristic_propose, ProposalKind, ProposalStatus, UserPrefProposal, UserPrefProposalStore,
};
use agentos_audit::{AuditEntry, AuditEventType, AuditLog, AuditSeverity};
use agentos_llm::LLMCore;
use agentos_types::{
    ContentPart, ContextCategory, ContextEntry, ContextPartition, ContextRole, ContextWindow,
    HookEvent, HookResult, TraceID,
};
use async_trait::async_trait;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct UserAdaptationHook {
    enabled: bool,
    scheduler: Arc<TaskScheduler>,
    context_manager: Arc<ContextManager>,
    proposal_store: Arc<UserPrefProposalStore>,
    active_llms: Arc<RwLock<HashMap<agentos_types::AgentID, Arc<dyn LLMCore>>>>,
    audit: Arc<AuditLog>,
    min_confidence: f32,
    max_proposals_per_task: usize,
    model: String,
}

impl UserAdaptationHook {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        enabled: bool,
        scheduler: Arc<TaskScheduler>,
        context_manager: Arc<ContextManager>,
        proposal_store: Arc<UserPrefProposalStore>,
        active_llms: Arc<RwLock<HashMap<agentos_types::AgentID, Arc<dyn LLMCore>>>>,
        audit: Arc<AuditLog>,
        min_confidence: f32,
        max_proposals_per_task: usize,
        model: String,
    ) -> Arc<Self> {
        Arc::new(Self {
            enabled,
            scheduler,
            context_manager,
            proposal_store,
            active_llms,
            audit,
            min_confidence,
            max_proposals_per_task,
            model,
        })
    }
}

#[derive(Debug, Deserialize)]
struct LlmProposal {
    content: String,
    confidence: f32,
    evidence: Vec<String>,
    #[serde(default)]
    kind: Option<String>,
}

#[async_trait]
impl Hook for UserAdaptationHook {
    fn name(&self) -> &'static str {
        "user-adaptation"
    }

    fn handles(&self, event: &HookEvent) -> bool {
        matches!(event, HookEvent::TaskEnd { success: true, .. })
    }

    async fn on_event(&self, event: &HookEvent) -> HookResult {
        if !self.enabled {
            return HookResult::Continue;
        }
        let HookEvent::TaskEnd {
            task_id,
            agent_id,
            success: _,
        } = event
        else {
            return HookResult::Continue;
        };

        let Some(task) = self.scheduler.get_task(task_id).await else {
            return HookResult::Continue;
        };
        if task.spawn_depth > 0 {
            return HookResult::Continue;
        }

        let Ok(ctx) = self.context_manager.get_context(task_id).await else {
            return HookResult::Continue;
        };

        let mut user_messages = Vec::new();
        for entry in &ctx.entries {
            if entry.role != ContextRole::User {
                continue;
            }
            for part in &entry.parts {
                if let ContentPart::Text { text } = part {
                    user_messages.push(text.clone());
                }
            }
        }

        let llm = self.active_llms.read().await.get(agent_id).cloned();
        let mut proposals = if let Some(llm) = llm {
            self.try_llm_propose(*task_id, *agent_id, &user_messages, llm)
                .await
                .unwrap_or_else(|| {
                    heuristic_propose(
                        *task_id,
                        *agent_id,
                        &user_messages,
                        self.max_proposals_per_task,
                    )
                })
        } else {
            heuristic_propose(
                *task_id,
                *agent_id,
                &user_messages,
                self.max_proposals_per_task,
            )
        };
        proposals.retain(|p| p.confidence >= self.min_confidence);
        if proposals.is_empty() {
            return HookResult::Continue;
        }

        match self.proposal_store.insert_many(&proposals).await {
            Ok(outcome) => {
                for p in &outcome.inserted {
                    self.audit
                        .append(AuditEntry {
                            timestamp: chrono::Utc::now(),
                            trace_id: TraceID::new(),
                            event_type: AuditEventType::ProposalCreated,
                            agent_id: Some(p.agent_id),
                            task_id: Some(p.task_id),
                            tool_id: None,
                            details: serde_json::json!({
                                "proposal_id": p.id,
                                "confidence": p.confidence,
                                "kind": p.kind,
                            }),
                            severity: AuditSeverity::Info,
                            reversible: false,
                            rollback_ref: None,
                        })
                        .ok();
                }
                if outcome.rejected > 0 {
                    tracing::debug!(
                        rejected = outcome.rejected,
                        "user-adaptation: dropped proposals failing store invariants",
                    );
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "user-adaptation insert proposals failed");
            }
        }

        HookResult::Continue
    }
}

impl UserAdaptationHook {
    async fn try_llm_propose(
        &self,
        task_id: agentos_types::TaskID,
        agent_id: agentos_types::AgentID,
        user_messages: &[String],
        llm: Arc<dyn LLMCore>,
    ) -> Option<Vec<UserPrefProposal>> {
        let mut ctx = ContextWindow::new(64);
        ctx.push(ContextEntry {
            role: ContextRole::System,
            parts: vec![ContentPart::Text {
                text: format!(
                    "You extract stable user preferences from chat messages.
Return STRICT JSON only: an array of objects with fields:
content (string), confidence (0..1), evidence (array of short message quotes), kind ('add'|'replace').
Rules:
- only durable preferences (tone, verbosity, formatting, recurring workflow constraints)
- ignore one-off task specifics
- max {} proposals
- confidence under 0.5 should be omitted",
                    self.max_proposals_per_task
                ),
            }],
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 1.0,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::default(),
            category: ContextCategory::System,
            is_summary: false,
        });
        ctx.push(ContextEntry {
            role: ContextRole::User,
            parts: vec![ContentPart::Text {
                text: format!(
                    "Configured proposer model hint: {}\nMessages:\n{}",
                    self.model,
                    user_messages
                        .iter()
                        .rev()
                        .take(20)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("\n---\n")
                ),
            }],
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 0.7,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::default(),
            category: ContextCategory::Task,
            is_summary: false,
        });

        let out = llm.infer(&ctx).await.ok()?;
        let parsed: Vec<LlmProposal> = serde_json::from_str(out.text.trim())
            .or_else(|_| {
                let s = out.text.trim();
                let l = s.find('[').ok_or_else(|| {
                    serde_json::Error::io(std::io::Error::other("no array start"))
                })?;
                let r = s
                    .rfind(']')
                    .ok_or_else(|| serde_json::Error::io(std::io::Error::other("no array end")))?;
                serde_json::from_str(&s[l..=r])
            })
            .ok()?;

        let mut rows = Vec::new();
        for p in parsed.into_iter().take(self.max_proposals_per_task) {
            if p.content.trim().is_empty() {
                continue;
            }
            rows.push(UserPrefProposal {
                id: uuid::Uuid::new_v4().to_string(),
                task_id,
                agent_id,
                kind: if p.kind.as_deref() == Some("replace") {
                    ProposalKind::Replace
                } else {
                    ProposalKind::Add
                },
                content: p.content.trim().to_string(),
                confidence: p.confidence.clamp(0.0, 1.0),
                evidence: p.evidence.into_iter().take(3).collect(),
                replaces: None,
                status: ProposalStatus::Pending,
                created_at: chrono::Utc::now(),
                reviewed_at: None,
            });
        }
        Some(rows)
    }
}
