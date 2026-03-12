use agentos_memory::types::{Procedure, ProcedureStep};
use agentos_memory::{EpisodicEntry, EpisodicStore, ProceduralStore};
use agentos_types::AgentOSError;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
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
    pub failed: usize,
}

pub struct ConsolidationEngine {
    episodic_store: Arc<EpisodicStore>,
    procedural_store: Arc<ProceduralStore>,
    config: ConsolidationConfig,
    task_completions_since_last: AtomicU64,
    last_run: RwLock<DateTime<Utc>>,
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
        }
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

        let since = *self.last_run.read().await;
        let episodes = self
            .episodic_store
            .find_successful_episodes(Some(since), self.config.max_episodes_per_cycle)?;
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
            let procedure = distill_group_to_procedure(&group);
            match self
                .procedural_store
                .search(&procedure.name, None, 1, 0.0)
                .await
            {
                Ok(existing) if !existing.is_empty() && existing[0].rrf_score > 0.90 => {
                    report.skipped_existing += 1;
                    continue;
                }
                Ok(_) => {}
                Err(_) => {}
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

fn tokenize(text: &str) -> HashSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2)
        .map(|w| w.to_string())
        .collect()
}

fn cluster_by_keywords(
    episodes: Vec<EpisodicEntry>,
    min_occurrences: usize,
) -> Vec<Vec<EpisodicEntry>> {
    let mut groups: HashMap<String, Vec<EpisodicEntry>> = HashMap::new();
    for ep in episodes {
        let text = ep.summary.clone().unwrap_or(ep.content.clone());
        let tokens = tokenize(&text);
        let mut key_parts = tokens.into_iter().take(4).collect::<Vec<_>>();
        key_parts.sort();
        let key = key_parts.join("|");
        groups.entry(key).or_default().push(ep);
    }

    groups
        .into_values()
        .filter(|g| g.len() >= min_occurrences)
        .collect()
}

fn distill_group_to_procedure(group: &[EpisodicEntry]) -> Procedure {
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
        steps.push(ProcedureStep {
            order: 0,
            action: "Follow the repeated successful approach from prior tasks".to_string(),
            tool: None,
            expected_outcome: Some("Task completes successfully".to_string()),
        });
    }

    Procedure {
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
    }
}
