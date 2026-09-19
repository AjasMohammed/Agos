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
        // An omitted filter arrives three ways depending on the model: absent,
        // `null`, or `""`. All three mean "no filter" — `Some("")` matched no
        // category, so the catalogue came back empty and the agent concluded
        // it had no tools at all.
        let category = payload
            .get("category")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let tag = payload
            .get("tag")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
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

        // Task-scoped allowlist: hide tools whose category is not in the
        // active list. None = no restriction.
        let allowlist = context.tool_categories.as_ref();
        let mut filtered: Vec<_> = summaries
            .iter()
            .filter(|s| {
                let allow_ok = allowlist
                    .is_none_or(|al| al.iter().any(|c| c.eq_ignore_ascii_case(&s.category)));
                let cat_ok = category.is_none_or(|c| s.category.eq_ignore_ascii_case(c));
                let tag_ok =
                    tag.is_none_or(|t| s.tags.iter().any(|tag| tag.eq_ignore_ascii_case(t)));
                // Same visibility rule as the native tool array: a tool the
                // agent holds no permission for is not listed. Enforcement at
                // call time is unchanged; this only stops the catalogue from
                // advertising what the agent cannot call.
                let perm_ok = agentos_capability::any_permission_granted(
                    &context.permissions,
                    &s.permissions,
                );
                allow_ok && cat_ok && tag_ok && perm_ok
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
        // `page` is 0-based, and models routinely send 1 for the first page.
        // Slicing past the end returned `tools: []` next to a non-zero
        // `total`, which reads as "the catalogue is empty". Clamp to the last
        // populated page and report the page actually served.
        let last_page = total.saturating_sub(1) / page_size;
        let requested_page = page;
        let page = page.min(last_page);
        // A caller that pages by incrementing a counter until `tools` is empty
        // would now never stop, since the clamped page is always full. Say so
        // explicitly, so both that loop and one following `next_page` end.
        let end_of_results = page < requested_page;
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

        let mut out = json!({
            "category": category,
            "tag": tag,
            "page": page,
            "page_size": page_size,
            "total": total,
            "tools": tools,
            "next_page": next_page,
        });
        if end_of_results {
            out["requested_page"] = json!(requested_page);
            out["note"] = json!(format!(
                "Requested page {requested_page} is past the end; serving the last page ({page}). \
                 Pages are 0-based — there is nothing after this one."
            ));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_manual::ToolSummary;
    use agentos_types::{AgentID, PermissionSet, TaskID, TraceID};
    use std::sync::Arc;
    use tokio::sync::RwLock;

    fn summary(name: &str, category: &str) -> ToolSummary {
        ToolSummary {
            name: name.to_string(),
            description: format!("{name} does something"),
            version: "1.0.0".to_string(),
            permissions: vec![],
            payload_schema: None,
            examples: vec![],
            trust_tier: "core".to_string(),
            capability_tags: vec![],
            search_hints: vec![],
            category: category.to_string(),
            tags: vec!["read".to_string()],
            risk_class: "readonly_scoped".to_string(),
            usage_hints: None,
        }
    }

    fn ctx(data_dir: &std::path::Path) -> ToolExecutionContext {
        ToolExecutionContext {
            data_dir: data_dir.to_path_buf(),
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
            tool_categories: None,
        }
    }

    async fn list(payload: serde_json::Value) -> serde_json::Value {
        let dir = tempfile::tempdir().unwrap();
        let summaries = Arc::new(RwLock::new(vec![
            summary("alpha", "system"),
            summary("beta", "system"),
            summary("gamma", "fs"),
        ]));
        ListToolsTool::new(summaries)
            .execute(payload, ctx(dir.path()))
            .await
            .unwrap()
    }

    /// The empty string is what a model emits for an omitted optional filter.
    /// Treated as a real filter it matches no category, and the agent is told
    /// it owns no tools at all.
    #[tokio::test]
    async fn empty_string_filters_mean_no_filter() {
        let out = list(json!({"category": "", "tag": "", "page": 0})).await;
        assert_eq!(out["total"], 3);
        assert_eq!(out["tools"].as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn null_filters_mean_no_filter() {
        let out = list(json!({"category": null, "tag": null})).await;
        assert_eq!(out["total"], 3);
    }

    #[tokio::test]
    async fn category_filter_still_applies() {
        let out = list(json!({"category": "system"})).await;
        assert_eq!(out["total"], 2);
        assert_eq!(out["tools"][0]["name"], "alpha");
    }

    /// `page` is 0-based but models send 1 for the first page. Slicing past
    /// the end used to return `tools: []` beside a non-zero `total`.
    #[tokio::test]
    async fn out_of_range_page_serves_the_last_page() {
        let out = list(json!({"category": "system", "page": 1, "page_size": 20})).await;
        assert_eq!(out["total"], 2);
        assert_eq!(out["page"], 0, "clamped page must be reported honestly");
        assert_eq!(out["tools"].as_array().unwrap().len(), 2);
        assert!(out["next_page"].is_null());
        assert_eq!(out["requested_page"], 1);
        assert!(
            out["note"].as_str().unwrap().contains("past the end"),
            "a full last page with no note makes 'loop until empty' run forever"
        );
    }

    /// Clamping must not collapse real pagination.
    #[tokio::test]
    async fn in_range_pages_are_untouched() {
        let out = list(json!({"page": 1, "page_size": 2})).await;
        assert_eq!(out["page"], 1);
        assert_eq!(out["tools"].as_array().unwrap().len(), 1);
        assert!(out["next_page"].is_null());
        assert!(out["note"].is_null(), "an in-range page was not clamped");

        let first = list(json!({"page": 0, "page_size": 2})).await;
        assert_eq!(first["next_page"], 1);
    }
}
