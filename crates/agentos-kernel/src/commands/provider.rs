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
        for entry in self.provider_catalog.list() {
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
}
