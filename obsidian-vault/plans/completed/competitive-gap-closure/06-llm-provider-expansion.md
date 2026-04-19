---
title: "Phase 2.3: LLM Provider Expansion"
tags:
  - llm
  - v3
  - plan
  - phase-2
date: 2026-03-30
status: planned
effort: 3d
priority: high
---

# Phase 2.3: LLM Provider Expansion

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Expand LLM provider support from 5 to 15+ via 5 native adapters (Bedrock, Azure, Groq, Together, Mistral) and a `providers.toml` catalog that auto-configures the existing `CustomCore` adapter for 10+ OpenAI-compatible providers.

**Architecture:** Native adapters implement `LLMCore` directly for providers with non-OpenAI APIs (Bedrock's SigV4, Azure's deployment routing). The provider catalog is a TOML file loaded at boot — each entry maps a provider name to a base URL, API key env var, and default model. `agentctl agent connect --provider deepseek` reads the catalog and creates a `CustomCore` instance.

**Tech Stack:** agentos-llm crate (existing), aws-sigv4 (Bedrock), reqwest, toml

---

## Why This Phase

AgentOS has 5 providers. OpenFang has 26. Most of the gap is OpenAI-compatible providers that just need a different base URL — the `CustomCore` adapter already handles the protocol. The provider catalog closes this gap with almost zero new code.

## Current → Target State

**Current:** 5 providers in `crates/agentos-llm/src/`: OpenAI, Anthropic, Gemini, Ollama, Custom. Adding a new provider requires code changes and a `--base-url` flag.

**Target:** 15+ providers. `agentctl agent connect --provider deepseek --model deepseek-chat` just works. `agentctl provider list` shows all available providers.

## Files Changed

| File | Action | Purpose |
|------|--------|---------|
| `config/providers.toml` | Create | Provider catalog (10+ entries) |
| `crates/agentos-llm/src/catalog.rs` | Create | Catalog loader and provider lookup |
| `crates/agentos-llm/src/bedrock.rs` | Create | AWS Bedrock adapter |
| `crates/agentos-llm/src/azure_openai.rs` | Create | Azure OpenAI adapter |
| `crates/agentos-llm/src/groq.rs` | Create | Groq adapter |
| `crates/agentos-llm/src/together.rs` | Create | Together AI adapter |
| `crates/agentos-llm/src/mistral.rs` | Create | Mistral adapter |
| `crates/agentos-llm/src/lib.rs` | Modify | Add new modules, catalog integration |
| `crates/agentos-llm/src/types.rs` | Modify | Add LLMProvider variants |
| `crates/agentos-bus/src/message.rs` | Modify | Add ProviderList command |
| `crates/agentos-cli/src/commands/provider.rs` | Create | `agentctl provider list` command |

## Dependencies

- **Requires:** Nothing — this is independent
- **Blocks:** Nothing

---

## Detailed Tasks

### Task 1: Provider Catalog

**Files:**
- Create: `config/providers.toml`
- Create: `crates/agentos-llm/src/catalog.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_catalog() {
        let toml_str = r#"
[[provider]]
name = "deepseek"
display_name = "DeepSeek"
base_url = "https://api.deepseek.com"
api_key_env = "DEEPSEEK_API_KEY"
compatible_with = "openai"
default_model = "deepseek-chat"
models = ["deepseek-chat", "deepseek-coder"]
"#;
        let catalog = ProviderCatalog::from_str(toml_str).unwrap();
        assert_eq!(catalog.providers.len(), 1);
        let p = catalog.lookup("deepseek").unwrap();
        assert_eq!(p.base_url, "https://api.deepseek.com");
        assert_eq!(p.compatible_with, "openai");
    }

    #[test]
    fn test_lookup_missing_provider() {
        let catalog = ProviderCatalog::from_str("").unwrap();
        assert!(catalog.lookup("nonexistent").is_none());
    }
}
```

- [ ] **Step 2: Implement catalog**

```rust
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize)]
pub struct CatalogEntry {
    pub name: String,
    pub display_name: String,
    pub base_url: String,
    pub api_key_env: String,
    pub compatible_with: String,
    pub default_model: String,
    #[serde(default)]
    pub models: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct CatalogFile {
    #[serde(default)]
    provider: Vec<CatalogEntry>,
}

pub struct ProviderCatalog {
    pub providers: HashMap<String, CatalogEntry>,
}

impl ProviderCatalog {
    pub fn from_str(toml_str: &str) -> Result<Self, toml::de::Error> {
        let file: CatalogFile = if toml_str.is_empty() {
            CatalogFile { provider: vec![] }
        } else {
            toml::from_str(toml_str)?
        };
        let providers = file.provider.into_iter().map(|p| (p.name.clone(), p)).collect();
        Ok(Self { providers })
    }

    pub fn from_file(path: &std::path::Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        Self::from_str(&content).map_err(|e| e.to_string())
    }

    pub fn lookup(&self, name: &str) -> Option<&CatalogEntry> {
        self.providers.get(name)
    }

    pub fn list(&self) -> Vec<&CatalogEntry> {
        self.providers.values().collect()
    }
}
```

- [ ] **Step 3: Write config/providers.toml with 10 entries**

Include: DeepSeek, Fireworks, Perplexity, OpenRouter, LM Studio, vLLM, Anyscale, Lepton, DeepInfra, SambaNova (full TOML from the design spec).

- [ ] **Step 4: Run tests**

Run: `cargo test -p agentos-llm -- test_load_catalog`

- [ ] **Step 5: Commit**

```bash
git add crates/agentos-llm/src/catalog.rs config/providers.toml
git commit -m "feat(llm): add provider catalog with 10 OpenAI-compatible entries"
```

### Task 2: Wire Catalog into Agent Connection

- [ ] In kernel's agent connect flow, when provider is not a known native variant, check the catalog
- [ ] If found and `compatible_with = "openai"`, create a `CustomCore` with the catalog's `base_url` and API key from env
- [ ] Add `LLMProvider::Catalog(String)` variant to the enum
- [ ] Test: `agentctl agent connect --provider deepseek --model deepseek-chat` creates a working agent (mock test)
- [ ] Commit

### Task 3: Groq Native Adapter

**Files:** `crates/agentos-llm/src/groq.rs`

Groq is OpenAI-compatible but with ultra-low latency. The native adapter adds:
- Groq-specific `x-groq-*` headers for routing
- Correct model catalog (llama, mixtral, gemma variants)
- Accurate token counting and pricing

- [ ] Write failing test: `test_groq_request_format`
- [ ] Implement `GroqCore` (mostly delegating to OpenAI-compatible logic with Groq headers)
- [ ] Add `LLMProvider::Groq` variant
- [ ] Run tests
- [ ] Commit

### Task 4: AWS Bedrock Adapter

**Files:** `crates/agentos-llm/src/bedrock.rs`

Bedrock requires SigV4 signing — not OpenAI-compatible.

- [ ] Write failing test: `test_bedrock_request_signing`
- [ ] Implement `BedrockCore` with AWS SigV4 auth, Converse API format, streaming
- [ ] Add `LLMProvider::Bedrock` variant
- [ ] Run tests
- [ ] Commit

### Task 5: Azure OpenAI, Together AI, Mistral Adapters

- [ ] `azure_openai.rs`: Azure AD auth, deployment-based URL routing, content filtering headers
- [ ] `together.rs`: OpenAI-compat with Together-specific model catalog and JSON mode
- [ ] `mistral.rs`: Native Mistral API format (similar to OpenAI but different tool calling schema)
- [ ] Add LLMProvider variants for each
- [ ] Run tests
- [ ] Commit

### Task 6: Provider List CLI Command

- [ ] Create `crates/agentos-cli/src/commands/provider.rs` with `agentctl provider list`
- [ ] Shows: native providers (Built-in) + catalog providers (Catalog) with status indicators
- [ ] Commit

## Verification

```bash
cargo build --workspace
cargo test -p agentos-llm
cargo clippy -p agentos-llm -- -D warnings
# Check provider count:
# agentctl provider list | wc -l  # Should show 15+
```
