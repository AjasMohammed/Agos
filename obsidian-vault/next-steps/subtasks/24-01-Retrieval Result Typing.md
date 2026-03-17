---
title: Retrieval Result Typing
tags:
  - kernel
  - memory
  - reliability
  - next-steps
  - v3
date: 2026-03-17
status: complete
effort: 4h
priority: critical
---

# Retrieval Result Typing

> Replace the silent `Err(_) => Vec::new()` pattern in `RetrievalExecutor` with a typed outcome enum that distinguishes "no data found" from "search infrastructure error".

---

## Why This Subtask

`RetrievalExecutor::execute()` in `crates/agentos-kernel/src/retrieval_gate.rs` spawns parallel searches against semantic, episodic, procedural, and tool stores. Every error branch silently converts to an empty vec:

- Line 296: `Err(_) => Vec::new()` (semantic)
- Line 323: `_ => Vec::new()` (episodic -- covers both JoinError and search error)
- Line 361: `Err(_) => Vec::new()` (procedural)

This means the caller (`task_executor.rs` line 385) cannot tell whether the stores were empty or broken. Currently it emits `MemorySearchFailed` for both cases, causing 100% task failure for new agents.

## Current -> Target State

| Aspect | Current | Target |
|--------|---------|--------|
| Return type of `execute()` | `Vec<RetrievalResult>` | `RetrievalOutcome` (new enum) |
| Error handling in spawned tasks | `Err(_) => Vec::new()` | `Err(e) => { warn; push to errors vec }` |
| Caller distinction | None -- empty vec means failure | `RetrievalOutcome::NoData` vs `RetrievalOutcome::Results(vec)` vs `RetrievalOutcome::SearchError { results, errors }` |

## What to Do

1. Open `crates/agentos-kernel/src/retrieval_gate.rs`

2. Add a new enum after the existing `RetrievalResult` struct (around line 62):

```rust
/// Outcome of a retrieval execution, distinguishing empty stores from errors.
#[derive(Debug)]
pub enum RetrievalOutcome {
    /// All stores returned successfully but had no matching data.
    NoData,
    /// At least one store returned results.
    Results(Vec<RetrievalResult>),
    /// One or more stores had infrastructure errors; partial results may still be included.
    SearchError {
        results: Vec<RetrievalResult>,
        errors: Vec<String>,
    },
}

// NOTE: Implemented as `SearchError` (not `SearchError` as originally planned).
// `SearchError` more accurately reflects that this variant indicates an infrastructure
// failure, regardless of whether partial results are present.

impl RetrievalOutcome {
    /// Returns true if no results were found (either empty or all errors).
    pub fn is_empty(&self) -> bool {
        match self {
            Self::NoData => true,
            Self::Results(r) => r.is_empty(),
            Self::SearchError { results, .. } => results.is_empty(),
        }
    }

    /// Extract the results vec regardless of outcome variant.
    pub fn into_results(self) -> Vec<RetrievalResult> {
        match self {
            Self::NoData => Vec::new(),
            Self::Results(r) => r,
            Self::SearchError { results, .. } => results,
        }
    }

    /// Returns true if any search backend returned an error.
    pub fn has_errors(&self) -> bool {
        matches!(self, Self::SearchError { .. })
    }

    /// Get error descriptions, if any.
    pub fn errors(&self) -> &[String] {
        match self {
            Self::SearchError { errors, .. } => errors,
            _ => &[],
        }
    }
}
```

3. Change the `execute()` method signature (line 264) from:
```rust
pub async fn execute(&self, plan: &RetrievalPlan, agent_id: Option<&AgentID>) -> Vec<RetrievalResult>
```
to:
```rust
pub async fn execute(&self, plan: &RetrievalPlan, agent_id: Option<&AgentID>) -> RetrievalOutcome
```

4. Inside `execute()`, replace the spawned task error handling. For the semantic branch (around line 282):
```rust
// Before:
Err(_) => Vec::new(),
// After:
Err(e) => {
    tracing::warn!(error = %e, "Semantic memory search failed");
    return Err(format!("semantic: {}", e));
}
```
Change the return type of each spawned task from `Vec<RetrievalResult>` to `Result<Vec<RetrievalResult>, String>`. Apply the same pattern to episodic (line 310-323) and procedural (line 333-361) branches.

5. In the merge section at the bottom of `execute()` (lines 411-428), collect results and errors:
```rust
let mut merged = Vec::new();
let mut errors = Vec::new();
for handle in handles {
    match handle.await {
        Ok(Ok(results)) => merged.extend(results),
        Ok(Err(err_msg)) => errors.push(err_msg),
        Err(join_err) => errors.push(format!("task join: {}", join_err)),
    }
}

// Deduplicate and sort as before
merged.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
let mut seen = HashSet::new();
let deduped: Vec<RetrievalResult> = merged.into_iter().filter(|r| seen.insert(r.content_hash())).collect();

if deduped.is_empty() && errors.is_empty() {
    RetrievalOutcome::NoData
} else if errors.is_empty() {
    RetrievalOutcome::Results(deduped)
} else if deduped.is_empty() {
    RetrievalOutcome::SearchError { results: Vec::new(), errors }
} else {
    RetrievalOutcome::SearchError { results: deduped, errors }
}
```

6. Update `format_as_knowledge_blocks` to accept `&[RetrievalResult]` (no change needed -- it already does).

7. Add unit tests:

```rust
#[test]
fn retrieval_outcome_no_data_is_empty() {
    assert!(RetrievalOutcome::NoData.is_empty());
    assert!(!RetrievalOutcome::NoData.has_errors());
}

#[test]
fn retrieval_outcome_results_not_empty() {
    let r = RetrievalOutcome::Results(vec![RetrievalResult {
        source: IndexType::Semantic,
        content: "test".to_string(),
        score: 0.5,
        metadata: None,
    }]);
    assert!(!r.is_empty());
    assert!(!r.has_errors());
}

#[test]
fn retrieval_outcome_partial_error_reports_errors() {
    let r = RetrievalOutcome::SearchError {
        results: Vec::new(),
        errors: vec!["semantic: connection refused".to_string()],
    };
    assert!(r.is_empty());
    assert!(r.has_errors());
    assert_eq!(r.errors().len(), 1);
}
```

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/retrieval_gate.rs` | Add `RetrievalOutcome` enum; change `execute()` return type; replace silent error swallowing with typed errors |

## Prerequisites

None -- this is the first subtask.

## Test Plan

- `cargo test -p agentos-kernel -- retrieval` -- all existing tests pass
- New tests: `retrieval_outcome_no_data_is_empty`, `retrieval_outcome_results_not_empty`, `retrieval_outcome_partial_error_reports_errors`
- Verify the code compiles: the next subtask (24-02) will update callers

## Verification

```bash
cargo build -p agentos-kernel
cargo test -p agentos-kernel -- retrieval --nocapture
cargo clippy -p agentos-kernel -- -D warnings
```
