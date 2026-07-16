//! Task-time tool scoping.
//!
//! Frontier models otherwise receive all ~132 tool schemas on every turn, which
//! hurts both cost and tool-selection accuracy. This module classifies a task
//! prompt into the tool *categories* whose native schemas should be pre-loaded,
//! and filters the native tool array to that scope.
//!
//! Scoping is a **soft pre-load filter, never a hard wall**: anything scoped out
//! stays reachable through the semantic `search-tools` escape hatch (Phase 1),
//! which searches the full registry irrespective of scope. The scope is computed
//! **once per task** and the native array is never re-armed mid-task (DD4), so it
//! stays behind the Anthropic tools cache breakpoint for the whole task.
//!
//! `category` is the *inferred* dimension (`AgentManualTool::infer_tool_category`)
//! — it is NOT a stored field on `ToolManifest`. The `read/write/exec/network/
//! fs/meta` taxonomy is a separate *tag* dimension; in particular `fs` is a tag,
//! not a category (file tools infer to `core`).

use agentos_tools::agent_manual::AgentManualTool;
use agentos_types::ToolManifest;
use async_trait::async_trait;

/// Classifies a task prompt into the tool categories to pre-load.
#[async_trait]
pub trait TaskToolClassifier: Send + Sync {
    /// Returns the categories whose tools should be pre-loaded for this task.
    /// `known_categories` is advisory — the authoritative set of values
    /// `infer_tool_category` can emit, supplied so impls that want it can fail
    /// open. It MAY be empty: the default heuristic ignores it, and callers pass
    /// `&[]` to skip an unnecessary registry scan.
    async fn classify(&self, prompt: &str, known_categories: &[String]) -> Vec<String>;
}

/// Zero-cost keyword classifier — the default. Targets the real categories
/// emitted by `infer_tool_category`, never the `fs`/`read`/`write` *tags*.
/// Always seeds `core` so generic tasks are never tool-starved. Deterministic,
/// no I/O, no inference — returns immediately despite the async trait.
#[derive(Debug, Default)]
pub struct HeuristicClassifier;

#[async_trait]
impl TaskToolClassifier for HeuristicClassifier {
    async fn classify(&self, prompt: &str, _known: &[String]) -> Vec<String> {
        let p = prompt.to_ascii_lowercase();
        let mut cats: std::collections::BTreeSet<&'static str> = Default::default();
        let any = |keys: &[&str]| keys.iter().any(|k| p.contains(k));

        // File/read/write/shell tasks live in category `core` (NOT a non-existent
        // `fs` category — `fs` is a tag and file tools infer to `core`).
        if any(&[
            "file",
            "read",
            "write",
            "edit",
            "grep",
            "glob",
            "delete",
            "directory",
            "folder",
            "path",
            "shell",
            "command",
            "execute",
        ]) {
            cats.insert("core");
        }
        if any(&[
            "remember",
            "recall",
            "last time",
            "previously",
            "memory",
            "forget",
        ]) {
            cats.insert("memory");
        }
        if any(&[
            "message",
            "slack",
            "discord",
            "telegram",
            "send",
            "channel",
            "whatsapp",
            "matrix",
            "mattermost",
        ]) {
            cats.insert("channel");
        }
        if any(&[
            "schedule",
            "cron",
            "timer",
            "remind",
            "every day",
            "recurring",
            "periodically",
        ]) {
            cats.insert("scheduling");
        }
        if any(&["container", "docker", "image", "pod"]) {
            cats.insert("containers");
        }
        if any(&["webhook"]) {
            cats.insert("webhooks");
        }
        if any(&[
            "device",
            "gpu",
            "sensor",
            "hardware",
            "camera",
            "microphone",
        ]) {
            cats.insert("hal");
        }
        if any(&["event", "subscribe", "publish", "notify event"]) {
            cats.insert("events");
        }
        if any(&["skill"]) {
            cats.insert("skills");
        }
        if any(&["plugin"]) {
            cats.insert("plugins");
        }
        if any(&["scratch", "scratchpad", "wikilink"]) {
            cats.insert("scratchpad");
        }
        if any(&[
            "environment",
            "virtualenv",
            "venv",
            "process",
            "build",
            "compile",
            "storage zone",
            "capability",
        ]) {
            cats.insert("capabilities");
        }
        if any(&["approval", "ask the user", "ask user", "notify the user"]) {
            cats.insert("notifications");
        }

        // `core` is always present so the common tools are never scoped away.
        // `mcp` too: installed MCP-server tools infer to category `mcp`, which no
        // keyword emits, so without this they'd be silently dropped from the
        // native array (still reachable via search-tools, but that's a regression
        // for the integrations users explicitly install).
        cats.insert("core");
        cats.insert("mcp");
        cats.into_iter().map(str::to_string).collect()
    }
}

/// Whether a category survives the (soft) scope. `None` = no scope (legacy "all
/// tools"). Meta-tagged tools always survive — the discovery/coordination escape
/// hatch must never be scoped out, whatever its category.
fn category_in_scope(category: &str, is_meta: bool, scope: Option<&[String]>) -> bool {
    match scope {
        None => true,
        Some(s) => is_meta || s.iter().any(|c| c.eq_ignore_ascii_case(category)),
    }
}

/// Whether a manifest survives the (soft) category scope. The category is
/// inferred (`ToolManifest` has no `category` field); meta-tagged tools always
/// survive. Used to filter the native tool array at task setup.
pub(crate) fn manifest_in_scope(manifest: &ToolManifest, scope: Option<&[String]>) -> bool {
    if scope.is_none() {
        return true;
    }
    let is_meta = manifest.tags.iter().any(|t| t.eq_ignore_ascii_case("meta"));
    let category = AgentManualTool::infer_tool_category(
        &manifest.manifest.name,
        &manifest.manifest.capability_tags,
        manifest.manifest.tags.as_deref(),
    );
    category_in_scope(&category, is_meta, scope)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn heuristic_routes_file_task_to_core_not_fs() {
        let cats = HeuristicClassifier
            .classify("read the config file at /etc/app.toml", &[])
            .await;
        assert!(cats.iter().any(|c| c == "core"));
        // `fs` is a tag, not a category — it must never appear here.
        assert!(!cats.iter().any(|c| c == "fs"));
    }

    #[tokio::test]
    async fn heuristic_routes_memory_and_always_seeds_core() {
        let cats = HeuristicClassifier
            .classify("remember that I prefer metric units", &[])
            .await;
        assert!(cats.iter().any(|c| c == "memory"));
        assert!(cats.iter().any(|c| c == "core"));
    }

    #[tokio::test]
    async fn heuristic_unknown_prompt_still_seeds_core_and_mcp() {
        let cats = HeuristicClassifier.classify("xyzzy", &[]).await;
        // BTreeSet order: always-seeded floor is core + mcp so generic tasks keep
        // the common tools and installed MCP servers.
        assert_eq!(cats, vec!["core".to_string(), "mcp".to_string()]);
    }

    #[test]
    fn category_in_scope_none_allows_all() {
        assert!(category_in_scope("memory", false, None));
        assert!(category_in_scope("anything", false, None));
    }

    #[test]
    fn category_in_scope_filters_by_category() {
        let scope = vec!["core".to_string(), "memory".to_string()];
        assert!(category_in_scope("memory", false, Some(&scope)));
        assert!(category_in_scope("CORE", false, Some(&scope))); // case-insensitive
        assert!(!category_in_scope("channel", false, Some(&scope)));
    }

    #[test]
    fn category_in_scope_meta_always_survives() {
        let scope = vec!["memory".to_string()];
        // A `channel`-category tool that is meta-tagged still survives a
        // memory-only scope (escape hatch / coordination).
        assert!(category_in_scope("channel", true, Some(&scope)));
    }
}
