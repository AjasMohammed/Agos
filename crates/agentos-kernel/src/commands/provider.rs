use crate::kernel::Kernel;
use agentos_bus::KernelResponse;
use serde_json::json;

impl Kernel {
    /// List all available LLM providers: built-in (native) providers and
    /// catalog providers loaded from `config/providers.toml`.
    pub(crate) async fn cmd_list_providers(&self) -> KernelResponse {
        let mut entries = Vec::new();

        // Built-in providers
        let builtins = [
            ("openai", "OpenAI", "OPENAI_API_KEY"),
            ("anthropic", "Anthropic", "ANTHROPIC_API_KEY"),
            ("gemini", "Gemini", "GEMINI_API_KEY"),
            ("ollama", "Ollama (local)", ""),
        ];

        for (name, display_name, api_key_env) in &builtins {
            let key_set = if api_key_env.is_empty() {
                true // Local providers don't need an API key
            } else {
                std::env::var(api_key_env)
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .is_some()
            };
            entries.push(json!({
                "name": name,
                "display_name": display_name,
                "source": "built-in",
                "api_key_env": api_key_env,
                "api_key_set": key_set,
                "default_model": "",
            }));
        }

        // Catalog providers
        let catalog_entries: Vec<agentos_llm::CatalogEntry> = self
            .provider_catalog
            .read()
            .unwrap()
            .list()
            .into_iter()
            .cloned()
            .collect();
        for entry in &catalog_entries {
            let key_set = if entry.api_key_env.is_empty() {
                true // Local providers don't need an API key
            } else {
                std::env::var(&entry.api_key_env)
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .is_some()
            };
            entries.push(json!({
                "name": entry.name,
                "display_name": entry.display_name,
                "source": "catalog",
                "api_key_env": entry.api_key_env,
                "api_key_set": key_set,
                "default_model": entry.default_model,
                "compatible_with": entry.compatible_with,
                "models": entry.models,
            }));
        }

        KernelResponse::ProviderList(entries)
    }

    /// Update the base URL for a named catalog provider, persisting the change
    /// back to `providers.toml`.
    pub(crate) async fn cmd_set_provider_url(&self, name: String, url: String) -> KernelResponse {
        // Update in-memory catalog
        let updated = self
            .provider_catalog
            .write()
            .unwrap()
            .set_base_url(&name, url.clone());

        if !updated {
            return KernelResponse::Error {
                message: format!(
                    "Provider '{}' not found in catalog. Run 'agentos provider list' to see available providers.",
                    name
                ),
            };
        }

        // Persist to file
        if let Some(path) = &self.catalog_path {
            let catalog_snapshot = self.provider_catalog.read().unwrap().clone_inner();
            let path = path.clone();
            let result =
                tokio::task::spawn_blocking(move || catalog_snapshot.save_to_file(&path)).await;

            match result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    return KernelResponse::Error {
                        message: format!("URL updated in memory but could not save to file: {}", e),
                    }
                }
                Err(e) => {
                    return KernelResponse::Error {
                        message: format!(
                            "URL updated in memory but file write task panicked: {}",
                            e
                        ),
                    }
                }
            }
        }

        tracing::info!(provider = %name, url = %url, "Provider base URL updated");
        KernelResponse::Success { data: None }
    }
}
