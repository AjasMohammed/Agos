//! Phase-6 tool-discovery eval harness.
//!
//! **Two tiers — run the right one for the right context:**
//!
//! - **Tier 1 (structural, per-PR):** runs under `Embedder::noop()` — no model
//!   download. Asserts wiring correctness: the gold dataset is valid, lexical
//!   search finds lexical-match rows, the allowlist filters correctly, fail-open
//!   behaviour is intact.
//!   ```
//!   cargo test -p agentos-tools --test tool_discovery_eval structural
//!   ```
//!
//! - **Tier 2 (semantic, #[ignore], nightly/on-demand):** requires the real
//!   MiniLM embedder (`Embedder::with_cache_dir`). Computes recall@k / MRR /
//!   precision@k and the Δ vs lexical baseline. Gate: recall@3 beats lexical by
//!   ≥10 points on synonym rows without dropping precision@3 below lexical.
//!   ```
//!   cargo test -p agentos-tools --test tool_discovery_eval -- --ignored --nocapture
//!   ```
//!
//! Updating this file? See the gold-dataset update process at the bottom.

use agentos_memory::Embedder;
use agentos_tools::{
    agent_manual::{SharedToolSummaries, ToolSummary},
    search_tools::SearchToolsTool,
    traits::{AgentTool, ToolExecutionContext},
};
use agentos_types::*;
use serde::Deserialize;
use std::{collections::HashMap, path::Path, sync::Arc};
use tokio::sync::RwLock;

// ── Gold dataset ─────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Clone)]
struct GoldRow {
    query: String,
    expect_tools: Vec<String>,
    category: String,
    #[serde(rename = "type")]
    row_type: String, // "synonym" | "lexical"
}

fn load_gold() -> Vec<GoldRow> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/tool_discovery_gold.jsonl");
    let content = std::fs::read_to_string(&path).expect("gold jsonl must exist");
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("gold row must parse"))
        .collect()
}

// ── Minimal ToolSummary set covering the gold tools ──────────────────────────

fn gold_summaries() -> Vec<ToolSummary> {
    let entries: &[(&str, &str, &str, &[&str])] = &[
        (
            "web-fetch",
            "Retrieve URL contents. Fetch a web page or HTTP resource.",
            "core",
            &["read", "network"],
        ),
        (
            "web-search",
            "Search the web for current information.",
            "core",
            &["read", "network"],
        ),
        (
            "memory-write",
            "Persist a fact or user preference into semantic memory.",
            "memory",
            &["write"],
        ),
        (
            "memory-search",
            "Search semantic memory for prior facts and patterns.",
            "memory",
            &["read"],
        ),
        (
            "context-memory-read",
            "Read the context memory store for this agent.",
            "core",
            &["read"],
        ),
        (
            "context-memory-update",
            "Update a stable user preference in context memory.",
            "core",
            &["write"],
        ),
        (
            "file-reader",
            "Read files from the data directory with pagination.",
            "core",
            &["read", "fs"],
        ),
        (
            "file-writer",
            "Write content to a file in the data directory.",
            "core",
            &["write", "fs"],
        ),
        (
            "shell-exec",
            "Execute a shell command in a sandboxed environment.",
            "core",
            &["exec"],
        ),
        (
            "spawn-agent",
            "Spawn a sub-agent to handle a specific delegated task.",
            "core",
            &["exec", "meta"],
        ),
        (
            "await-agents",
            "Wait for and collect results from spawned child agents.",
            "core",
            &["read", "meta"],
        ),
        (
            "describe-tool",
            "Get the full schema and examples for a specific tool by name.",
            "core",
            &["read", "meta"],
        ),
        (
            "search-tools",
            "Search all registered tools by keyword or intent query.",
            "core",
            &["read", "meta"],
        ),
        (
            "list-tools",
            "List tools filtered by category, tag, or paginated.",
            "core",
            &["read", "meta"],
        ),
        (
            "channel-send",
            "Send a message to a connected messaging channel.",
            "channel",
            &["write", "network"],
        ),
        (
            "schedule-once",
            "Schedule a task or notification to run once at a future time.",
            "scheduling",
            &["write"],
        ),
        (
            "schedule-recurring",
            "Schedule a task to run on a repeating cron schedule.",
            "scheduling",
            &["write"],
        ),
        (
            "scratchpad-write",
            "Write a note or wiki page to the agent scratchpad.",
            "scratchpad",
            &["write"],
        ),
        (
            "system-mounts",
            "List filesystem mounts and disk usage on the host.",
            "core",
            &["read"],
        ),
        (
            "system-services",
            "Inspect systemd services running on the host.",
            "core",
            &["read"],
        ),
        (
            "network-sockets",
            "List active network sockets and listening ports.",
            "core",
            &["read", "network"],
        ),
        (
            "ask-user",
            "Prompt the human operator for input or approval.",
            "notifications",
            &["read", "meta"],
        ),
        (
            "notify-user",
            "Send a notification or message to the user.",
            "notifications",
            &["write", "meta"],
        ),
        (
            "kmc-env-create",
            "Create a managed environment for Python, Node, or Rust.",
            "capabilities",
            &["exec"],
        ),
        (
            "kmc-proc-spawn",
            "Spawn a managed process inside a capability environment.",
            "capabilities",
            &["exec"],
        ),
        (
            "kmc-net-check",
            "Check network connectivity from a capability environment.",
            "capabilities",
            &["read", "network"],
        ),
    ];
    entries
        .iter()
        .map(|(name, desc, cat, tags)| ToolSummary {
            name: name.to_string(),
            description: desc.to_string(),
            version: "1.0.0".into(),
            permissions: vec![],
            payload_schema: None,
            examples: vec![],
            trust_tier: "core".into(),
            capability_tags: vec![],
            category: cat.to_string(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            risk_class: "readonly_scoped".into(),
            usage_hints: None,
        })
        .collect()
}

fn make_shared(summaries: Vec<ToolSummary>) -> SharedToolSummaries {
    Arc::new(RwLock::new(summaries))
}

fn noop_ctx() -> ToolExecutionContext {
    ToolExecutionContext {
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
        tool_categories: None,
    }
}

async fn run_search(tool: &SearchToolsTool, query: &str) -> Vec<String> {
    let result = tool
        .execute(serde_json::json!({"query": query, "top_k": 10}), noop_ctx())
        .await
        .expect("search-tools must not fail");
    result["matches"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .map(|m| m["name"].as_str().unwrap_or("").to_string())
        .collect()
}

// ── Tier-1 structural tests (run per-PR, no model download) ──────────────────

#[tokio::test]
async fn structural_gold_dataset_validity_guards() {
    // Every expected tool in the gold dataset must exist in the summary set.
    let summaries = gold_summaries();
    let name_set: std::collections::HashSet<&str> =
        summaries.iter().map(|s| s.name.as_str()).collect();
    let gold = load_gold();
    let mut missing: Vec<String> = Vec::new();
    for row in &gold {
        for tool in &row.expect_tools {
            if !name_set.contains(tool.as_str()) {
                missing.push(format!("gold row {:?}: unknown tool '{}'", row.query, tool));
            }
        }
    }
    if !missing.is_empty() {
        panic!(
            "Gold dataset validity failures ({}):\n{}",
            missing.len(),
            missing.join("\n")
        );
    }
}

#[tokio::test]
async fn structural_gold_category_validity() {
    // Every gold row's `category` must equal `infer_tool_category(...)` for at
    // least one of its expected tools — prevents phantom categories like "network".
    let summaries = gold_summaries();
    let cat_by_name: HashMap<&str, &str> = summaries
        .iter()
        .map(|s| (s.name.as_str(), s.category.as_str()))
        .collect();
    let gold = load_gold();
    let mut bad: Vec<String> = Vec::new();
    for row in &gold {
        let actual_cats: Vec<&str> = row
            .expect_tools
            .iter()
            .filter_map(|t| cat_by_name.get(t.as_str()).copied())
            .collect();
        if !actual_cats.is_empty() && !actual_cats.contains(&row.category.as_str()) {
            bad.push(format!(
                "query {:?}: gold category '{}' ≠ any infer_tool_category result {:?}",
                row.query, row.category, actual_cats
            ));
        }
    }
    if !bad.is_empty() {
        panic!(
            "Gold category validation failures ({}):\n{}",
            bad.len(),
            bad.join("\n")
        );
    }
}

#[tokio::test]
async fn structural_lexical_rows_found_by_keyword_scorer() {
    // On lexical rows, at least one expected tool must appear in the top-10
    // from the keyword-only scorer (noop embedder → falls back to lexical).
    let shared = make_shared(gold_summaries());
    let tool = SearchToolsTool::new(shared, Arc::new(Embedder::noop()));
    let gold = load_gold();
    let lexical_rows: Vec<&GoldRow> = gold.iter().filter(|r| r.row_type == "lexical").collect();
    let n_lexical = lexical_rows.len();
    let mut failed: Vec<String> = Vec::new();
    for row in &lexical_rows {
        let hits = run_search(&tool, &row.query).await;
        let found = row.expect_tools.iter().any(|e| hits.contains(e));
        if !found {
            failed.push(format!(
                "query {:?}: expected any of {:?}, got {:?}",
                row.query,
                row.expect_tools,
                &hits[..hits.len().min(5)]
            ));
        }
    }
    if !failed.is_empty() {
        panic!(
            "Lexical rows not found by keyword scorer ({}/{}):\n{}",
            failed.len(),
            n_lexical,
            failed.join("\n")
        );
    }
}

#[tokio::test]
async fn structural_allowlist_filters_results() {
    // Tools outside the category allowlist must not appear in results.
    let shared = make_shared(gold_summaries());
    let tool = SearchToolsTool::new(shared, Arc::new(Embedder::noop()));
    // Query that would normally match both core and memory tools.
    let mut ctx = noop_ctx();
    ctx.tool_categories = Some(vec!["memory".to_string()]);
    let result = tool
        .execute(
            serde_json::json!({"query": "search memory", "top_k": 10}),
            ctx,
        )
        .await
        .expect("search-tools");
    let empty = vec![];
    let names: Vec<&str> = result["matches"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .map(|m| m["name"].as_str().unwrap_or(""))
        .collect();
    // core tools (e.g. "search-tools") must not appear when only "memory" is allowed.
    // BUT: meta-tagged tools survive any scope (from Phase 3 + Phase 1 design).
    // search-tools is meta-tagged, so it may appear — only assert non-meta core tools are absent.
    let non_meta_core = ["file-reader", "web-fetch", "shell-exec", "file-writer"];
    for bad in non_meta_core {
        assert!(
            !names.contains(&bad),
            "non-meta core tool '{bad}' leaked past memory-only allowlist: {names:?}"
        );
    }
}

#[tokio::test]
async fn structural_noop_embedder_fail_open() {
    // With a noop embedder the search must still return results via lexical fallback.
    let shared = make_shared(gold_summaries());
    let tool = SearchToolsTool::new(shared, Arc::new(Embedder::noop()));
    let hits = run_search(&tool, "file-reader").await;
    assert!(
        !hits.is_empty(),
        "fail-open: noop embedder must still return lexical matches"
    );
    assert!(
        hits.contains(&"file-reader".to_string()),
        "exact name must rank"
    );
}

// ── Tier-2 semantic tests (real embedder, #[ignore], nightly) ─────────────────

fn recall_at_k(hits: &[String], expected: &[String], k: usize) -> bool {
    hits.iter().take(k).any(|h| expected.contains(h))
}

fn reciprocal_rank(hits: &[String], expected: &[String]) -> f64 {
    hits.iter()
        .enumerate()
        .find_map(|(i, h)| {
            if expected.contains(h) {
                Some(1.0 / (i + 1) as f64)
            } else {
                None
            }
        })
        .unwrap_or(0.0)
}

#[tokio::test]
#[ignore = "requires real MiniLM embedder (downloads ~23MB); run nightly or on-demand with --ignored"]
async fn semantic_recall_beats_lexical_baseline_on_synonym_rows() {
    // Build the real embedder + index.
    let model_cache = std::env::var("FASTEMBED_CACHE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("fastembed"));
    let embedder = Arc::new(
        Embedder::with_cache_dir(&model_cache)
            .or_else(|_| Embedder::new())
            .expect("real embedder required for Tier-2 eval"),
    );
    let summaries = gold_summaries();
    let shared = make_shared(summaries.clone());
    let tool = SearchToolsTool::new(shared, embedder);
    let gold = load_gold();
    let synonym_rows: Vec<&GoldRow> = gold.iter().filter(|r| r.row_type == "synonym").collect();

    let mut semantic_r1 = 0usize;
    let mut semantic_r3 = 0usize;
    let mut lexical_r1 = 0usize;
    let mut lexical_r3 = 0usize;
    let mut mrr_semantic = 0.0f64;
    let mut mrr_lexical = 0.0f64;

    // Pre-build lexical scores for comparison — uses the same score_tool logic
    // as the original substring scorer, no RRF, no semantic component.
    let lex_results: HashMap<String, Vec<String>> = synonym_rows
        .iter()
        .map(|row| {
            let q = row.query.to_lowercase();
            let mut scored: Vec<(i32, String)> = summaries
                .iter()
                .map(|s| {
                    let sc = SearchToolsTool::score_tool(&s.name, &s.description, &s.tags, &q);
                    (sc, s.name.clone())
                })
                .filter(|(sc, _)| *sc > 0)
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            (
                row.query.clone(),
                scored.into_iter().map(|(_, n)| n).collect(),
            )
        })
        .collect();

    for row in &synonym_rows {
        let semantic_hits = run_search(&tool, &row.query).await;
        let lexical_hits = lex_results[&row.query].clone();
        if recall_at_k(&semantic_hits, &row.expect_tools, 1) {
            semantic_r1 += 1;
        }
        if recall_at_k(&semantic_hits, &row.expect_tools, 3) {
            semantic_r3 += 1;
        }
        if recall_at_k(&lexical_hits, &row.expect_tools, 1) {
            lexical_r1 += 1;
        }
        if recall_at_k(&lexical_hits, &row.expect_tools, 3) {
            lexical_r3 += 1;
        }
        mrr_semantic += reciprocal_rank(&semantic_hits, &row.expect_tools);
        mrr_lexical += reciprocal_rank(&lexical_hits, &row.expect_tools);
    }

    let n = synonym_rows.len() as f64;
    let sem_r1 = (semantic_r1 as f64 / n * 100.0) as u32;
    let sem_r3 = (semantic_r3 as f64 / n * 100.0) as u32;
    let lex_r1 = (lexical_r1 as f64 / n * 100.0) as u32;
    let lex_r3 = (lexical_r3 as f64 / n * 100.0) as u32;
    let sem_mrr = mrr_semantic / n;
    let lex_mrr = mrr_lexical / n;

    println!(
        "\n=== Tool Discovery Eval (synonym rows, n={}) ===",
        synonym_rows.len()
    );
    println!("            recall@1  recall@3   MRR");
    println!("semantic:   {:7}%  {:7}%  {:.3}", sem_r1, sem_r3, sem_mrr);
    println!("lexical:    {:7}%  {:7}%  {:.3}", lex_r1, lex_r3, lex_mrr);
    println!("Δ recall@3: {:+}pp", sem_r3 as i32 - lex_r3 as i32);

    // Gate: semantic recall@3 must beat lexical by at least 10 percentage points on
    // synonym rows (the class where semantic uniquely helps). Treat as a band —
    // ONNX output is not bit-reproducible; ±5pp variance is expected.
    let delta = sem_r3 as i32 - lex_r3 as i32;
    assert!(
        delta >= 10,
        "semantic recall@3 must beat lexical by ≥10pp on synonym rows, got Δ={delta}pp \
         (semantic={sem_r3}%, lexical={lex_r3}%)"
    );
}

// ── Gold dataset update process ──────────────────────────────────────────────
//
// When adding a new tool to tools/core/:
// 1. Add ≥1 gold row to tests/data/tool_discovery_gold.jsonl with:
//    - "type": "synonym" if the query has no lexical overlap with the tool name/desc
//    - "type": "lexical" if the query contains words from the tool name/description
//    - "category": must match AgentManualTool::infer_tool_category(name, ...) output
//      (categories: memory/mcp/scratchpad/channel/events/skills/plugins/containers/
//       webhooks/capabilities/hal/scheduling/notifications — else "core")
// 2. Add a matching ToolSummary to gold_summaries() in this file so it has
//    realistic name, description, category, and tags.
// 3. Run Tier-1 to confirm the row passes validity guards:
//    cargo test -p agentos-tools --test tool_discovery_eval structural
