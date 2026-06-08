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

    // Snapshot the PRE-WRITE file content so this change can be rolled back.
    // Revisioning is a safety sidecar, not a gate: a snapshot failure (e.g. an
    // unwritable DB) is logged but must NOT block the config write.
    let old = resolve_dotted_key(&doc, key).ok();
    if let Err(e) = super::config_revision_store::snapshot(&content, key, old.as_deref(), value) {
        eprintln!("warning: could not record config revision: {e}");
    }

    set_dotted_key(&mut doc, key, value)?;

    std::fs::write(&path, doc.to_string())
        .map_err(|e| anyhow::anyhow!("Cannot write config: {}", e))?;

    println!("{} = {}", key, value);
    Ok(())
}

/// Print config revision history, newest first.
pub fn handle_history(limit: usize) -> anyhow::Result<()> {
    let rows = super::config_revision_store::list(limit)?;
    if rows.is_empty() {
        println!("No config revisions recorded yet.");
        return Ok(());
    }
    println!("{:>5}  {:<25}  {:<30}  change", "rev", "created_at", "key");
    for r in rows {
        let key = r.key.unwrap_or_default();
        let change = match (r.old_value, r.new_value) {
            (Some(o), Some(n)) => format!("{o} → {n}"),
            (None, Some(n)) => format!("(unset) → {n}"),
            _ => String::new(),
        };
        println!(
            "{:>5}  {:<25}  {:<30}  {}",
            r.rev, r.created_at, key, change
        );
    }
    Ok(())
}

/// Roll the config file back to a stored revision's content. The running
/// kernel's `ConfigWatcher` hot-reloads the write automatically.
pub fn handle_rollback(rev: i64) -> anyhow::Result<()> {
    let content = super::config_revision_store::get(rev)?
        .ok_or_else(|| anyhow::anyhow!("Revision {rev} not found"))?;

    // Refuse to write a snapshot that doesn't parse — never restore a broken config.
    content
        .parse::<DocumentMut>()
        .map_err(|e| anyhow::anyhow!("Revision {rev} is not valid TOML, refusing rollback: {e}"))?;

    let path = config_path();

    // Snapshot the CURRENT file first so the rollback is itself reversible.
    if let Ok(current) = std::fs::read_to_string(&path) {
        if let Err(e) = super::config_revision_store::snapshot(
            &current,
            &format!("rollback->{rev}"),
            None,
            "rollback",
        ) {
            eprintln!("warning: could not record pre-rollback revision: {e}");
        }
    }

    std::fs::write(&path, &content).map_err(|e| anyhow::anyhow!("Cannot write config: {}", e))?;
    println!("Rolled back to revision {rev}");
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

    // ---- Revisioning / rollback integration (mutate global env → run serially) ----

    use serial_test::serial;

    /// Point AGENTOS_CONFIG + AGENTOS_CONFIG_REVISIONS at a temp dir; restore on drop.
    struct EnvGuard {
        dir: tempfile::TempDir,
    }
    impl EnvGuard {
        fn new(initial_config: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let cfg = dir.path().join("config.toml");
            std::fs::write(&cfg, initial_config).unwrap();
            std::env::set_var("AGENTOS_CONFIG", &cfg);
            std::env::set_var("AGENTOS_CONFIG_REVISIONS", dir.path().join("rev.db"));
            Self { dir }
        }
        fn config_file(&self) -> std::path::PathBuf {
            self.dir.path().join("config.toml")
        }
        fn read_config(&self) -> String {
            std::fs::read_to_string(self.config_file()).unwrap()
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            std::env::remove_var("AGENTOS_CONFIG");
            std::env::remove_var("AGENTOS_CONFIG_REVISIONS");
        }
    }

    #[test]
    #[serial]
    fn set_records_pre_write_content() {
        let g = EnvGuard::new("[kernel]\ndefault_task_timeout_secs = 300\n");
        let before = g.read_config();
        handle_set("kernel.default_task_timeout_secs", "1").unwrap();
        // The file is now changed...
        assert!(g.read_config().contains("= 1"));
        // ...but the recorded revision holds the PRE-write content.
        let rows = super::super::config_revision_store::list(10).unwrap();
        assert_eq!(rows.len(), 1);
        let stored = super::super::config_revision_store::get(rows[0].rev)
            .unwrap()
            .unwrap();
        assert_eq!(stored, before);
        assert_eq!(rows[0].new_value.as_deref(), Some("1"));
        assert_eq!(rows[0].old_value.as_deref(), Some("300"));
    }

    #[test]
    #[serial]
    fn rollback_restores_prior_content() {
        let g = EnvGuard::new("[llm]\nprimary = \"old\"\n");
        let original = g.read_config();
        handle_set("llm.primary", "new").unwrap();
        assert!(g.read_config().contains("new"));
        // Revision 1 holds the pre-set ("old") content.
        handle_rollback(1).unwrap();
        assert_eq!(g.read_config(), original);
        let doc: DocumentMut = g.read_config().parse().unwrap();
        assert_eq!(resolve_dotted_key(&doc, "llm.primary").unwrap(), "old");
    }

    #[test]
    #[serial]
    fn rollback_refuses_unparseable_revision() {
        let g = EnvGuard::new("[llm]\nprimary = \"keep\"\n");
        // Inject a revision whose stored content is invalid TOML.
        let rev = super::super::config_revision_store::snapshot("= = not valid =", "x", None, "y")
            .unwrap();
        let before = g.read_config();
        let err = handle_rollback(rev).unwrap_err();
        assert!(err.to_string().contains("not valid TOML"));
        // The live config must be untouched.
        assert_eq!(g.read_config(), before);
    }

    #[test]
    #[serial]
    fn snapshot_failure_does_not_block_set() {
        let g = EnvGuard::new("[kernel]\nx = 1\n");
        // Point the revisions DB at a path whose parent doesn't exist → open fails.
        std::env::set_var(
            "AGENTOS_CONFIG_REVISIONS",
            g.dir.path().join("no_such_dir").join("rev.db"),
        );
        // The set must still succeed despite the snapshot failure.
        handle_set("kernel.x", "2").unwrap();
        assert!(g.read_config().contains("= 2"));
    }
}
