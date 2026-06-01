//! Semantic index for `search-tools`.
//!
//! Generalises the manual-section embedding idiom (`SemanticIndex` /
//! `semantic_suggest_async` in [`crate::agent_manual`]) to the **dynamic** tool
//! catalogue. Unlike the section index — which is a static `OnceLock` built once
//! at boot — the tool set changes at runtime (tool install/removal, MCP attach),
//! so this index rebuilds itself **lazily** whenever it notices the shared tool
//! summaries changed (detected via a cheap signature). That keeps it from ever
//! being stale at query time without threading an explicit rebuild hook through
//! every registry-mutation site.
//!
//! Fail-open by design: when the embedder is a no-op (`memory.disable_embedder`)
//! or an embed fails, the cache is left empty and `semantic_rank` returns
//! nothing, so `search-tools` falls back to its substring scorer.

use crate::agent_manual::{cosine, ToolSummary};
use agentos_memory::Embedder;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Cosine scores below this are essentially random for MiniLM. Mirrors the
/// section-suggester floor (`agent_manual.rs`). Tuned empirically in the
/// Phase-6 eval harness.
const COSINE_FLOOR: f32 = 0.2;

struct IndexCache {
    /// Signature of the summary set the embeddings were built from. `0` is the
    /// "never built" sentinel, forcing the first `ensure_fresh` to build.
    sig: u64,
    /// `(tool name, embedding)`. Empty when the embedder is a no-op or a rebuild
    /// failed — callers then rely on the keyword scorer.
    entries: Vec<(String, Vec<f32>)>,
}

/// Lazily-refreshing semantic index over the tool catalogue.
pub struct ToolSearchIndex {
    embedder: Arc<Embedder>,
    cache: RwLock<IndexCache>,
}

impl ToolSearchIndex {
    pub fn new(embedder: Arc<Embedder>) -> Self {
        Self {
            embedder,
            cache: RwLock::new(IndexCache {
                sig: 0,
                entries: Vec::new(),
            }),
        }
    }

    /// Content signature of the summary set. Includes name + description so a
    /// description edit (which changes the embedding text) also triggers a
    /// rebuild.
    fn signature(summaries: &[ToolSummary]) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        summaries.len().hash(&mut h);
        // Hash EXACTLY the fields that compose the embedding text in
        // `ensure_fresh` (name + description + capability_tags). If these
        // diverge, a field-only edit would leave the signature unchanged and the
        // index would serve embeddings built from the stale text.
        for s in summaries {
            s.name.hash(&mut h);
            s.description.hash(&mut h);
            s.capability_tags.hash(&mut h);
        }
        // Avoid colliding with the `0` "never built" sentinel.
        h.finish() | 1
    }

    /// Rebuild the embedding cache if the summaries changed since last time.
    /// Embeds the whole corpus on a blocking thread (one-off, tens of ms for a
    /// few hundred tools); cheap no-op when nothing changed.
    async fn ensure_fresh(&self, summaries: &[ToolSummary]) {
        let sig = Self::signature(summaries);
        {
            let cache = self.cache.read().await;
            if cache.sig == sig {
                return;
            }
        }
        if self.embedder.is_noop() || summaries.is_empty() {
            *self.cache.write().await = IndexCache {
                sig,
                entries: Vec::new(),
            };
            return;
        }
        let texts: Vec<String> = summaries
            .iter()
            .map(|s| {
                format!(
                    "{}. {}. {}",
                    s.name,
                    s.description,
                    s.capability_tags.join(" ")
                )
            })
            .collect();
        let names: Vec<String> = summaries.iter().map(|s| s.name.clone()).collect();
        let embedder = Arc::clone(&self.embedder);
        // The MiniLM forward pass is synchronous + CPU-bound — never run it on
        // the async worker (see agent_manual::semantic_suggest_async).
        let embeddings = tokio::task::spawn_blocking(move || {
            let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
            embedder.embed(&refs).ok()
        })
        .await
        .ok()
        .flatten();
        let entries = match embeddings {
            Some(vs) if vs.len() == names.len() => names.into_iter().zip(vs).collect(),
            // Fail-open: an embed failure or length mismatch leaves the index
            // empty so search-tools uses the keyword scorer.
            _ => Vec::new(),
        };
        // A concurrent query may have embedded the same `sig` while we were
        // working (benign — same result). Re-check under the write guard to skip
        // the redundant write. Note the embed itself stays *outside* the guard so
        // it never runs while holding the lock.
        let mut guard = self.cache.write().await;
        if guard.sig != sig {
            *guard = IndexCache { sig, entries };
        }
    }

    /// Rank tool names by cosine similarity to `query`, restricted to `allowed`
    /// names when set. Best-first. Empty when semantic ranking is unavailable
    /// (no-op embedder or query-embed failure) — the caller then falls back to
    /// the keyword scorer.
    pub async fn semantic_rank(
        &self,
        summaries: &[ToolSummary],
        query: &str,
        allowed: Option<&HashSet<String>>,
        max: usize,
    ) -> Vec<(String, f32)> {
        if max == 0 || query.trim().is_empty() {
            return Vec::new();
        }
        self.ensure_fresh(summaries).await;
        if self.embedder.is_noop() {
            return Vec::new();
        }
        // Embed the query off the async worker. Do NOT hold the cache lock here.
        let embedder = Arc::clone(&self.embedder);
        let q = query.to_string();
        let qv = tokio::task::spawn_blocking(move || {
            embedder.embed(&[q.as_str()]).ok().and_then(|mut v| v.pop())
        })
        .await
        .ok()
        .flatten();
        let Some(qv) = qv else {
            return Vec::new();
        };
        // Re-acquire the cache only for the synchronous cosine pass — no `.await`
        // is held across the lock guard.
        let cache = self.cache.read().await;
        rank_by_cosine(&cache.entries, &qv, allowed, max)
    }
}

/// Pure cosine ranking over a precomputed corpus. Factored out so the ranking /
/// floor / dedup / ordering logic is unit-testable without loading an embedder.
pub(crate) fn rank_by_cosine(
    entries: &[(String, Vec<f32>)],
    query_vec: &[f32],
    allowed: Option<&HashSet<String>>,
    max: usize,
) -> Vec<(String, f32)> {
    let mut scored: Vec<(f32, &str)> = entries
        .iter()
        .filter(|(name, _)| allowed.is_none_or(|a| a.contains(name)))
        .map(|(name, emb)| (cosine(query_vec, emb), name.as_str()))
        .filter(|(score, _)| *score >= COSINE_FLOOR)
        .collect();
    // Deterministic total order: score desc, then name asc (mirrors the keyword
    // scorer's tiebreaker in search_tools.rs) so equal scores never flap.
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.cmp(b.1))
    });
    scored
        .into_iter()
        .take(max)
        .map(|(score, name)| (name.to_string(), score))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus() -> Vec<(String, Vec<f32>)> {
        vec![
            ("file-reader".to_string(), vec![1.0, 0.0]),
            ("http-client".to_string(), vec![0.0, 1.0]),
        ]
    }

    #[test]
    fn exact_direction_match_ranks_first() {
        let out = rank_by_cosine(&corpus(), &[1.0, 0.0], None, 5);
        assert_eq!(out[0].0, "file-reader");
        assert!((out[0].1 - 1.0).abs() < 1e-5);
    }

    #[test]
    fn synonym_vector_matches_without_lexical_overlap() {
        // Query close to http-client's direction; far from file-reader's.
        let out = rank_by_cosine(&corpus(), &[0.1, 0.99], None, 5);
        assert_eq!(out[0].0, "http-client");
        // file-reader (cosine ~0.1) is below the floor and excluded.
        assert!(out.iter().all(|(n, _)| n != "file-reader"));
    }

    #[test]
    fn floor_excludes_weak_matches() {
        // Orthogonal query → cosine 0 with both → nothing survives the floor.
        let out = rank_by_cosine(&corpus(), &[0.0, 0.0], None, 5);
        assert!(out.is_empty());
    }

    #[test]
    fn allowlist_restricts_candidates() {
        let allow: HashSet<String> = ["http-client".to_string()].into_iter().collect();
        let out = rank_by_cosine(&corpus(), &[1.0, 0.0], Some(&allow), 5);
        // file-reader would have matched but is not in the allowlist.
        assert!(out.iter().all(|(n, _)| n == "http-client"));
    }

    #[test]
    fn equal_scores_break_ties_by_name() {
        let entries = vec![
            ("zebra".to_string(), vec![1.0, 0.0]),
            ("alpha".to_string(), vec![1.0, 0.0]),
        ];
        let out = rank_by_cosine(&entries, &[1.0, 0.0], None, 5);
        assert_eq!(out[0].0, "alpha");
        assert_eq!(out[1].0, "zebra");
    }

    #[tokio::test]
    async fn noop_embedder_is_fail_open_empty() {
        let idx = ToolSearchIndex::new(Arc::new(Embedder::noop()));
        let summaries: Vec<ToolSummary> = Vec::new();
        let out = idx.semantic_rank(&summaries, "anything", None, 5).await;
        assert!(out.is_empty());
    }

    fn summary(name: &str, desc: &str, caps: &[&str]) -> ToolSummary {
        ToolSummary {
            name: name.into(),
            description: desc.into(),
            version: "1.0.0".into(),
            permissions: vec![],
            payload_schema: None,
            examples: vec![],
            trust_tier: "core".into(),
            capability_tags: caps.iter().map(|s| s.to_string()).collect(),
            category: "core".into(),
            tags: vec![],
            risk_class: "readonly_scoped".into(),
            usage_hints: None,
        }
    }

    #[test]
    fn signature_tracks_capability_tags() {
        // Same name+description but different capability_tags (which feed the
        // embedding text) must yield different signatures — otherwise the index
        // serves vectors built from stale tags. Guards the field-coverage bug.
        let a = vec![summary("file-reader", "Read files", &["io"])];
        let b = vec![summary("file-reader", "Read files", &["io", "disk"])];
        assert_ne!(
            ToolSearchIndex::signature(&a),
            ToolSearchIndex::signature(&b)
        );
    }

    #[test]
    fn signature_is_never_zero_sentinel() {
        assert_ne!(ToolSearchIndex::signature(&[]), 0);
        assert_ne!(ToolSearchIndex::signature(&[summary("a", "b", &[])]), 0);
    }
}
