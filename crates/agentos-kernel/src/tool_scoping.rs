//! Task-time tool scoping: working-set admission for the native tool array
//! (`admit`) plus the legacy explicit `task.tool_categories` filter.
//!
//! The native tool array is a **working set**, not the catalogue: `admit` keeps
//! T0 (the `meta`-tagged escape hatch, config `pinned_tools`, the agent's most
//! used tools) plus T1 (top-K hybrid-retrieval hits over the task prompt) and
//! parks everything else in a deferred pool. Anything deferred stays reachable
//! through `search-tools`/`describe-tool`: the executor arms the schema on a
//! hit (appended after the cache breakpoint), or — on providers with native
//! deferral — the API expands a `tool_reference`. An explicit
//! `task.tool_categories` keeps the legacy whole-category filter
//! (`manifest_in_scope`). Category comes from `AgentManualTool::category_of`:
//! the manifest's `[manifest].category` when declared, else name inference.

use agentos_tools::agent_manual::AgentManualTool;
use agentos_types::ToolManifest;
use std::collections::HashMap;

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
    let category = AgentManualTool::category_of(manifest);
    category_in_scope(&category, is_meta, scope)
}

#[cfg(test)]
mod tests {
    use super::*;

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

/// Working-set admission policy for the native tool array (deferred tool
/// loading). See `obsidian-vault/plans/deferred-tool-loading/`.
pub struct WorkingSetPolicy<'a> {
    /// Always-loaded tool names (config `tools.discovery.pinned_tools`).
    pub pinned_tools: &'a [String],
    /// Per-agent most-used tools pinned into T0.
    pub pinned_usage_top_n: usize,
    /// T1 size: how many retrieval hits over the task prompt to pre-arm.
    pub working_set_size: usize,
}

fn take(by_name: &mut HashMap<String, ToolManifest>, native: &mut Vec<ToolManifest>, name: &str) {
    if let Some(m) = by_name.remove(name) {
        native.push(m);
    }
}

/// Split `all` into the native array and the deferred pool.
///
/// Native order is deterministic and stable across iterations (it is the
/// cached prefix): T0 = `meta`-tagged escape hatch (sorted by name) + pinned
/// tools (config order) + the agent's top-N by usage; then T1 = `t1_ranked`
/// (best-first, already ranked by the caller) up to `working_set_size`.
/// Every input manifest lands in exactly one side.
pub fn admit(
    all: Vec<ToolManifest>,
    usage: &HashMap<String, f64>,
    t1_ranked: &[String],
    policy: &WorkingSetPolicy<'_>,
) -> (Vec<ToolManifest>, HashMap<String, ToolManifest>) {
    let all_len = all.len();
    let mut by_name: HashMap<String, ToolManifest> = all
        .into_iter()
        .map(|m| (m.manifest.name.clone(), m))
        .collect();
    // ToolRegistry enforces unique names; a duplicate here would vanish from
    // both sides and break the "exactly one side" invariant.
    debug_assert_eq!(
        by_name.len(),
        all_len,
        "duplicate tool names passed to admit"
    );
    let mut native = Vec::with_capacity(policy.working_set_size + 16);

    let mut meta: Vec<String> = by_name
        .values()
        .filter(|m| m.tags.iter().any(|t| t.eq_ignore_ascii_case("meta")))
        .map(|m| m.manifest.name.clone())
        .collect();
    meta.sort();
    for n in &meta {
        take(&mut by_name, &mut native, n);
    }
    for n in policy.pinned_tools {
        take(&mut by_name, &mut native, n);
    }
    let mut ranked: Vec<(&String, f64)> = usage
        .iter()
        .filter(|(n, _)| by_name.contains_key(*n))
        .map(|(n, s)| (n, *s))
        .collect();
    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(b.0))
    });
    let top: Vec<String> = ranked
        .into_iter()
        .take(policy.pinned_usage_top_n)
        .map(|(n, _)| n.clone())
        .collect();
    for n in &top {
        take(&mut by_name, &mut native, n);
    }
    let mut admitted = 0usize;
    for n in t1_ranked {
        if admitted >= policy.working_set_size {
            break;
        }
        if by_name.contains_key(n) {
            take(&mut by_name, &mut native, n);
            admitted += 1;
        }
    }
    (native, by_name)
}

/// Move `names` a successful `search-tools`/`describe-tool` surfaced out of a
/// chat turn's deferred `pool` into the `native` array, so the model can call
/// them next iteration. Names not pooled (already native, unknown, withheld)
/// are ignored. Stops once `armed` reaches `cap` for the turn — the chat pool
/// is rebuilt every turn, so there is no LRU eviction like the task path's.
pub(crate) fn arm_discovered(
    names: Vec<String>,
    native: &mut Vec<ToolManifest>,
    pool: &mut HashMap<String, ToolManifest>,
    armed: &mut usize,
    cap: usize,
) {
    for name in names {
        if *armed >= cap.max(1) {
            break;
        }
        if let Some(m) = pool.remove(&name) {
            tracing::info!(tool_name = %name, "Armed deferred chat tool after discovery");
            native.push(m);
            *armed += 1;
        }
    }
}

#[cfg(test)]
mod admit_tests {
    use super::*;

    fn manifest(name: &str, tags: &[&str]) -> ToolManifest {
        let mut m = crate::tool_registry::tests::make_core_manifest(name);
        m.tags = tags.iter().map(|t| t.to_string()).collect();
        m
    }

    #[test]
    fn admit_orders_t0_then_t1_and_pools_the_rest() {
        let all = vec![
            manifest("zeta", &["read"]),
            manifest("search-tools", &["meta"]),
            manifest("think", &["read"]),
            manifest("file-reader", &["read"]),
            manifest("web-fetch", &["network"]),
            manifest("audio", &["exec"]),
        ];
        let pinned = vec!["think".to_string(), "missing".to_string()];
        let mut usage = HashMap::new();
        usage.insert("web-fetch".to_string(), 9.0);
        usage.insert("zeta".to_string(), 1.0);
        let policy = WorkingSetPolicy {
            pinned_tools: &pinned,
            pinned_usage_top_n: 1,
            working_set_size: 1,
        };
        let t1 = vec![
            "web-fetch".to_string(),
            "file-reader".to_string(),
            "audio".to_string(),
        ];
        let (native, pool) = admit(all, &usage, &t1, &policy);
        let names: Vec<&str> = native.iter().map(|m| m.manifest.name.as_str()).collect();
        // meta, pinned, usage top-1, then T1 (web-fetch already taken → file-reader)
        assert_eq!(
            names,
            vec!["search-tools", "think", "web-fetch", "file-reader"]
        );
        let mut pooled: Vec<&String> = pool.keys().collect();
        pooled.sort();
        assert_eq!(pooled, vec!["audio", "zeta"]);
    }

    #[test]
    fn arm_discovered_moves_pooled_hits_up_to_cap() {
        let mut native = vec![manifest("search-tools", &["meta"])];
        let mut pool: HashMap<String, ToolManifest> = ["a", "b", "c"]
            .iter()
            .map(|n| (n.to_string(), manifest(n, &[])))
            .collect();
        let mut armed = 0;
        let hits = ["search-tools", "missing", "a", "b", "c"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        arm_discovered(hits, &mut native, &mut pool, &mut armed, 2);
        let names: Vec<&str> = native.iter().map(|m| m.manifest.name.as_str()).collect();
        // already-native and unknown names ignored; cap stops before "c"
        assert_eq!(names, vec!["search-tools", "a", "b"]);
        assert_eq!(armed, 2);
        assert!(pool.contains_key("c") && pool.len() == 1);
    }

    #[test]
    fn admit_over_core_catalogue_stays_small() {
        let core = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/core");
        let none = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/__none__");
        let registry = crate::tool_registry::ToolRegistry::load_from_dirs(&core, &none)
            .expect("core manifests load");
        let all: Vec<ToolManifest> = registry
            .list_all()
            .into_iter()
            .map(|t| t.manifest.clone())
            .collect();
        let total = all.len();
        let meta_count = all
            .iter()
            .filter(|m| m.tags.iter().any(|t| t == "meta"))
            .count();
        let cfg = crate::config::DiscoverySettings::default();
        let policy = WorkingSetPolicy {
            pinned_tools: &cfg.pinned_tools,
            pinned_usage_top_n: cfg.pinned_usage_top_n,
            working_set_size: cfg.working_set_size,
        };
        // Realistic worst case: a full T1 of K distinct hits and a populated
        // usage map (defaults: 6 meta + 5 pinned (1 overlaps meta) + 3 usage + 8).
        let t1: Vec<String> = [
            "file-reader",
            "file-writer",
            "shell-exec",
            "web-fetch",
            "file-glob",
            "file-grep",
            "http-client",
            "web-search",
            "sys-monitor",
            "process-manager",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let mut usage = HashMap::new();
        usage.insert("memory-write".to_string(), 9.0);
        usage.insert("agent-message".to_string(), 5.0);
        usage.insert("notify-user".to_string(), 50.0); // pinned already → no extra slot
        usage.insert("hardware-info".to_string(), 3.0);
        let (native, pool) = admit(all, &usage, &t1, &policy);
        let ceiling =
            meta_count + cfg.pinned_tools.len() + cfg.pinned_usage_top_n + cfg.working_set_size;
        assert!(
            native.len() <= ceiling,
            "native={} ceiling={ceiling} meta={meta_count}",
            native.len()
        );
        assert!(
            native.len() <= 21,
            "worst-case native array must stay ≤21, got {}",
            native.len()
        );
        assert_eq!(native.len() + pool.len(), total);
        for n in ["search-tools", "describe-tool", "list-tools"] {
            assert!(
                native.iter().any(|m| m.manifest.name == n),
                "{n} missing from T0"
            );
        }
    }
}
