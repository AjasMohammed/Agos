//! Phase 7 — E2E / integration tests for the Proactive Personalization subsystem.
//!
//! These tests do NOT boot a full kernel — they exercise stores and pure
//! functions directly to avoid heavyweight setup and flaky serial ordering.
//! They verify the behavioral contracts that span multiple phases:
//!
//! 1. `disabled_by_default_no_l0_block` — `PersonalizationConfig::default().enabled == false`
//!    and the render path returns nothing when disabled.
//! 2. `profile_entry_promoted_and_rendered` — upsert + pin + list_pinned + render
//!    chain returns a well-formed L0 block containing the entry.
//! 3. `render_block_within_token_budget` — 12 overlong entries respect the 300-token cap.
//! 4. `interest_model_not_in_context_path` — `context_compiler.rs` must not reference
//!    `interest_model` or `user_interests` (coupling guardrail).
//! 5. `opt_in_default_off_all_phases` — config defaults pin the opt-in contract.

use agentos_kernel::{
    config::{PersonalizationConfig, UserProfileConfig},
    context_compiler::render_user_profile_block,
    user_profile_store::{UserProfileStore, UNPINNED_RANK},
};
use agentos_types::{
    ProfileCategory, ProfileEntry, ProfileEntryID, ProfileEntryStatus, ProfileSource,
};

fn make_entry(key: &str, value: &str, confidence: f32, pin_rank: i64) -> ProfileEntry {
    let now = chrono::Utc::now();
    ProfileEntry {
        id: ProfileEntryID::new(),
        category: ProfileCategory::Other,
        key: key.to_string(),
        value: value.to_string(),
        confidence,
        source: ProfileSource::Explicit,
        pin_rank,
        usage_count: 0,
        last_used: None,
        created_at: now,
        updated_at: now,
        status: ProfileEntryStatus::Active,
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Test 1: disabled_by_default_no_l0_block
// ──────────────────────────────────────────────────────────────────────────────

/// Asserts that `PersonalizationConfig::default().enabled` is `false` AND that
/// the render path returns `None` when given an empty slice (simulating the
/// task_executor's behavior when `enabled == false`: it skips `list_pinned` and
/// passes `&[]` to the renderer).
///
/// This pins the opt-in contract: existing deployments that have never set
/// `[personalization] enabled = true` must receive zero L0 context.
#[test]
fn disabled_by_default_no_l0_block() {
    let cfg = PersonalizationConfig::default();
    assert!(
        !cfg.enabled,
        "PersonalizationConfig::default().enabled must be false (opt-in, fail-closed)"
    );

    // When the task_executor sees `!cfg.enabled` it does NOT call `list_pinned()`
    // and passes an empty slice to the renderer. Assert that an empty slice
    // produces None (no block injected into context).
    let block = render_user_profile_block(&[], cfg.profile_token_budget, 4.0);
    assert!(
        block.is_none(),
        "render_user_profile_block(&[]) must return None (no entries → no block)"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Test 2: profile_entry_promoted_and_rendered
// ──────────────────────────────────────────────────────────────────────────────

/// Asserts the full upsert → pin → list_pinned → render chain. A profile entry
/// that is upserted, pinned, and then read back via `list_pinned` must appear in
/// the rendered L0 block.
///
/// This is the "accept → promote → inject" part of the E2E loop: the acceptance
/// side (proposal accept → upsert) is exercised by directly calling `upsert` with
/// a `FromProposal` source, which is what `commands/personalization.rs` does after
/// the user accepts a proposal.
#[tokio::test]
async fn profile_entry_promoted_and_rendered() {
    let dir = tempfile::tempdir().unwrap();
    let store = UserProfileStore::open(dir.path().join("profile.db"))
        .await
        .unwrap();
    Box::leak(Box::new(dir));

    // Simulate proposal acceptance: upsert with FromProposal source.
    let entry = ProfileEntry {
        id: ProfileEntryID::new(),
        category: ProfileCategory::TechStack,
        key: "preferred_language".to_string(),
        value: "Rust".to_string(),
        confidence: 0.9,
        source: ProfileSource::FromProposal {
            proposal_id: "test-proposal-123".to_string(),
        },
        pin_rank: UNPINNED_RANK,
        usage_count: 0,
        last_used: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        status: ProfileEntryStatus::Active,
    };
    let id = entry.id.to_string();
    store.upsert(entry).await.unwrap();
    store.pin(&id).await.unwrap();

    let pinned = store.list_pinned().await.unwrap();
    assert_eq!(pinned.len(), 1, "one pinned entry after pin()");
    assert_eq!(pinned[0].key, "preferred_language");

    // Render the L0 block from the pinned list (as context_compiler would).
    let block = render_user_profile_block(&pinned, 300, 4.0)
        .expect("non-empty pinned list must render a block");

    assert!(
        block.contains("Rust"),
        "rendered L0 block must contain the entry value"
    );
    assert!(
        block.contains("<user_profile>"),
        "block must be wrapped in <user_profile> tag"
    );
    assert!(
        block.trim_end().ends_with("</user_profile>"),
        "block must close with </user_profile> tag"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Test 3: render_block_within_token_budget
// ──────────────────────────────────────────────────────────────────────────────

/// Asserts the Phase 2 token-budget enforcement: 12 entries with very long
/// values (100+ chars each) must still produce a block ≤ 300 tokens using
/// the `chars / 4.0` estimator that production accounting uses.
///
/// This is a budget regression test — if the truncation logic is broken,
/// overlong entries will bloat the system prompt and push tool definitions
/// out of the Anthropic cache breakpoint.
#[test]
fn render_block_within_token_budget() {
    let token_budget = 300usize;
    let chars_per_token = 4.0_f32;

    let entries: Vec<ProfileEntry> = (0..12)
        .map(|i| {
            let mut e = make_entry(
                &format!("key_{i}"),
                &"x".repeat(120), // 120-char value — well over budget if all included
                0.9,
                i as i64,
            );
            e.category = ProfileCategory::CommunicationStyle;
            e
        })
        .collect();

    let block = render_user_profile_block(&entries, token_budget, chars_per_token)
        .expect("non-empty entries must render a block");

    // Phase 2 token estimator: (chars / chars_per_token).ceil() + 1 per entry.
    // We just assert the total block char count respects the budget.
    let max_chars = (token_budget as f32 * chars_per_token) as usize;
    assert!(
        block.chars().count() <= max_chars,
        "block ({} chars) must be within budget ({} chars = {} tokens × {})",
        block.chars().count(),
        max_chars,
        token_budget,
        chars_per_token
    );

    assert!(
        block.trim_end().ends_with("</user_profile>"),
        "closing </user_profile> tag must always be present"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Test 4: interest_model_not_in_context_path
// ──────────────────────────────────────────────────────────────────────────────

/// Code-level coupling guardrail: `context_compiler.rs` must NOT import or
/// reference the `interest_model` or `user_interests` modules. The interest
/// model is a background L2 aggregator with zero task-context cost; pulling it
/// into the context-compilation path would create an implicit coupling that
/// makes the system prompt sensitive to background aggregation timing.
///
/// Uses `include_str!` to embed the source at compile time — the assertion is
/// evaluated at test runtime but the content is resolved at compile time, so
/// a rename of the file path would produce a compile error rather than a
/// false-positive.
#[test]
fn interest_model_not_in_context_path() {
    // include_str! is resolved at compile time relative to the crate root.
    let context_compiler_src = include_str!("../../src/context_compiler.rs");

    assert!(
        !context_compiler_src.contains("interest_model"),
        "context_compiler.rs must not reference `interest_model` — \
         the interest model is a background L2 aggregator and must not \
         be coupled into the context-compilation (L0) path"
    );
    assert!(
        !context_compiler_src.contains("user_interests"),
        "context_compiler.rs must not reference `user_interests` — \
         the interest store is a background-only component"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Test 5: opt_in_default_off_all_phases
// ──────────────────────────────────────────────────────────────────────────────

/// Pins the three-layer opt-in contract:
///
/// - `PersonalizationConfig::default().enabled == false` — L0 read-back is OFF
/// - `PersonalizationConfig::default().proactive_enabled == false` — recommendations are OFF
/// - `UserProfileConfig::default().enabled == true` — the store itself is enabled
///   (it harmlessly holds promoted prefs when empty; the context-injection gate
///   is the separate `PersonalizationConfig.enabled` flag)
///
/// Any regression here silently enables personalization for all deployments
/// that never set the flag.
#[test]
fn opt_in_default_off_all_phases() {
    let pcfg = PersonalizationConfig::default();
    let ucfg = UserProfileConfig::default();

    assert!(
        !pcfg.enabled,
        "PersonalizationConfig::default().enabled must be false (L0 context injection opt-in)"
    );
    assert!(
        !pcfg.proactive_enabled,
        "PersonalizationConfig::default().proactive_enabled must be false (recommendation opt-in)"
    );
    assert!(
        ucfg.enabled,
        "UserProfileConfig::default().enabled must be true \
         (the store is harmlessly empty; only context injection is off by default)"
    );
}
