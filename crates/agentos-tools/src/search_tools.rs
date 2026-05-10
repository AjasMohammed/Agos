use crate::agent_manual::SharedToolSummaries;
use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::json;

pub struct SearchToolsTool {
    tool_summaries: SharedToolSummaries,
}

impl SearchToolsTool {
    pub fn new(tool_summaries: SharedToolSummaries) -> Self {
        Self { tool_summaries }
    }

    /// Score a tool against a query string.
    /// Returns a score where higher is more relevant.
    fn score_tool(name: &str, description: &str, tags: &[String], query_lower: &str) -> i32 {
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

        // Task-scoped allowlist: hide tools whose category is not in the
        // active list. None = no restriction.
        let allowlist = context.tool_categories.as_ref();
        let mut scored: Vec<(i32, &str, &str)> = summaries
            .iter()
            .filter(|s| {
                allowlist.is_none_or(|al| al.iter().any(|c| c.eq_ignore_ascii_case(&s.category)))
            })
            .map(|s| {
                let score = Self::score_tool(&s.name, &s.description, &s.tags, &query_lower);
                (score, s.name.as_str(), s.description.as_str())
            })
            .filter(|(score, _, _)| *score > 0)
            .collect();

        // Sort: descending score, then ascending name
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));

        let matches: Vec<serde_json::Value> = scored
            .iter()
            .take(top_k)
            .map(|(score, name, description)| {
                json!({
                    "name": name,
                    "description": description,
                    "score": score,
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
