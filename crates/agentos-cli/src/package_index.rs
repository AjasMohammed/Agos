/// Local package index for tool and skill discovery.
///
/// Stored as JSON at `~/.agentos/index.json` (or a path specified by the user).
/// Tools and skills can be published to and searched from a local index without
/// requiring a running kernel or network access.
use agentos_types::TrustTier;
use serde::{Deserialize, Serialize};

/// Index file version — bump when the schema changes incompatibly.
pub const INDEX_VERSION: u32 = 1;

/// The root structure of a package index file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageIndex {
    pub version: u32,
    #[serde(default)]
    pub tools: Vec<PackageEntry>,
    #[serde(default)]
    pub skills: Vec<PackageEntry>,
}

impl PackageIndex {
    pub fn new() -> Self {
        Self {
            version: INDEX_VERSION,
            tools: Vec::new(),
            skills: Vec::new(),
        }
    }

    /// Load a package index from a JSON file. Creates an empty index if the file
    /// does not exist.
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Self::new());
        }
        let content = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&content)?)
    }

    /// Save the index to a JSON file atomically (write-then-rename).
    ///
    /// Writing to a temporary file and then renaming ensures the index is never
    /// left in a partially-written state if the process is killed mid-write.
    pub fn save(&self, path: &std::path::Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(self)?;
        let tmp_path = path.with_extension("json.tmp");
        std::fs::write(&tmp_path, &content)?;
        std::fs::rename(&tmp_path, path)?;
        Ok(())
    }

    /// Add or update a tool entry (upserts by name + version).
    pub fn upsert_tool(&mut self, entry: PackageEntry) {
        if let Some(existing) = self
            .tools
            .iter_mut()
            .find(|e| e.name == entry.name && e.version == entry.version)
        {
            *existing = entry;
        } else {
            self.tools.push(entry);
        }
    }

    /// Add or update a skill entry (upserts by name + version).
    pub fn upsert_skill(&mut self, entry: PackageEntry) {
        if let Some(existing) = self
            .skills
            .iter_mut()
            .find(|e| e.name == entry.name && e.version == entry.version)
        {
            *existing = entry;
        } else {
            self.skills.push(entry);
        }
    }

    /// Search tools by query string (case-insensitive match against name, description, tags).
    pub fn search_tools(&self, query: &str) -> Vec<&PackageEntry> {
        let q = query.to_lowercase();
        self.tools
            .iter()
            .filter(|e| {
                e.name.to_lowercase().contains(&q)
                    || e.description.to_lowercase().contains(&q)
                    || e.tags.iter().any(|t| t.to_lowercase().contains(&q))
                    || e.author.to_lowercase().contains(&q)
            })
            .collect()
    }

    /// Search skills by query string (case-insensitive match against name, description, tags).
    pub fn search_skills(&self, query: &str) -> Vec<&PackageEntry> {
        let q = query.to_lowercase();
        self.skills
            .iter()
            .filter(|e| {
                e.name.to_lowercase().contains(&q)
                    || e.description.to_lowercase().contains(&q)
                    || e.tags.iter().any(|t| t.to_lowercase().contains(&q))
                    || e.author.to_lowercase().contains(&q)
            })
            .collect()
    }
}

impl Default for PackageIndex {
    fn default() -> Self {
        Self::new()
    }
}

/// A single entry in the package index (tool or skill).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageEntry {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub trust_tier: TrustTier,
    /// Ed25519 signature over the canonical manifest payload (hex-encoded).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// URL to download the package from (for remote indices).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_url: Option<String>,
    /// Searchable tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Path to the manifest file on disk (for locally-published packages).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_path: Option<String>,
    /// ISO 8601 timestamp when this entry was published.
    pub published_at: String,
}

/// Return the default index path: `~/.agentos/index.json`.
pub fn default_index_path() -> std::path::PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    std::path::PathBuf::from(home)
        .join(".agentos")
        .join("index.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_index_is_empty() {
        let idx = PackageIndex::new();
        assert!(idx.tools.is_empty());
        assert!(idx.skills.is_empty());
        assert_eq!(idx.version, INDEX_VERSION);
    }

    #[test]
    fn upsert_tool_adds_entry() {
        let mut idx = PackageIndex::new();
        let entry = PackageEntry {
            name: "my-tool".into(),
            version: "1.0.0".into(),
            description: "A test tool".into(),
            author: "dev".into(),
            trust_tier: TrustTier::Community,
            signature: None,
            download_url: None,
            tags: vec!["test".into()],
            manifest_path: Some("/tmp/my-tool.toml".into()),
            published_at: "2026-04-09T00:00:00Z".into(),
        };
        idx.upsert_tool(entry);
        assert_eq!(idx.tools.len(), 1);
    }

    #[test]
    fn upsert_tool_deduplicates_by_name_version() {
        let mut idx = PackageIndex::new();
        let make = |desc: &str| PackageEntry {
            name: "my-tool".into(),
            version: "1.0.0".into(),
            description: desc.to_string(),
            author: "dev".into(),
            trust_tier: TrustTier::Community,
            signature: None,
            download_url: None,
            tags: vec![],
            manifest_path: None,
            published_at: "2026-04-09T00:00:00Z".into(),
        };
        idx.upsert_tool(make("first"));
        idx.upsert_tool(make("second"));
        assert_eq!(idx.tools.len(), 1);
        assert_eq!(idx.tools[0].description, "second");
    }

    #[test]
    fn search_tools_matches_name() {
        let mut idx = PackageIndex::new();
        idx.upsert_tool(PackageEntry {
            name: "file-reader".into(),
            version: "1.0.0".into(),
            description: "Reads files".into(),
            author: "core".into(),
            trust_tier: TrustTier::Core,
            signature: None,
            download_url: None,
            tags: vec!["file".into(), "io".into()],
            manifest_path: None,
            published_at: "2026-04-09T00:00:00Z".into(),
        });
        let results = idx.search_tools("file");
        assert_eq!(results.len(), 1);
        let empty = idx.search_tools("nonexistent");
        assert!(empty.is_empty());
    }

    #[test]
    fn roundtrip_json() {
        let mut idx = PackageIndex::new();
        idx.upsert_tool(PackageEntry {
            name: "tool-a".into(),
            version: "0.1.0".into(),
            description: "Test".into(),
            author: "dev".into(),
            trust_tier: TrustTier::Community,
            signature: None,
            download_url: None,
            tags: vec![],
            manifest_path: None,
            published_at: "2026-04-09T00:00:00Z".into(),
        });
        let json = serde_json::to_string(&idx).unwrap();
        let decoded: PackageIndex = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.tools.len(), 1);
        assert_eq!(decoded.tools[0].name, "tool-a");
    }
}
