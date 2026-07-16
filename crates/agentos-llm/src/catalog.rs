use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// A single provider entry from the `providers.toml` catalog.
///
/// All "override" fields are optional; absent values fall back to OpenAI-compatible
/// defaults so existing catalog files continue to parse without modification.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct CatalogEntry {
    pub name: String,
    pub display_name: String,
    pub base_url: String,
    pub api_key_env: String,
    pub compatible_with: String,
    pub default_model: String,
    #[serde(default)]
    pub models: Vec<String>,
    /// Model IDs that accept OpenAI-style `image_url` parts (CustomCore / openai-compat).
    #[serde(default)]
    pub vision_models: Vec<String>,

    // ---- Capability overrides (apply to all models for this provider) ----
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_images: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_tool_calling: Option<bool>,
    /// Whether this provider should use native tool-calling prompt mode
    /// (`tool_calls` protocol) instead of JSON-in-markdown fallback guidance.
    /// Defaults to `None` (treated as false/safe fallback by `CustomCore`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_native_tool_calling: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_streaming: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_prompt_caching: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_json_mode: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_thinking: Option<bool>,

    // ---- Auth / endpoint overrides ----
    /// HTTP header used for auth. Defaults to `Authorization`. Use `api-key`
    /// for Azure-style providers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_header: Option<String>,
    /// Prefix prepended to the API key in the auth header. Defaults to
    /// `"Bearer "`. Use `""` for providers that pass the bare key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_prefix: Option<String>,
    /// Chat completions path appended to `base_url`. Default `/chat/completions`.
    /// May include query string (e.g. `?api-version=2024-08-01`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_path: Option<String>,
    /// Models list path appended to `base_url`. Default `/models`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models_path: Option<String>,

    /// Static extra headers attached to every request (e.g. tenancy IDs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_headers: Option<HashMap<String, String>>,

    /// Permit a private/loopback/link-local `base_url`. Required for legitimate
    /// local providers (lmstudio, ollama, vllm). Catalog validation rejects
    /// private addresses unless this is `Some(true)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_private_hosts: Option<bool>,
}

impl CatalogEntry {
    pub fn auth_header_name(&self) -> &str {
        self.auth_header.as_deref().unwrap_or("Authorization")
    }
    pub fn auth_header_prefix(&self) -> &str {
        self.auth_prefix.as_deref().unwrap_or("Bearer ")
    }
    pub fn chat_path_or_default(&self) -> &str {
        self.chat_path.as_deref().unwrap_or("/chat/completions")
    }
    pub fn models_path_or_default(&self) -> &str {
        self.models_path.as_deref().unwrap_or("/models")
    }
}

/// Parsed provider catalog loaded from a TOML file.
///
/// The catalog maps provider names to their configuration entries, allowing
/// the kernel to auto-configure `CustomCore` instances for OpenAI-compatible
/// providers without requiring manual `--base-url` flags.
#[derive(Debug, Clone)]
pub struct ProviderCatalog {
    providers: HashMap<String, CatalogEntry>,
}

/// Top-level TOML structure: `[[provider]]` array.
#[derive(Debug, Deserialize)]
struct CatalogFile {
    #[serde(default)]
    provider: Vec<CatalogEntry>,
}

impl ProviderCatalog {
    /// Parse a catalog from a TOML string. Keys are normalised to lowercase so
    /// `lookup`, `set_models`, `remove`, etc. all hit a single canonical entry
    /// regardless of how the user cased the `name = "…"` field.
    pub fn parse(toml_str: &str) -> Result<Self, toml::de::Error> {
        let file: CatalogFile = toml::from_str(toml_str)?;
        let mut providers = HashMap::new();
        for entry in file.provider {
            providers.insert(entry.name.to_lowercase(), entry);
        }
        Ok(Self { providers })
    }

    /// Load a catalog from a file path. Returns an empty catalog if the file
    /// does not exist (this is not an error — the catalog is optional).
    pub fn from_file(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self::empty());
        }
        let content = std::fs::read_to_string(path).map_err(|e| {
            format!(
                "Failed to read provider catalog at {}: {}",
                path.display(),
                e
            )
        })?;
        Self::parse(&content).map_err(|e| format!("Failed to parse provider catalog: {}", e))
    }

    /// Parse a catalog from a TOML string. Used for the built-in catalog that
    /// ships embedded in the binary as a fallback when no `providers.toml` is
    /// colocated with the config file.
    pub fn from_toml_str(content: &str) -> Result<Self, String> {
        Self::parse(content).map_err(|e| format!("Failed to parse provider catalog: {}", e))
    }

    /// Create an empty catalog with no providers.
    pub fn empty() -> Self {
        Self {
            providers: HashMap::new(),
        }
    }

    /// Look up a provider by name (case-insensitive).
    pub fn lookup(&self, name: &str) -> Option<&CatalogEntry> {
        self.providers
            .get(&name.to_lowercase())
            .or_else(|| self.providers.get(name))
    }

    /// Return all catalog entries sorted by name.
    pub fn list(&self) -> Vec<&CatalogEntry> {
        let mut entries: Vec<&CatalogEntry> = self.providers.values().collect();
        entries.sort_by_key(|e| &e.name);
        entries
    }

    /// Check if the catalog has no entries.
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// Number of entries in the catalog.
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    /// Return a cloned copy of this catalog (used for snapshot-before-write).
    pub fn clone_inner(&self) -> Self {
        Self {
            providers: self.providers.clone(),
        }
    }

    /// Override the base URL for an existing provider. Returns `true` if the
    /// provider was found and updated, `false` if the name is unknown.
    pub fn set_base_url(&mut self, name: &str, url: String) -> bool {
        let key = name.to_lowercase();
        if let Some(entry) = self.providers.get_mut(&key) {
            entry.base_url = url;
            true
        } else if let Some(entry) = self.providers.get_mut(name) {
            entry.base_url = url;
            true
        } else {
            false
        }
    }

    /// Insert or replace a provider entry. Returns `true` when an existing
    /// entry was replaced, `false` when this is a fresh insert.
    pub fn upsert(&mut self, entry: CatalogEntry) -> bool {
        let key = entry.name.to_lowercase();
        let existed = self.providers.contains_key(&key) || self.providers.contains_key(&entry.name);
        // Drop any case-variant duplicates first.
        self.providers.remove(&entry.name);
        self.providers.insert(key, entry);
        existed
    }

    /// Remove a provider by name. Returns the removed entry if present.
    pub fn remove(&mut self, name: &str) -> Option<CatalogEntry> {
        let key = name.to_lowercase();
        self.providers
            .remove(&key)
            .or_else(|| self.providers.remove(name))
    }

    /// Replace the `models` array of an existing entry. Used by the
    /// `/models` auto-probe. Returns `true` on success.
    pub fn set_models(&mut self, name: &str, models: Vec<String>) -> bool {
        let key = name.to_lowercase();
        if let Some(entry) = self.providers.get_mut(&key) {
            entry.models = models;
            true
        } else if let Some(entry) = self.providers.get_mut(name) {
            entry.models = models;
            true
        } else {
            false
        }
    }

    /// Serialize the catalog back to TOML and write it to `path`.
    pub fn save_to_file(&self, path: &std::path::Path) -> Result<(), String> {
        let mut lines = String::new();
        // Sort entries for stable output
        let mut entries: Vec<&CatalogEntry> = self.providers.values().collect();
        entries.sort_by_key(|e| &e.name);
        for entry in entries {
            lines.push_str("[[provider]]\n");
            lines.push_str(&format!("name = {:?}\n", entry.name));
            lines.push_str(&format!("display_name = {:?}\n", entry.display_name));
            lines.push_str(&format!("base_url = {:?}\n", entry.base_url));
            lines.push_str(&format!("api_key_env = {:?}\n", entry.api_key_env));
            lines.push_str(&format!("compatible_with = {:?}\n", entry.compatible_with));
            lines.push_str(&format!("default_model = {:?}\n", entry.default_model));
            if !entry.models.is_empty() {
                let models_str = entry
                    .models
                    .iter()
                    .map(|m| format!("{:?}", m))
                    .collect::<Vec<_>>()
                    .join(", ");
                lines.push_str(&format!("models = [{}]\n", models_str));
            }
            if !entry.vision_models.is_empty() {
                let vm_str = entry
                    .vision_models
                    .iter()
                    .map(|m| format!("{:?}", m))
                    .collect::<Vec<_>>()
                    .join(", ");
                lines.push_str(&format!("vision_models = [{}]\n", vm_str));
            }
            // Optional capability + auth + path overrides.
            if let Some(v) = entry.context_window {
                lines.push_str(&format!("context_window = {}\n", v));
            }
            if let Some(v) = entry.max_output_tokens {
                lines.push_str(&format!("max_output_tokens = {}\n", v));
            }
            if let Some(v) = entry.supports_images {
                lines.push_str(&format!("supports_images = {}\n", v));
            }
            if let Some(v) = entry.supports_tool_calling {
                lines.push_str(&format!("supports_tool_calling = {}\n", v));
            }
            if let Some(v) = entry.supports_native_tool_calling {
                lines.push_str(&format!("supports_native_tool_calling = {}\n", v));
            }
            if let Some(v) = entry.supports_streaming {
                lines.push_str(&format!("supports_streaming = {}\n", v));
            }
            if let Some(v) = entry.supports_prompt_caching {
                lines.push_str(&format!("supports_prompt_caching = {}\n", v));
            }
            if let Some(v) = entry.supports_json_mode {
                lines.push_str(&format!("supports_json_mode = {}\n", v));
            }
            if let Some(v) = entry.supports_thinking {
                lines.push_str(&format!("supports_thinking = {}\n", v));
            }
            if let Some(v) = &entry.auth_header {
                lines.push_str(&format!("auth_header = {:?}\n", v));
            }
            if let Some(v) = &entry.auth_prefix {
                lines.push_str(&format!("auth_prefix = {:?}\n", v));
            }
            if let Some(v) = &entry.chat_path {
                lines.push_str(&format!("chat_path = {:?}\n", v));
            }
            if let Some(v) = &entry.models_path {
                lines.push_str(&format!("models_path = {:?}\n", v));
            }
            if let Some(map) = &entry.extra_headers {
                if !map.is_empty() {
                    let mut keys: Vec<&String> = map.keys().collect();
                    keys.sort();
                    let body = keys
                        .iter()
                        .map(|k| format!("{:?} = {:?}", k, map[*k]))
                        .collect::<Vec<_>>()
                        .join(", ");
                    lines.push_str(&format!("extra_headers = {{ {} }}\n", body));
                }
            }
            if let Some(v) = entry.allow_private_hosts {
                lines.push_str(&format!("allow_private_hosts = {}\n", v));
            }
            lines.push('\n');
        }
        // Atomic write: stage to a sibling `.tmp` file then rename. A crash
        // mid-write leaves the original `providers.toml` intact rather than
        // truncated.
        let tmp_path = path.with_extension("toml.tmp");
        std::fs::write(&tmp_path, lines)
            .map_err(|e| format!("Failed to write provider catalog (tmp): {}", e))?;
        std::fs::rename(&tmp_path, path).map_err(|e| {
            // Best-effort cleanup of the staged file.
            let _ = std::fs::remove_file(&tmp_path);
            format!("Failed to rename provider catalog into place: {}", e)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_TOML: &str = r#"
[[provider]]
name = "deepseek"
display_name = "DeepSeek"
base_url = "https://api.deepseek.com"
api_key_env = "DEEPSEEK_API_KEY"
compatible_with = "openai"
default_model = "deepseek-chat"
models = ["deepseek-chat", "deepseek-coder"]

[[provider]]
name = "groq"
display_name = "Groq"
base_url = "https://api.groq.com/openai/v1"
api_key_env = "GROQ_API_KEY"
compatible_with = "openai"
default_model = "llama-3.3-70b-versatile"
models = ["llama-3.3-70b-versatile", "mixtral-8x7b-32768"]

[[provider]]
name = "lmstudio"
display_name = "LM Studio"
base_url = "http://localhost:1234/v1"
api_key_env = ""
compatible_with = "openai"
default_model = "local"
models = ["local"]
"#;

    #[test]
    fn test_parse_catalog() {
        let catalog = ProviderCatalog::parse(TEST_TOML).expect("should parse");
        assert_eq!(catalog.len(), 3);
        assert!(!catalog.is_empty());
    }

    #[test]
    fn test_lookup_existing() {
        let catalog = ProviderCatalog::parse(TEST_TOML).expect("should parse");
        let entry = catalog.lookup("deepseek").expect("should find deepseek");
        assert_eq!(entry.display_name, "DeepSeek");
        assert_eq!(entry.base_url, "https://api.deepseek.com");
        assert_eq!(entry.default_model, "deepseek-chat");
        assert_eq!(entry.models.len(), 2);
    }

    #[test]
    fn test_lookup_missing() {
        let catalog = ProviderCatalog::parse(TEST_TOML).expect("should parse");
        assert!(catalog.lookup("nonexistent").is_none());
    }

    #[test]
    fn test_list_sorted() {
        let catalog = ProviderCatalog::parse(TEST_TOML).expect("should parse");
        let entries = catalog.list();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].name, "deepseek");
        assert_eq!(entries[1].name, "groq");
        assert_eq!(entries[2].name, "lmstudio");
    }

    #[test]
    fn test_empty_catalog() {
        let catalog = ProviderCatalog::empty();
        assert!(catalog.is_empty());
        assert_eq!(catalog.len(), 0);
        assert!(catalog.lookup("anything").is_none());
    }

    #[test]
    fn test_from_file_nonexistent() {
        let catalog = ProviderCatalog::from_file(Path::new("/nonexistent/providers.toml")).unwrap();
        assert!(catalog.is_empty());
    }

    #[test]
    fn test_empty_provider_list() {
        let catalog = ProviderCatalog::parse("").expect("empty string should parse");
        assert!(catalog.is_empty());
    }

    #[test]
    fn test_no_api_key_env() {
        let catalog = ProviderCatalog::parse(TEST_TOML).expect("should parse");
        let lmstudio = catalog.lookup("lmstudio").expect("should find lmstudio");
        assert_eq!(lmstudio.api_key_env, "");
    }

    #[test]
    fn test_default_overrides_used_when_unset() {
        let catalog = ProviderCatalog::parse(TEST_TOML).expect("should parse");
        let entry = catalog.lookup("deepseek").unwrap();
        assert_eq!(entry.auth_header_name(), "Authorization");
        assert_eq!(entry.auth_header_prefix(), "Bearer ");
        assert_eq!(entry.chat_path_or_default(), "/chat/completions");
        assert_eq!(entry.models_path_or_default(), "/models");
    }

    #[test]
    fn test_overrides_parsed() {
        let toml = r#"
[[provider]]
name = "azure"
display_name = "Azure"
base_url = "https://x.openai.azure.com/openai"
api_key_env = "AZURE_KEY"
compatible_with = "openai"
default_model = "gpt-4o"
auth_header = "api-key"
auth_prefix = ""
chat_path = "/deployments/gpt-4o/chat/completions?api-version=2024-08-01-preview"
context_window = 128000
supports_images = true
[provider.extra_headers]
"x-ms-region" = "eastus"
"#;
        let catalog = ProviderCatalog::parse(toml).expect("parse");
        let e = catalog.lookup("azure").unwrap();
        assert_eq!(e.auth_header_name(), "api-key");
        assert_eq!(e.auth_header_prefix(), "");
        assert!(e.chat_path_or_default().contains("api-version"));
        assert_eq!(e.context_window, Some(128000));
        assert_eq!(e.supports_images, Some(true));
        let hdrs = e.extra_headers.as_ref().unwrap();
        assert_eq!(hdrs.get("x-ms-region").map(String::as_str), Some("eastus"));
    }

    #[test]
    fn test_upsert_and_remove() {
        let mut catalog = ProviderCatalog::empty();
        let entry = CatalogEntry {
            name: "foo".into(),
            display_name: "Foo".into(),
            base_url: "https://foo".into(),
            api_key_env: "FOO_KEY".into(),
            compatible_with: "openai".into(),
            default_model: "f1".into(),
            ..Default::default()
        };
        assert!(!catalog.upsert(entry.clone()));
        assert_eq!(catalog.len(), 1);
        // Replace.
        let mut e2 = entry.clone();
        e2.display_name = "Foo2".into();
        assert!(catalog.upsert(e2));
        assert_eq!(catalog.lookup("foo").unwrap().display_name, "Foo2");
        // Remove.
        let removed = catalog.remove("FOO");
        assert!(removed.is_some());
        assert!(catalog.is_empty());
    }

    #[test]
    fn test_parse_lowercases_mixed_case_names() {
        let toml = r#"
[[provider]]
name = "DeepSeek"
display_name = "DeepSeek"
base_url = "https://api.deepseek.com"
api_key_env = "DEEPSEEK_API_KEY"
compatible_with = "openai"
default_model = "deepseek-chat"
"#;
        let mut catalog = ProviderCatalog::parse(toml).unwrap();
        // Lookups under any case should hit the entry.
        assert!(catalog.lookup("deepseek").is_some());
        assert!(catalog.lookup("DeepSeek").is_some());
        assert!(catalog.lookup("DEEPSEEK").is_some());
        // Mutations resolve the canonical key.
        assert!(catalog.set_models("DeepSeek", vec!["m1".into()]));
        assert_eq!(catalog.lookup("deepseek").unwrap().models, vec!["m1"]);
        assert!(catalog.set_base_url("DEEPSEEK", "https://x".into()));
        assert!(catalog.remove("DeepSeek").is_some());
        assert!(catalog.is_empty());
    }

    #[test]
    fn test_set_models() {
        let mut catalog = ProviderCatalog::parse(TEST_TOML).unwrap();
        assert!(catalog.set_models("deepseek", vec!["new-model".into()]));
        assert_eq!(
            catalog.lookup("deepseek").unwrap().models,
            vec!["new-model"]
        );
        assert!(!catalog.set_models("nonexistent", vec![]));
    }

    #[test]
    fn test_save_round_trip_with_overrides() {
        let mut catalog = ProviderCatalog::empty();
        let mut hdrs = HashMap::new();
        hdrs.insert("x-tenant".into(), "abc".into());
        catalog.upsert(CatalogEntry {
            name: "azure".into(),
            display_name: "Azure".into(),
            base_url: "https://x".into(),
            api_key_env: "AZ".into(),
            compatible_with: "openai".into(),
            default_model: "gpt-4o".into(),
            context_window: Some(128_000),
            supports_images: Some(true),
            auth_header: Some("api-key".into()),
            auth_prefix: Some(String::new()),
            chat_path: Some("/foo".into()),
            extra_headers: Some(hdrs),
            ..Default::default()
        });
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("providers.toml");
        catalog.save_to_file(&path).unwrap();
        let loaded = ProviderCatalog::from_file(&path).unwrap();
        let e = loaded.lookup("azure").unwrap();
        assert_eq!(e.context_window, Some(128_000));
        assert_eq!(e.supports_images, Some(true));
        assert_eq!(e.auth_header_name(), "api-key");
        assert_eq!(e.auth_header_prefix(), "");
        assert_eq!(e.chat_path.as_deref(), Some("/foo"));
        assert_eq!(
            e.extra_headers
                .as_ref()
                .and_then(|m| m.get("x-tenant"))
                .map(String::as_str),
            Some("abc")
        );
    }
}
