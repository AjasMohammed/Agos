use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

/// A single provider entry from the `providers.toml` catalog.
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
    /// Parse a catalog from a TOML string.
    pub fn parse(toml_str: &str) -> Result<Self, toml::de::Error> {
        let file: CatalogFile = toml::from_str(toml_str)?;
        let mut providers = HashMap::new();
        for entry in file.provider {
            providers.insert(entry.name.clone(), entry);
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
}
