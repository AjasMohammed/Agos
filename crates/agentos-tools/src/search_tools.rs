use crate::agent_manual::{SharedToolSummaries, ToolSummary};
use crate::tool_search_index::ToolSearchIndex;
use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_memory::Embedder;
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub struct SearchToolsTool {
    tool_summaries: SharedToolSummaries,
    /// Semantic index over the catalogue. Self-refreshing; fail-open to the
    /// keyword scorer when the embedder is unavailable.
    index: Arc<ToolSearchIndex>,
}

impl SearchToolsTool {
    pub fn new(tool_summaries: SharedToolSummaries, embedder: Arc<Embedder>) -> Self {
        Self {
            tool_summaries,
            index: Arc::new(ToolSearchIndex::new(embedder)),
        }
    }

    /// Share an existing index (the kernel's admission ranker owns one) so the
    /// catalogue is embedded once per process, not once per index.
    pub fn with_index(tool_summaries: SharedToolSummaries, index: Arc<ToolSearchIndex>) -> Self {
        Self {
            tool_summaries,
            index,
        }
    }

    /// Score a tool against a query string by lexical overlap.
    /// Returns a score where higher is more relevant. Exposed `pub` so the
    /// Phase-6 eval harness can compute a clean lexical baseline for Δ metrics.
    pub fn score_tool(name: &str, description: &str, tags: &[String], query_lower: &str) -> i32 {
        let mut score: i32 = 0;
        let query_tokens: Vec<&str> = query_lower.split_whitespace().collect();

        // exact name match
        if name == query_lower {
            score += 10;
        }
        // name contains query
        if name.contains(query_lower) {
            score += 5;
        }
        // description match (case-insensitive already lowercase)
        if description.to_lowercase().contains(query_lower) {
            score += 2;
        }
        // each query token that overlaps with tags
        for token in &query_tokens {
            if tags.iter().any(|t| t.to_lowercase().contains(token)) {
                score += 1;
            }
        }
        score
    }
}

/// Lexical score over the full index text (hints, category, parameter names
/// and descriptions), not just name/description/tags. Same evidence the
/// embedding leg sees. Keeps `score_tool`'s exact-name boosts.
pub fn score_summary(s: &ToolSummary, query_lower: &str) -> i32 {
    let mut score = SearchToolsTool::score_tool(&s.name, &s.description, &s.tags, query_lower);
    let text = crate::tool_search_index::index_text(s).to_lowercase();
    for token in query_lower.split_whitespace().filter(|t| t.len() >= 3) {
        if text.contains(token) {
            score += 1;
        }
    }
    score
}

/// Reciprocal-rank fusion of the semantic and keyword rankings (both
/// best-first). `k = 60` is the standard RRF constant. Returns
/// `(name, fused_score, source)` best-first, capped at `top_k`; `source`
/// names the leg that ranked the tool higher. An empty semantic list (no-op
/// embedder) degrades to the keyword order unchanged.
pub fn rrf_merge(
    semantic_names: &[String],
    keyword_names: &[String],
    top_k: usize,
) -> Vec<(String, f32, &'static str)> {
    const K: f32 = 60.0;
    let mut fused: HashMap<&str, (f32, usize, usize)> = HashMap::new(); // score, sem_rank, kw_rank
    for (i, n) in semantic_names.iter().enumerate() {
        let e = fused
            .entry(n.as_str())
            .or_insert((0.0, usize::MAX, usize::MAX));
        e.0 += 1.0 / (K + i as f32 + 1.0);
        e.1 = i;
    }
    for (i, n) in keyword_names.iter().enumerate() {
        let e = fused
            .entry(n.as_str())
            .or_insert((0.0, usize::MAX, usize::MAX));
        e.0 += 1.0 / (K + i as f32 + 1.0);
        e.2 = i;
    }
    let mut out: Vec<(String, f32, &'static str)> = fused
        .into_iter()
        .map(|(n, (sc, sr, kr))| {
            (
                n.to_string(),
                sc,
                if sr <= kr { "semantic" } else { "keyword" },
            )
        })
        .collect();
    out.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    out.truncate(top_k);
    out
}

/// Per-agent usage scores for the ranking prior. Empty when the store is
/// absent (fresh install) — the prior is then a no-op.
async fn agentos_tools_usage(context: &ToolExecutionContext) -> HashMap<String, f64> {
    crate::agent_manual::AgentManualTool::load_usage_scores_async(
        context.data_dir.clone(),
        context.agent_id,
    )
    .await
}

#[async_trait]
impl AgentTool for SearchToolsTool {
    fn name(&self) -> &str {
        "search-tools"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let query = payload
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("search-tools requires 'query'".into())
            })?;
        let top_k =
            (payload.get("top_k").and_then(|v| v.as_u64()).unwrap_or(5) as usize).clamp(1, 20);

        // Drop tools the agent holds no permission for, before either ranking
        // leg sees them: a search hit the agent cannot call wastes a turn and
        // (with `rearm_on_describe`) arms a native schema for nothing.
        // Visibility only — call-time enforcement is unchanged.
        // The FULL catalogue goes to the index so its content signature is
        // stable across agents (a per-permission subset would re-embed the
        // corpus on every alternating caller); permission + category
        // visibility is applied as an `allowed` set on both legs instead.
        let summaries: Vec<ToolSummary> = self.tool_summaries.read().await.clone();

        let query_lower = query.to_lowercase();

        // Task-scoped allowlist: hide tools whose category is not in the active
        // list. `None` = no restriction.
        let allowlist = context.tool_categories.as_ref();
        let cat_allowed = |category: &str| {
            allowlist.is_none_or(|al| al.iter().any(|c| c.eq_ignore_ascii_case(category)))
        };
        let permitted: HashSet<String> = summaries
            .iter()
            .filter(|s| {
                cat_allowed(&s.category)
                    && agentos_capability::any_permission_granted(
                        &context.permissions,
                        &s.permissions,
                    )
            })
            .map(|s| s.name.clone())
            .collect();

        // 1. Keyword scoring (substring/token overlap), descending score then name.
        let mut keyword: Vec<(i32, String)> = summaries
            .iter()
            .filter(|s| permitted.contains(&s.name))
            .map(|s| (score_summary(s, &query_lower), s.name.clone()))
            .filter(|(score, _)| *score > 0)
            .collect();
        keyword.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        let keyword_score: HashMap<String, i32> =
            keyword.iter().map(|(s, n)| (n.clone(), *s)).collect();
        let keyword_names: Vec<String> = keyword.into_iter().map(|(_, n)| n).collect();

        // 2. Semantic ranking (embedding cosine), restricted to the same set.
        let semantic = self
            .index
            .semantic_rank(&summaries, query, Some(&permitted), top_k * 2)
            .await;
        let semantic_score: HashMap<String, f32> = semantic.iter().cloned().collect();
        let semantic_names: Vec<String> = semantic.into_iter().map(|(n, _)| n).collect();

        // 3. Fuse both legs with RRF, then add a small per-agent usage prior so
        // the tool this agent habitually uses outranks a near-duplicate sibling.
        let mut order = rrf_merge(&semantic_names, &keyword_names, top_k * 2);
        let usage = agentos_tools_usage(&context).await;
        if !usage.is_empty() {
            for (name, score, _) in order.iter_mut() {
                if let Some(u) = usage.get(name) {
                    *score += 0.05 * (1.0 + *u as f32).ln();
                }
            }
            order.sort_by(|a, b| {
                b.1.partial_cmp(&a.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.0.cmp(&b.0))
            });
        }
        order.truncate(top_k);
        let desc_by_name: HashMap<&str, &str> = summaries
            .iter()
            .map(|s| (s.name.as_str(), s.description.as_str()))
            .collect();

        let matches: Vec<serde_json::Value> = order
            .iter()
            .map(|(name, fused, source)| {
                let description = desc_by_name.get(name.as_str()).copied().unwrap_or("");
                // Always a float so consumers see one type per row.
                let score = if *source == "semantic" {
                    json!(semantic_score.get(name).copied().unwrap_or(0.0))
                } else {
                    json!(keyword_score.get(name).copied().unwrap_or(0) as f32)
                };
                json!({
                    "name": name,
                    "description": description,
                    "score": score,
                    "fused": fused,
                    "match": source,
                })
            })
            .collect();

        Ok(json!({
            "query": query,
            "top_k": top_k,
            "matches": matches,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rrf_merge_fuses_both_legs_and_dedups() {
        let semantic = vec!["web-fetch".to_string(), "http-client".to_string()];
        let keyword = vec![
            "http-client".to_string(), // in both → highest fused score
            "file-reader".to_string(),
        ];
        let out = rrf_merge(&semantic, &keyword, 5);
        let names: Vec<&str> = out.iter().map(|(n, _, _)| n.as_str()).collect();
        assert_eq!(names, vec!["http-client", "web-fetch", "file-reader"]);
        // http-client is #2 semantic but #1 keyword → the keyword leg ranked it higher.
        assert_eq!(out[0].2, "keyword");
        assert_eq!(out[1].2, "semantic");
        assert!(out.iter().all(|(_, s, _)| *s > 0.0));
        // Empty semantic leg → keyword order unchanged.
        let out = rrf_merge(&[], &keyword, 5);
        let names: Vec<&str> = out.iter().map(|(n, _, _)| n.as_str()).collect();
        assert_eq!(names, vec!["http-client", "file-reader"]);
        assert!(out.iter().all(|(_, _, src)| *src == "keyword"));
    }

    #[tokio::test]
    async fn execute_applies_category_allowlist_end_to_end() {
        use crate::agent_manual::ToolSummary;
        use crate::traits::ToolExecutionContext;
        use agentos_types::*;

        fn summary(name: &str, desc: &str, category: &str) -> ToolSummary {
            ToolSummary {
                name: name.into(),
                description: desc.into(),
                version: "1.0.0".into(),
                permissions: vec![],
                payload_schema: None,
                examples: vec![],
                trust_tier: "core".into(),
                capability_tags: vec![],
                category: category.into(),
                search_hints: Vec::new(),
                tags: vec![],
                risk_class: "readonly_scoped".into(),
                usage_hints: None,
            }
        }

        let summaries = vec![
            summary("file-reader", "Read files", "core"),
            summary("slack-send", "Read and send a message", "channel"),
        ];
        let shared = Arc::new(tokio::sync::RwLock::new(summaries));
        // No-op embedder → semantic disabled → exercises the keyword path, which
        // is where the end-to-end allowlist application lives.
        let tool = SearchToolsTool::new(shared, Arc::new(Embedder::noop()));

        let ctx = ToolExecutionContext {
            data_dir: std::path::PathBuf::from("/tmp"),
            task_id: TaskID::new(),
            agent_id: AgentID::new(),
            trace_id: TraceID::new(),
            permissions: PermissionSet::new(),
            vault: None,
            hal: None,
            file_lock_registry: None,
            agent_registry: None,
            task_registry: None,
            escalation_query: None,
            workspace_paths: vec![],
            workspace_paths_writable: vec![],
            workspace_paths_executable: vec![],
            capability_registry: None,
            capability_dispatcher: None,
            storage_zone_query: None,
            cancellation_token: tokio_util::sync::CancellationToken::new(),
            tool_categories: Some(vec!["core".into()]),
        };

        // "read" matches BOTH tools by keyword, but only `core` is allowed.
        let out = tool
            .execute(json!({"query": "read"}), ctx)
            .await
            .expect("search-tools execute");
        let names: Vec<String> = out["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["name"].as_str().unwrap().to_string())
            .collect();
        assert!(names.iter().any(|n| n == "file-reader"));
        assert!(
            !names.iter().any(|n| n == "slack-send"),
            "channel tool must be excluded by the core allowlist, got {names:?}"
        );
    }

    #[test]
    fn merge_respects_top_k() {
        let semantic = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let keyword = vec!["d".to_string()];
        let out = rrf_merge(&semantic, &keyword, 2);
        assert_eq!(out.len(), 2);
        // "a" (sem #1) and "d" (kw #1) tie on RRF score → name order breaks it.
        assert_eq!(out[0].0, "a");
        assert_eq!(out[1].0, "d");
    }

    #[test]
    fn keyword_scorer_prefers_exact_name() {
        let exact = SearchToolsTool::score_tool("file-reader", "Read files", &[], "file-reader");
        let partial =
            SearchToolsTool::score_tool("file-reader-extra", "Read files", &[], "file-reader");
        assert!(exact > partial);
    }
}
