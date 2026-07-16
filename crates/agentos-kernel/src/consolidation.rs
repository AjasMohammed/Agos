use agentos_memory::types::{Procedure, ProcedureSearchResult, ProcedureStep};
use agentos_memory::{EpisodicEntry, EpisodicStore, ProceduralStore};
use agentos_types::AgentOSError;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidationConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_min_occurrences")]
    pub min_pattern_occurrences: usize,
    #[serde(default = "default_task_trigger")]
    pub task_completions_trigger: u64,
    #[serde(default = "default_time_trigger")]
    pub time_trigger_hours: u64,
    #[serde(default = "default_max_episodes")]
    pub max_episodes_per_cycle: u32,
}

fn default_enabled() -> bool {
    true
}
fn default_min_occurrences() -> usize {
    3
}
fn default_task_trigger() -> u64 {
    100
}
fn default_time_trigger() -> u64 {
    24
}
fn default_max_episodes() -> u32 {
    500
}

impl Default for ConsolidationConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            min_pattern_occurrences: default_min_occurrences(),
            task_completions_trigger: default_task_trigger(),
            time_trigger_hours: default_time_trigger(),
            max_episodes_per_cycle: default_max_episodes(),
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct ConsolidationReport {
    pub patterns_found: usize,
    pub created: usize,
    pub skipped_existing: usize,
    pub skipped_low_information: usize,
    pub failed: usize,
}

pub struct ConsolidationEngine {
    episodic_store: Arc<EpisodicStore>,
    procedural_store: Arc<ProceduralStore>,
    config: ConsolidationConfig,
    task_completions_since_last: AtomicU64,
    last_run: RwLock<DateTime<Utc>>,
    /// Serializes concurrent `run_cycle` calls so the background loop and
    /// `on_task_completed` cannot race on `last_run` / the procedural store.
    cycle_lock: tokio::sync::Mutex<()>,
}

impl ConsolidationEngine {
    pub fn new(
        episodic_store: Arc<EpisodicStore>,
        procedural_store: Arc<ProceduralStore>,
        config: ConsolidationConfig,
    ) -> Self {
        Self {
            episodic_store,
            procedural_store,
            config,
            task_completions_since_last: AtomicU64::new(0),
            last_run: RwLock::new(Utc::now()),
            cycle_lock: tokio::sync::Mutex::new(()),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    pub async fn on_task_completed(&self) {
        if !self.config.enabled {
            return;
        }

        let count = self
            .task_completions_since_last
            .fetch_add(1, Ordering::Relaxed)
            + 1;
        let should_run = if count >= self.config.task_completions_trigger {
            true
        } else {
            let last = *self.last_run.read().await;
            let hours_since = (Utc::now() - last).num_hours().max(0) as u64;
            hours_since >= self.config.time_trigger_hours
        };
        if should_run {
            let _ = self.run_cycle().await;
        }
    }

    pub async fn run_cycle(&self) -> Result<ConsolidationReport, AgentOSError> {
        if !self.config.enabled {
            return Ok(ConsolidationReport::default());
        }
        // Serialize concurrent callers (background loop + on_task_completed).
        let _guard = self.cycle_lock.lock().await;

        let since = *self.last_run.read().await;
        let episodes = self
            .episodic_store
            .find_successful_episodes(Some(since), self.config.max_episodes_per_cycle)
            .await?;
        if episodes.len() < self.config.min_pattern_occurrences {
            *self.last_run.write().await = Utc::now();
            self.task_completions_since_last.store(0, Ordering::Relaxed);
            return Ok(ConsolidationReport::default());
        }

        let patterns = cluster_by_keywords(episodes, self.config.min_pattern_occurrences);
        let mut report = ConsolidationReport {
            patterns_found: patterns.len(),
            ..Default::default()
        };

        for group in patterns {
            let Some(procedure) = distill_group_to_procedure(&group) else {
                report.skipped_low_information += 1;
                continue;
            };
            match self
                .procedural_store
                .search(&procedure.name, None, 1, 0.0)
                .await
            {
                Ok(existing)
                    if existing
                        .first()
                        .is_some_and(|e| is_existing_duplicate(e, &procedure.name)) =>
                {
                    report.skipped_existing += 1;
                    continue;
                }
                Ok(_) => {}
                Err(_) => {
                    // Skip storing on dedup search failure to avoid creating duplicates.
                    report.failed += 1;
                    continue;
                }
            }

            match self.procedural_store.store(&procedure).await {
                Ok(_) => report.created += 1,
                Err(_) => report.failed += 1,
            }
        }

        *self.last_run.write().await = Utc::now();
        self.task_completions_since_last.store(0, Ordering::Relaxed);
        Ok(report)
    }
}

/// True when the closest existing procedure should suppress storing a new one
/// with `candidate_name`. Exact name matches always dedup; otherwise require
/// near-identical embedding similarity. Compare against `semantic_score` (raw
/// cosine), NOT `rrf_score`: the hybrid score is `0.7·cosine + 0.3·fts_norm`
/// whenever FTS matches, which caps it near ~0.74 even for an exact duplicate,
/// so a 0.9 gate on it can never fire for textually identical names.
fn is_existing_duplicate(existing: &ProcedureSearchResult, candidate_name: &str) -> bool {
    existing.procedure.name == candidate_name || existing.semantic_score > 0.90
}

/// Sorted, deduplicated tokens. Deterministic order so the cluster key and the
/// procedure name agree across runs and across word-order permutations.
fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2)
        .map(|w| w.to_string())
        .collect::<BTreeSet<String>>()
        .into_iter()
        .collect()
}

fn cluster_by_keywords(
    episodes: Vec<EpisodicEntry>,
    min_occurrences: usize,
) -> Vec<Vec<EpisodicEntry>> {
    let mut groups: HashMap<String, Vec<EpisodicEntry>> = HashMap::new();
    for ep in episodes {
        let text = ep.summary.clone().unwrap_or(ep.content.clone());
        let key = tokenize(&text)
            .into_iter()
            .take(4)
            .collect::<Vec<_>>()
            .join("|");
        groups.entry(key).or_default().push(ep);
    }

    groups
        .into_values()
        .filter(|g| g.len() >= min_occurrences)
        .collect()
}

/// Returns `None` when the group carries too little information to be a useful
/// procedure (no tool metadata to derive steps from) — storing a generic
/// one-step "follow the prior approach" SOP only pollutes retrieval.
fn distill_group_to_procedure(group: &[EpisodicEntry]) -> Option<Procedure> {
    let first = &group[0];
    let text = first.summary.clone().unwrap_or(first.content.clone());
    let title_tokens = tokenize(&text).into_iter().take(3).collect::<Vec<_>>();
    let name = if title_tokens.is_empty() {
        "consolidated-procedure".to_string()
    } else {
        title_tokens.join("-")
    };

    let mut tools = HashSet::new();
    for ep in group {
        if let Some(meta) = &ep.metadata {
            if let Some(tool) = meta.get("tool").and_then(|v| v.as_str()) {
                tools.insert(tool.to_string());
            }
        }
    }

    let mut steps = Vec::new();
    for (idx, tool) in tools.into_iter().take(5).enumerate() {
        steps.push(ProcedureStep {
            order: idx,
            action: format!("Use '{}' as part of the workflow", tool),
            tool: Some(tool),
            expected_outcome: Some("Step completed".to_string()),
        });
    }
    if steps.is_empty() {
        return None;
    }

    Some(Procedure {
        id: String::new(),
        name,
        description: format!("Auto-consolidated from {} successful episodes", group.len()),
        preconditions: Vec::new(),
        steps,
        postconditions: vec!["Successful task completion".to_string()],
        success_count: group.len() as u32,
        failure_count: 0,
        source_episodes: group.iter().map(|e| e.id.to_string()).collect(),
        agent_id: None,
        tags: vec!["auto-consolidated".to_string()],
        created_at: Utc::now(),
        updated_at: Utc::now(),
        last_used_at: None,
        use_count: 0,
        confidence: agentos_memory::types::default_confidence(),
        status: agentos_memory::MemoryStatus::Active,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_memory::types::EpisodeType;
    use agentos_types::{AgentID, TaskID, TraceID};

    fn episode(summary: &str, metadata: Option<serde_json::Value>) -> EpisodicEntry {
        EpisodicEntry {
            id: 0,
            task_id: TaskID::new(),
            agent_id: AgentID::new(),
            entry_type: EpisodeType::SystemEvent,
            content: summary.to_string(),
            summary: Some(summary.to_string()),
            metadata,
            timestamp: Utc::now(),
            trace_id: TraceID::new(),
        }
    }

    fn search_result(name: &str, semantic_score: f32, rrf_score: f32) -> ProcedureSearchResult {
        let group = vec![episode(
            name,
            Some(serde_json::json!({"tool": "file-read"})),
        )];
        let mut procedure = distill_group_to_procedure(&group).unwrap();
        procedure.name = name.to_string();
        ProcedureSearchResult {
            procedure,
            semantic_score,
            fts_score: 5.0,
            rrf_score,
        }
    }

    #[test]
    fn tokenize_is_sorted_and_deduped() {
        assert_eq!(
            tokenize("Task completed successfully task"),
            vec!["completed", "successfully", "task"]
        );
    }

    #[test]
    fn word_order_permutations_produce_identical_names() {
        let meta = Some(serde_json::json!({"tool": "file-read"}));
        let a = distill_group_to_procedure(&[episode("task completed successfully", meta.clone())])
            .unwrap();
        let b = distill_group_to_procedure(&[episode("completed task successfully", meta.clone())])
            .unwrap();
        let c =
            distill_group_to_procedure(&[episode("successfully completed task", meta)]).unwrap();
        assert_eq!(a.name, b.name);
        assert_eq!(b.name, c.name);
    }

    #[test]
    fn permuted_summaries_cluster_together() {
        let eps = vec![
            episode("task completed successfully", None),
            episode("completed task successfully", None),
            episode("successfully completed task", None),
        ];
        let groups = cluster_by_keywords(eps, 3);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 3);
    }

    #[test]
    fn low_information_group_is_rejected() {
        let group = vec![episode("task completed successfully", None); 3];
        assert!(distill_group_to_procedure(&group).is_none());
    }

    #[test]
    fn group_with_tool_metadata_distills_steps() {
        let meta = Some(serde_json::json!({"tool": "file-read"}));
        let group = vec![
            episode("read config file", meta.clone()),
            episode("read config file", meta),
        ];
        let procedure = distill_group_to_procedure(&group).unwrap();
        assert_eq!(procedure.steps.len(), 1);
        assert_eq!(procedure.steps[0].tool.as_deref(), Some("file-read"));
    }

    #[test]
    fn exact_name_match_dedups_despite_capped_hybrid_score() {
        // The hybrid rrf_score caps near ~0.74 when FTS matches — the old
        // `rrf_score > 0.90` gate could never fire for an identical name.
        let existing = search_result("completed-successfully-task", 1.0, 0.73);
        assert!(is_existing_duplicate(
            &existing,
            "completed-successfully-task"
        ));
    }

    #[test]
    fn near_identical_embedding_dedups_without_name_match() {
        let existing = search_result("completed-successfully-task", 0.95, 0.69);
        assert!(is_existing_duplicate(&existing, "task-finished-cleanly"));
    }

    #[test]
    fn dissimilar_procedure_is_not_deduped() {
        let existing = search_result("completed-successfully-task", 0.42, 0.31);
        assert!(!is_existing_duplicate(&existing, "deploy-staging-rollout"));
    }
}
