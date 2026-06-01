use crate::agent_manual::SharedToolSummaries;
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
    index: ToolSearchIndex,
}

impl SearchToolsTool {
    pub fn new(tool_summaries: SharedToolSummaries, embedder: Arc<Embedder>) -> Self {
        Self {
            tool_summaries,
            index: ToolSearchIndex::new(embedder),
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

/// Merge semantic and keyword candidate names into a single best-first order:
/// semantic hits lead (they capture intent/synonyms), then keyword hits fill the
/// remaining slots, deduped by name and capped at `top_k`. Mirrors the
/// semantic-first-then-keyword-fill strategy of `suggest_manual_sections_async`.
/// Returns `(name, source)` where `source` is `"semantic"` or `"keyword"`.
pub(crate) fn merge_order(
    semantic_names: &[String],
    keyword_names: &[String],
    top_k: usize,
) -> Vec<(String, &'static str)> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out: Vec<(String, &'static str)> = Vec::with_capacity(top_k);
    for name in semantic_names {
        if out.len() >= top_k {
            return out;
        }
        if seen.insert(name.as_str()) {
            out.push((name.clone(), "semantic"));
        }
    }
    for name in keyword_names {
        if out.len() >= top_k {
            break;
        }
        if seen.insert(name.as_str()) {
            out.push((name.clone(), "keyword"));
        }
    }
    out
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

        let summaries = {
            let guard = self.tool_summaries.read().await;
            guard.clone()
        };

        let query_lower = query.to_lowercase();

        // Task-scoped allowlist: hide tools whose category is not in the active
        // list. `None` = no restriction.
        let allowlist = context.tool_categories.as_ref();
        let cat_allowed = |category: &str| {
            allowlist.is_none_or(|al| al.iter().any(|c| c.eq_ignore_ascii_case(category)))
        };

        // 1. Keyword scoring (substring/token overlap), descending score then name.
        let mut keyword: Vec<(i32, String)> = summaries
            .iter()
            .filter(|s| cat_allowed(&s.category))
            .map(|s| {
                (
                    Self::score_tool(&s.name, &s.description, &s.tags, &query_lower),
                    s.name.clone(),
                )
            })
            .filter(|(score, _)| *score > 0)
            .collect();
        keyword.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        let keyword_score: HashMap<String, i32> =
            keyword.iter().map(|(s, n)| (n.clone(), *s)).collect();
        let keyword_names: Vec<String> = keyword.into_iter().map(|(_, n)| n).collect();

        // 2. Semantic ranking (embedding cosine), restricted to the same allowlist.
        let allowed_names: Option<HashSet<String>> = allowlist.map(|_| {
            summaries
                .iter()
                .filter(|s| cat_allowed(&s.category))
                .map(|s| s.name.clone())
                .collect()
        });
        let semantic = self
            .index
            .semantic_rank(&summaries, query, allowed_names.as_ref(), top_k)
            .await;
        let semantic_score: HashMap<String, f32> = semantic.iter().cloned().collect();
        let semantic_names: Vec<String> = semantic.into_iter().map(|(n, _)| n).collect();

        // 3. Merge: semantic first, keyword fills the rest, deduped.
        let order = merge_order(&semantic_names, &keyword_names, top_k);
        let desc_by_name: HashMap<&str, &str> = summaries
            .iter()
            .map(|s| (s.name.as_str(), s.description.as_str()))
            .collect();

        let matches: Vec<serde_json::Value> = order
            .iter()
            .map(|(name, source)| {
                let description = desc_by_name.get(name.as_str()).copied().unwrap_or("");
                let score = if *source == "semantic" {
                    json!(semantic_score.get(name).copied().unwrap_or(0.0))
                } else {
                    json!(keyword_score.get(name).copied().unwrap_or(0))
                };
                json!({
                    "name": name,
                    "description": description,
                    "score": score,
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
    fn merge_puts_semantic_first_then_keyword_fill_deduped() {
        let semantic = vec!["web-fetch".to_string(), "http-client".to_string()];
        let keyword = vec![
            "http-client".to_string(), // dup of a semantic hit — must not repeat
            "file-reader".to_string(),
        ];
        let out = merge_order(&semantic, &keyword, 5);
        let names: Vec<&str> = out.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["web-fetch", "http-client", "file-reader"]);
        assert_eq!(out[0].1, "semantic");
        assert_eq!(out[2].1, "keyword");
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
        let out = merge_order(&semantic, &keyword, 2);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].0, "a");
        assert_eq!(out[1].0, "b");
    }

    #[test]
    fn keyword_scorer_prefers_exact_name() {
        let exact = SearchToolsTool::score_tool("file-reader", "Read files", &[], "file-reader");
        let partial =
            SearchToolsTool::score_tool("file-reader-extra", "Read files", &[], "file-reader");
        assert!(exact > partial);
    }
}
