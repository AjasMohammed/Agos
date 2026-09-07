//! The catalog shipped in `config/providers.toml` is embedded in the binary
//! and is the fallback when no sibling file exists — a TOML typo there breaks
//! provider resolution for every provider at once, so parse it under test.

use agentos_llm::catalog::ProviderCatalog;

const EMBEDDED: &str = include_str!("../../../config/providers.toml");

#[test]
fn embedded_catalog_parses() {
    let catalog = ProviderCatalog::parse(EMBEDDED).expect("embedded providers.toml must parse");
    assert!(catalog.lookup("nvidia").is_some());
    assert!(catalog.lookup("deepseek").is_some());
}

#[test]
fn nvidia_entry_carries_the_nim_overrides() {
    let catalog = ProviderCatalog::parse(EMBEDDED).unwrap();
    let entry = catalog.lookup("nvidia").unwrap();
    // Assert that each knob is *set*, not what it is tuned to — otherwise
    // retuning providers.toml breaks a test in another crate. Without these the
    // adapter assumes a 32K window, lets NIM apply its own low per-model
    // max_tokens default, and gives up on reasoning models after 60s of
    // silence.
    assert!(entry.context_window.is_some());
    assert!(entry.max_output_tokens.is_some());
    assert!(entry.read_timeout_secs.is_some());
    assert!(entry.request_timeout_secs.is_some());
    // A read timeout above the total timeout could never fire.
    assert!(entry.read_timeout_secs <= entry.request_timeout_secs);
    // Behaviour flags, not tuning: NIM speaks the native tool_calls protocol,
    // and it defers long generations to a poll URL that is deliberately on a
    // different host from base_url.
    assert_eq!(entry.supports_native_tool_calling, Some(true));
    let poll = entry.status_url_template.as_deref().unwrap();
    assert!(poll.contains("{id}"), "{poll}");
    assert!(poll.starts_with("https://api.nvcf.nvidia.com/"), "{poll}");
}

#[test]
fn every_entry_defaults_to_a_model_it_lists() {
    let catalog = ProviderCatalog::parse(EMBEDDED).unwrap();
    for entry in catalog.list() {
        if entry.models.is_empty() {
            continue;
        }
        assert!(
            entry.models.contains(&entry.default_model),
            "{}: default_model {:?} is not in its own models list",
            entry.name,
            entry.default_model
        );
    }
}
