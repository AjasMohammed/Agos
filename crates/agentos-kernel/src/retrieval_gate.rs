use crate::tool_registry::ToolRegistry;
use agentos_memory::{EpisodicStore, ProceduralStore, SemanticStore};
use agentos_types::AgentID;
use std::collections::{BTreeMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IndexType {
    Semantic,
    Episodic,
    Procedural,
    Tools,
}

impl std::fmt::Display for IndexType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IndexType::Semantic => write!(f, "semantic"),
            IndexType::Episodic => write!(f, "episodic"),
            IndexType::Procedural => write!(f, "procedural"),
            IndexType::Tools => write!(f, "tools"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct IndexQuery {
    pub index: IndexType,
    pub top_k: usize,
    pub query: String,
}

#[derive(Debug, Clone)]
pub struct RetrievalPlan {
    pub queries: Vec<IndexQuery>,
}

impl RetrievalPlan {
    pub fn empty() -> Self {
        Self {
            queries: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.queries.is_empty()
    }

    pub fn total_top_k(&self) -> usize {
        self.queries.iter().map(|q| q.top_k).sum()
    }
}

#[derive(Debug, Clone)]
pub struct RetrievalResult {
    pub source: IndexType,
    pub content: String,
    pub score: f32,
    pub metadata: Option<serde_json::Value>,
}

impl RetrievalResult {
    pub fn content_hash(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.content.hash(&mut hasher);
        hasher.finish()
    }
}

pub struct RetrievalGate {
    default_top_k: usize,
}

impl RetrievalGate {
    pub fn new(default_top_k: usize) -> Self {
        Self { default_top_k }
    }

    pub fn classify(&self, query: &str) -> RetrievalPlan {
        let lower = query.to_lowercase();
        let words: Vec<&str> = lower.split_whitespace().collect();
        if Self::is_trivial(&lower, &words) {
            return RetrievalPlan::empty();
        }

        let mut queries = Vec::new();
        let mut seen = HashSet::new();

        if Self::has_episodic_signal(&lower) {
            queries.push(IndexQuery {
                index: IndexType::Episodic,
                top_k: self.default_top_k,
                query: query.to_string(),
            });
            seen.insert(IndexType::Episodic);
        }

        if Self::has_procedural_signal(&lower) {
            queries.push(IndexQuery {
                index: IndexType::Procedural,
                top_k: 3,
                query: query.to_string(),
            });
            seen.insert(IndexType::Procedural);
        }

        if Self::has_tool_signal(&lower) {
            queries.push(IndexQuery {
                index: IndexType::Tools,
                top_k: self.default_top_k,
                query: query.to_string(),
            });
            seen.insert(IndexType::Tools);
        }

        if Self::has_factual_signal(&lower) && !seen.contains(&IndexType::Semantic) {
            queries.push(IndexQuery {
                index: IndexType::Semantic,
                top_k: self.default_top_k,
                query: query.to_string(),
            });
            seen.insert(IndexType::Semantic);
        }

        if queries.is_empty() {
            queries.push(IndexQuery {
                index: IndexType::Semantic,
                top_k: self.default_top_k,
                query: query.to_string(),
            });
        }

        RetrievalPlan { queries }
    }

    fn is_trivial(lower: &str, words: &[&str]) -> bool {
        const TRIVIAL_WORDS: &[&str] = &[
            "ok", "okay", "yes", "no", "sure", "thanks", "thank", "done", "next", "continue",
            "stop", "cancel", "quit", "exit", "help", "got", "it", "right", "fine", "good",
            "great", "cool", "yep", "nope", "y", "n",
        ];

        if words.is_empty() {
            return true;
        }
        if words.len() <= 3 && words.iter().all(|w| TRIVIAL_WORDS.contains(w)) {
            return true;
        }

        const TRIVIAL_PHRASES: &[&str] = &[
            "got it",
            "sounds good",
            "go ahead",
            "do it",
            "that works",
            "looks good",
            "makes sense",
        ];
        lower.len() < 30 && TRIVIAL_PHRASES.iter().any(|p| lower.contains(p))
    }

    fn has_episodic_signal(lower: &str) -> bool {
        const SIGNALS: &[&str] = &[
            "remember",
            "last time",
            "previously",
            "earlier",
            "before",
            "what happened",
            "history",
            "recall",
            "when did",
            "past",
            "yesterday",
            "last week",
            "ago",
            "recent",
            "tried before",
            "we did",
            "you did",
            "i asked",
        ];
        SIGNALS.iter().any(|s| lower.contains(s))
    }

    fn has_procedural_signal(lower: &str) -> bool {
        const SIGNALS: &[&str] = &[
            "how to",
            "how do",
            "steps to",
            "procedure for",
            "process for",
            "workflow",
            "best way to",
            "instructions for",
            "guide for",
            "walk me through",
            "step by step",
            "recipe for",
            "method for",
            "best practice",
            "standard operating",
        ];
        SIGNALS.iter().any(|s| lower.contains(s))
    }

    fn has_tool_signal(lower: &str) -> bool {
        const SIGNALS: &[&str] = &[
            "find tool",
            "search tool",
            "need a tool",
            "which tool",
            "available tool",
            "tool for",
            "capability",
            "what tools",
            "list tools",
        ];
        SIGNALS.iter().any(|s| lower.contains(s))
    }

    fn has_factual_signal(lower: &str) -> bool {
        const SIGNALS: &[&str] = &[
            "what is",
            "what are",
            "who is",
            "where is",
            "define",
            "explain",
            "describe",
            "tell me about",
            "difference between",
            "compare",
            "summarize",
        ];
        SIGNALS.iter().any(|s| lower.contains(s))
    }
}

pub struct RetrievalExecutor {
    semantic: Arc<SemanticStore>,
    episodic: Arc<EpisodicStore>,
    procedural: Arc<ProceduralStore>,
    tool_registry: Arc<RwLock<ToolRegistry>>,
}

impl RetrievalExecutor {
    pub fn new(
        semantic: Arc<SemanticStore>,
        episodic: Arc<EpisodicStore>,
        procedural: Arc<ProceduralStore>,
        tool_registry: Arc<RwLock<ToolRegistry>>,
    ) -> Self {
        Self {
            semantic,
            episodic,
            procedural,
            tool_registry,
        }
    }

    pub async fn execute(
        &self,
        plan: &RetrievalPlan,
        agent_id: Option<&AgentID>,
    ) -> Vec<RetrievalResult> {
        if plan.is_empty() {
            return Vec::new();
        }

        let mut handles: Vec<tokio::task::JoinHandle<Vec<RetrievalResult>>> = Vec::new();
        for query in &plan.queries {
            match query.index {
                IndexType::Semantic => {
                    let store = self.semantic.clone();
                    let q = query.query.clone();
                    let top_k = query.top_k;
                    let aid = agent_id.copied();
                    handles.push(tokio::spawn(async move {
                        match store.search(&q, aid.as_ref(), top_k, 0.0).await {
                            Ok(results) => results
                                .into_iter()
                                .map(|r| RetrievalResult {
                                    source: IndexType::Semantic,
                                    content: r.chunk.content,
                                    score: r.rrf_score,
                                    metadata: Some(serde_json::json!({
                                        "key": r.entry.key,
                                        "semantic_score": r.semantic_score,
                                        "fts_score": r.fts_score,
                                    })),
                                })
                                .collect(),
                            Err(_) => Vec::new(),
                        }
                    }));
                }
                IndexType::Episodic => {
                    let store = self.episodic.clone();
                    let q = query.query.clone();
                    let top_k = query.top_k as u32;
                    let aid = agent_id.copied();
                    handles.push(tokio::spawn(async move {
                        let result = tokio::task::spawn_blocking(move || {
                            store.search_events(&q, None, aid.as_ref(), top_k)
                        })
                        .await;
                        match result {
                            Ok(Ok(episodes)) => episodes
                                .into_iter()
                                .map(|ep| RetrievalResult {
                                    source: IndexType::Episodic,
                                    content: ep.summary.unwrap_or(ep.content),
                                    score: 0.5,
                                    metadata: Some(serde_json::json!({
                                        "episode_type": ep.entry_type.as_str(),
                                        "timestamp": ep.timestamp.to_rfc3339(),
                                    })),
                                })
                                .collect(),
                            _ => Vec::new(),
                        }
                    }));
                }
                IndexType::Procedural => {
                    let store = self.procedural.clone();
                    let q = query.query.clone();
                    let top_k = query.top_k;
                    let aid = agent_id.copied();
                    handles.push(tokio::spawn(async move {
                        match store.search(&q, aid.as_ref(), top_k, 0.0).await {
                            Ok(results) => results
                                .into_iter()
                                .map(|r| {
                                    let steps = r
                                        .procedure
                                        .steps
                                        .iter()
                                        .take(4)
                                        .map(|s| format!("{}: {}", s.order + 1, s.action))
                                        .collect::<Vec<_>>()
                                        .join("; ");
                                    RetrievalResult {
                                        source: IndexType::Procedural,
                                        content: format!(
                                            "Procedure: {}\n{}\nSteps: {}",
                                            r.procedure.name, r.procedure.description, steps
                                        ),
                                        score: r.rrf_score,
                                        metadata: Some(serde_json::json!({
                                            "name": r.procedure.name,
                                            "success_count": r.procedure.success_count,
                                            "failure_count": r.procedure.failure_count,
                                        })),
                                    }
                                })
                                .collect(),
                            Err(_) => Vec::new(),
                        }
                    }));
                }
                IndexType::Tools => {
                    let registry = self.tool_registry.clone();
                    let q = query.query.clone();
                    let top_k = query.top_k;
                    handles.push(tokio::spawn(async move {
                        let lower = q.to_lowercase();
                        let words: Vec<&str> = lower.split_whitespace().collect();
                        let reg = registry.read().await;
                        let mut results: Vec<RetrievalResult> = reg
                            .list_all()
                            .into_iter()
                            .filter_map(|tool| {
                                let name = tool.manifest.manifest.name.to_lowercase();
                                let desc = tool.manifest.manifest.description.to_lowercase();
                                let hits = words
                                    .iter()
                                    .filter(|w| name.contains(**w) || desc.contains(**w))
                                    .count();
                                if hits == 0 {
                                    return None;
                                }
                                Some(RetrievalResult {
                                    source: IndexType::Tools,
                                    content: format!(
                                        "{}: {}",
                                        tool.manifest.manifest.name,
                                        tool.manifest.manifest.description
                                    ),
                                    score: hits as f32 / words.len().max(1) as f32,
                                    metadata: Some(serde_json::json!({
                                        "tool_name": tool.manifest.manifest.name
                                    })),
                                })
                            })
                            .collect();
                        results.sort_by(|a, b| {
                            b.score
                                .partial_cmp(&a.score)
                                .unwrap_or(std::cmp::Ordering::Equal)
                        });
                        results.truncate(top_k);
                        results
                    }));
                }
            }
        }

        let mut merged = Vec::new();
        for handle in handles {
            if let Ok(results) = handle.await {
                merged.extend(results);
            }
        }

        merged.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let mut seen = HashSet::new();
        merged
            .into_iter()
            .filter(|r| seen.insert(r.content_hash()))
            .collect()
    }

    pub fn format_as_knowledge_blocks(results: &[RetrievalResult]) -> Vec<String> {
        if results.is_empty() {
            return Vec::new();
        }
        let mut grouped: BTreeMap<String, Vec<&RetrievalResult>> = BTreeMap::new();
        for r in results {
            grouped.entry(r.source.to_string()).or_default().push(r);
        }

        grouped
            .into_iter()
            .map(|(source, items)| {
                let tag = source.to_uppercase();
                let mut block = format!("[RETRIEVED_{}]\n", tag);
                for item in items {
                    block.push_str("- ");
                    block.push_str(&item.content);
                    block.push('\n');
                }
                block.push_str(&format!("[/RETRIEVED_{}]", tag));
                block
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_trivial_query_returns_empty_plan() {
        let gate = RetrievalGate::new(5);
        let plan = gate.classify("ok thanks");
        assert!(plan.is_empty());
    }

    #[test]
    fn classify_procedural_query_hits_procedural_index() {
        let gate = RetrievalGate::new(5);
        let plan = gate.classify("How to deploy this service step by step?");
        assert!(!plan.is_empty());
        assert!(plan
            .queries
            .iter()
            .any(|q| q.index == IndexType::Procedural));
    }

    #[test]
    fn format_groups_by_source() {
        let blocks = RetrievalExecutor::format_as_knowledge_blocks(&[
            RetrievalResult {
                source: IndexType::Semantic,
                content: "fact-a".to_string(),
                score: 0.9,
                metadata: None,
            },
            RetrievalResult {
                source: IndexType::Episodic,
                content: "event-a".to_string(),
                score: 0.7,
                metadata: None,
            },
        ]);
        assert_eq!(blocks.len(), 2);
        assert!(blocks.iter().any(|b| b.contains("RETRIEVED_SEMANTIC")));
        assert!(blocks.iter().any(|b| b.contains("RETRIEVED_EPISODIC")));
    }
}
