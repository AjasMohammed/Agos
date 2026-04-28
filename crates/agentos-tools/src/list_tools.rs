use crate::agent_manual::{AgentManualTool, SharedToolSummaries};
use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::json;

pub struct ListToolsTool {
    tool_summaries: SharedToolSummaries,
}

impl ListToolsTool {
    pub fn new(tool_summaries: SharedToolSummaries) -> Self {
        Self { tool_summaries }
    }
}

#[async_trait]
impl AgentTool for ListToolsTool {
    fn name(&self) -> &str {
        "list-tools"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let category = payload.get("category").and_then(|v| v.as_str());
        let tag = payload.get("tag").and_then(|v| v.as_str());
        let page = payload.get("page").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let page_size = (payload
            .get("page_size")
            .and_then(|v| v.as_u64())
            .unwrap_or(20) as usize)
            .clamp(1, 50);

        let summaries = {
            let guard = self.tool_summaries.read().await;
            guard.clone()
        };

        let usage_scores =
            AgentManualTool::load_usage_scores_async(context.data_dir.clone(), context.agent_id)
                .await;

        let mut filtered: Vec<_> = summaries
            .iter()
            .filter(|s| {
                let cat_ok = category.is_none_or(|c| s.category.eq_ignore_ascii_case(c));
                let tag_ok =
                    tag.is_none_or(|t| s.tags.iter().any(|tag| tag.eq_ignore_ascii_case(t)));
                cat_ok && tag_ok
            })
            .collect();

        if !usage_scores.is_empty() {
            filtered.sort_by(|a, b| {
                let sa = usage_scores.get(&a.name).copied().unwrap_or(0.0);
                let sb = usage_scores.get(&b.name).copied().unwrap_or(0.0);
                sb.partial_cmp(&sa)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.name.cmp(&b.name))
            });
        } else {
            filtered.sort_by(|a, b| a.name.cmp(&b.name));
        }

        let total = filtered.len();
        let start = page.saturating_mul(page_size).min(total);
        let end = start.saturating_add(page_size).min(total);

        let tools: Vec<serde_json::Value> = filtered[start..end]
            .iter()
            .map(|s| {
                json!({
                    "name": s.name,
                    "description": s.description,
                    "category": s.category,
                    "tags": s.tags,
                })
            })
            .collect();

        let next_page: Option<usize> = if end < total { Some(page + 1) } else { None };

        Ok(json!({
            "category": category,
            "tag": tag,
            "page": page,
            "page_size": page_size,
            "total": total,
            "tools": tools,
            "next_page": next_page,
        }))
    }
}
