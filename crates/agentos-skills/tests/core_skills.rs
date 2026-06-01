//! Verifies that every skill bundled under `skills/core/` parses and loads.
//!
//! These skills are embedded into the `agentos` binary via `rust-embed` and
//! extracted to the data dir on first run, so a malformed `SKILL.toml` or a
//! missing prompt file would break a fresh install. This test catches that at
//! `cargo test` time.

use agentos_skills::SkillRegistry;
use std::path::PathBuf;

fn core_skills_dir() -> PathBuf {
    // crates/agentos-skills/ -> repo root -> skills/core
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../skills/core")
        .canonicalize()
        .expect("skills/core directory should exist")
}

#[test]
fn all_core_skills_load() {
    let dir = core_skills_dir();
    let mut registry = SkillRegistry::new();
    let loaded = registry
        .load_from_dir(&dir)
        .expect("loading core skills should not error");

    // Every subdirectory with a SKILL.toml must load successfully.
    let subdir_count = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir() && e.path().join("SKILL.toml").exists())
        .count();

    assert_eq!(
        loaded, subdir_count,
        "every skill dir under skills/core must load (loaded {loaded} of {subdir_count})"
    );
    assert!(
        loaded >= 13,
        "expected the full bundled skill set, got {loaded}"
    );
}

#[test]
fn ecosystem_skills_present_and_core_tier() {
    let dir = core_skills_dir();
    let mut registry = SkillRegistry::new();
    registry.load_from_dir(&dir).unwrap();

    for name in [
        "memory-curator",
        "task-orchestrator",
        "scratchpad-organizer",
        "automation-scheduler",
        "tool-navigator",
    ] {
        let skill = registry
            .get(name)
            .unwrap_or_else(|| panic!("ecosystem skill '{name}' should be installed"));
        assert_eq!(
            skill.manifest.skill.trust_tier, "core",
            "ecosystem skill '{name}' must be core trust tier"
        );
        assert!(
            !skill.system_prompt.trim().is_empty(),
            "ecosystem skill '{name}' must have a non-empty system prompt"
        );
    }
}
