use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, Table};

pub fn config_path() -> PathBuf {
    std::env::var("AGENTOS_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("config/default.toml"))
}

/// Read a dotted key from the config file and print its value.
pub fn handle_get(key: &str) -> anyhow::Result<()> {
    let path = config_path();
    let content = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("Cannot read config at {}: {}", path.display(), e))?;
    let doc: DocumentMut = content
        .parse()
        .map_err(|e| anyhow::anyhow!("Config parse error: {}", e))?;

    let value = resolve_dotted_key(&doc, key)?;
    println!("{}", value);
    Ok(())
}

/// Set a dotted key in the config file, preserving comments and formatting.
pub fn handle_set(key: &str, value: &str) -> anyhow::Result<()> {
    let path = config_path();
    let content = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("Cannot read config at {}: {}", path.display(), e))?;
    let mut doc: DocumentMut = content
        .parse()
        .map_err(|e| anyhow::anyhow!("Config parse error: {}", e))?;

    set_dotted_key(&mut doc, key, value)?;

    std::fs::write(&path, doc.to_string())
        .map_err(|e| anyhow::anyhow!("Cannot write config: {}", e))?;

    println!("{} = {}", key, value);
    Ok(())
}

/// List all top-level sections in the config file.
pub fn handle_list(path_override: Option<&Path>) -> anyhow::Result<()> {
    let path = path_override
        .map(|p| p.to_path_buf())
        .unwrap_or_else(config_path);
    let content = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("Cannot read config at {}: {}", path.display(), e))?;
    let doc: DocumentMut = content
        .parse()
        .map_err(|e| anyhow::anyhow!("Config parse error: {}", e))?;

    println!("Config file: {}\n", path.display());
    for (key, item) in doc.as_table().iter() {
        if item.is_table() {
            println!("[{}]", key);
        } else {
            println!("{} = {}", key, format_value(item));
        }
    }
    Ok(())
}

/// Resolve an arbitrary-depth dotted key like "kernel.autonomous_mode.task_timeout_secs".
/// Returns the plain string value (quotes stripped for TOML strings).
pub fn resolve_dotted_key(doc: &DocumentMut, key: &str) -> anyhow::Result<String> {
    let parts: Vec<&str> = key.split('.').collect();
    let mut current: &Item = doc.as_item();

    for (i, part) in parts.iter().enumerate() {
        current = current.get(part).ok_or_else(|| {
            let prefix = parts[..=i].join(".");
            anyhow::anyhow!("Key '{}' not found", prefix)
        })?;
    }

    Ok(format_value(current))
}

/// Set an arbitrary-depth dotted key, creating intermediate tables as needed.
/// Preserves all existing keys, comments, and formatting via `toml_edit`.
pub fn set_dotted_key(doc: &mut DocumentMut, key: &str, value: &str) -> anyhow::Result<()> {
    let parts: Vec<&str> = key.split('.').collect();
    if parts.is_empty() {
        anyhow::bail!("Empty key");
    }

    // Parse value: try integer, float, bool, then fall back to quoted string.
    // Users who want to force a string type should quote the value themselves.
    let toml_value = if let Ok(i) = value.parse::<i64>() {
        toml_edit::value(i)
    } else if let Ok(f) = value.parse::<f64>() {
        toml_edit::value(f)
    } else if let Ok(b) = value.parse::<bool>() {
        toml_edit::value(b)
    } else {
        toml_edit::value(value)
    };

    if parts.len() == 1 {
        doc[parts[0]] = toml_value;
        return Ok(());
    }

    // Navigate to the parent table, creating intermediate tables as needed.
    let (path_parts, leaf) = parts.split_at(parts.len() - 1);
    let leaf = leaf[0];

    let mut table: &mut Table = doc.as_table_mut();
    for part in path_parts {
        if table.get(part).is_none() {
            table[part] = Item::Table(Table::new());
        }
        table = table[part]
            .as_table_mut()
            .ok_or_else(|| anyhow::anyhow!("'{}' is not a table", part))?;
    }
    table[leaf] = toml_value;
    Ok(())
}

/// Format an `Item` as a plain display value (strips surrounding quotes from strings).
fn format_value(item: &Item) -> String {
    if let Some(s) = item.as_str() {
        return s.to_string();
    }
    item.to_string().trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_temp(content: &str) -> NamedTempFile {
        let mut tmp = NamedTempFile::new().unwrap();
        write!(tmp, "{}", content).unwrap();
        tmp.flush().unwrap();
        tmp
    }

    #[test]
    fn test_config_get_plain_value() {
        let tmp = write_temp("[llm]\nprimary = \"anthropic/claude-opus-4-6\"\n");
        let content = std::fs::read_to_string(tmp.path()).unwrap();
        let doc: DocumentMut = content.parse().unwrap();
        let val = resolve_dotted_key(&doc, "llm.primary").unwrap();
        // Should return bare string, not TOML-quoted "\"anthropic/...\"".
        assert_eq!(val, "anthropic/claude-opus-4-6");
    }

    #[test]
    fn test_config_get_nested_3_levels() {
        let tmp = write_temp("[kernel.autonomous_mode]\ntask_timeout_secs = 600\n");
        let content = std::fs::read_to_string(tmp.path()).unwrap();
        let doc: DocumentMut = content.parse().unwrap();
        let val = resolve_dotted_key(&doc, "kernel.autonomous_mode.task_timeout_secs").unwrap();
        assert_eq!(val, "600");
    }

    #[test]
    fn test_config_set_preserves_comments() {
        let tmp = write_temp("[llm]\n# Primary provider\nprimary = \"old\"\nfallbacks = []\n");
        let path = tmp.path().to_path_buf();
        let content = std::fs::read_to_string(&path).unwrap();
        let mut doc: DocumentMut = content.parse().unwrap();

        set_dotted_key(&mut doc, "llm.primary", "new-value").unwrap();
        let result = doc.to_string();

        assert!(result.contains("# Primary provider"), "comment lost");
        assert!(result.contains("new-value"), "value not set");
        assert!(result.contains("fallbacks"), "other key lost");
    }

    #[test]
    fn test_config_set_creates_intermediate_tables() {
        let tmp = write_temp("[kernel]\ndefault_task_timeout_secs = 300\n");
        let path = tmp.path().to_path_buf();
        let content = std::fs::read_to_string(&path).unwrap();
        let mut doc: DocumentMut = content.parse().unwrap();

        set_dotted_key(&mut doc, "kernel.new_section.value", "42").unwrap();
        let result = doc.to_string();
        assert!(result.contains("42"), "nested value not set");
    }

    #[test]
    fn test_config_missing_key_returns_error() {
        let tmp = write_temp("[llm]\nprimary = \"x\"\n");
        let content = std::fs::read_to_string(tmp.path()).unwrap();
        let doc: DocumentMut = content.parse().unwrap();
        assert!(resolve_dotted_key(&doc, "llm.nonexistent").is_err());
        assert!(resolve_dotted_key(&doc, "missing_section.key").is_err());
    }
}
