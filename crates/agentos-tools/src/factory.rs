use crate::traits::AgentTool;
use agentos_memory::{Embedder, EpisodicStore, ProceduralStore, SemanticStore};
use agentos_types::AgentOSError;
use std::path::Path;
use std::sync::Arc;
use tracing::warn;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCategory {
    Stateless,
    Memory,
    Network,
    Hal,
}

const STATELESS_TOOL_NAMES: &[&str] = &[
    "datetime",
    "think",
    "file-reader",
    "file-writer",
    "file-editor",
    "file-glob",
    "file-grep",
    "file-delete",
    "file-move",
    "file-diff",
    "data-parser",
    "memory-block-write",
    "memory-block-read",
    "memory-block-list",
    "memory-block-delete",
];

const MEMORY_TOOL_NAMES: &[&str] = &[
    "memory-search",
    "memory-write",
    "memory-read",
    "memory-delete",
    "memory-stats",
    "archival-insert",
    "archival-search",
    "episodic-list",
    "procedure-create",
    "procedure-delete",
    "procedure-list",
    "procedure-search",
];

const NETWORK_TOOL_NAMES: &[&str] = &["http-client", "web-fetch"];

const HAL_TOOL_NAMES: &[&str] = &[
    "hardware-info",
    "sys-monitor",
    "process-manager",
    "log-reader",
    "network-monitor",
];

const KERNEL_CONTEXT_TOOL_NAMES: &[&str] = &[
    "agent-message",
    "task-delegate",
    "agent-list",
    "task-status",
    "task-list",
    "shell-exec",
];

const SPECIAL_CONTEXT_TOOL_NAMES: &[&str] = &["agent-manual", "agent-self"];

pub fn tool_category(name: &str) -> Option<ToolCategory> {
    if STATELESS_TOOL_NAMES.contains(&name) {
        Some(ToolCategory::Stateless)
    } else if MEMORY_TOOL_NAMES.contains(&name) {
        Some(ToolCategory::Memory)
    } else if NETWORK_TOOL_NAMES.contains(&name) {
        Some(ToolCategory::Network)
    } else if HAL_TOOL_NAMES.contains(&name) {
        Some(ToolCategory::Hal)
    } else {
        // Kernel-context tools, special-context tools, and unknown tools
        // all return None — they must execute in-process, not sandboxed.
        if !KERNEL_CONTEXT_TOOL_NAMES.contains(&name) && !SPECIAL_CONTEXT_TOOL_NAMES.contains(&name)
        {
            tracing::debug!(
                tool = name,
                "Unknown tool name, returning None from tool_category"
            );
        }
        None
    }
}

fn parse_tool_weight(weight: &str) -> Option<ToolCategory> {
    match weight.trim().to_ascii_lowercase().as_str() {
        "stateless" => Some(ToolCategory::Stateless),
        "memory" => Some(ToolCategory::Memory),
        "network" => Some(ToolCategory::Network),
        "hal" => Some(ToolCategory::Hal),
        _ => None,
    }
}

/// Determine the dependency category for a tool.
///
/// If `manifest_weight` is provided, it takes priority over the hardcoded
/// name-based lookup. Unknown values are ignored with a warning so manifests
/// remain forward-compatible.
pub fn tool_category_with_weight(
    name: &str,
    manifest_weight: Option<&str>,
) -> Option<ToolCategory> {
    if let Some(weight) = manifest_weight
        .map(str::trim)
        .filter(|weight| !weight.is_empty())
    {
        if let Some(category) = parse_tool_weight(weight) {
            return Some(category);
        }

        warn!(
            tool = name,
            weight, "Unknown tool weight hint, falling back to name-based detection"
        );
    }

    tool_category(name)
}

pub fn build_single_tool(
    name: &str,
    data_dir: &Path,
) -> Result<Option<Box<dyn AgentTool>>, AgentOSError> {
    build_single_tool_with_model_cache(name, data_dir, &data_dir.join("models"))
}

pub fn build_single_tool_with_model_cache(
    name: &str,
    data_dir: &Path,
    model_cache_dir: &Path,
) -> Result<Option<Box<dyn AgentTool>>, AgentOSError> {
    build_single_tool_with_model_cache_and_weight(name, None, data_dir, model_cache_dir)
}

pub fn build_single_tool_with_model_cache_and_weight(
    name: &str,
    manifest_weight: Option<&str>,
    data_dir: &Path,
    model_cache_dir: &Path,
) -> Result<Option<Box<dyn AgentTool>>, AgentOSError> {
    let category = match tool_category_with_weight(name, manifest_weight) {
        Some(category) => category,
        None => return Ok(None),
    };

    match category {
        ToolCategory::Stateless => build_stateless_tool(name),
        ToolCategory::Memory => build_memory_tool(name, data_dir, model_cache_dir),
        ToolCategory::Network => build_network_tool(name),
        ToolCategory::Hal => build_hal_tool(name),
    }
}

fn build_stateless_tool(name: &str) -> Result<Option<Box<dyn AgentTool>>, AgentOSError> {
    let tool: Box<dyn AgentTool> = match name {
        "datetime" => Box::new(crate::datetime::DatetimeTool::new()),
        "think" => Box::new(crate::think::ThinkTool::new()),
        "file-reader" => Box::new(crate::file_reader::FileReader::new()),
        "file-writer" => Box::new(crate::file_writer::FileWriter::new()),
        "file-editor" => Box::new(crate::file_editor::FileEditor::new()),
        "file-glob" => Box::new(crate::file_glob::FileGlob::new()),
        "file-grep" => Box::new(crate::file_grep::FileGrep::new()),
        "file-delete" => Box::new(crate::file_delete::FileDelete::new()),
        "file-move" => Box::new(crate::file_move::FileMove::new()),
        "file-diff" => Box::new(crate::file_diff::FileDiff::new()),
        "data-parser" => Box::new(crate::data_parser::DataParser::new()),
        "memory-block-write" => Box::new(crate::memory_block_write::MemoryBlockWriteTool::new()),
        "memory-block-read" => Box::new(crate::memory_block_read::MemoryBlockReadTool::new()),
        "memory-block-list" => Box::new(crate::memory_block_list::MemoryBlockListTool::new()),
        "memory-block-delete" => Box::new(crate::memory_block_delete::MemoryBlockDeleteTool::new()),
        _ => return Ok(None),
    };
    Ok(Some(tool))
}

fn build_memory_tool(
    name: &str,
    data_dir: &Path,
    model_cache_dir: &Path,
) -> Result<Option<Box<dyn AgentTool>>, AgentOSError> {
    match name {
        "episodic-list" => {
            let episodic = Arc::new(EpisodicStore::open(data_dir)?);
            return Ok(Some(Box::new(crate::episodic_list::EpisodicList::new(
                episodic,
            ))));
        }
        "procedure-create" | "procedure-delete" | "procedure-list" | "procedure-search" => {
            let embedder = init_embedder(model_cache_dir)?;
            let procedural = Arc::new(ProceduralStore::open_with_embedder(data_dir, embedder)?);
            let tool: Box<dyn AgentTool> = match name {
                "procedure-create" => {
                    Box::new(crate::procedure_create::ProcedureCreate::new(procedural))
                }
                "procedure-delete" => {
                    Box::new(crate::procedure_delete::ProcedureDelete::new(procedural))
                }
                "procedure-list" => Box::new(crate::procedure_list::ProcedureList::new(procedural)),
                "procedure-search" => {
                    Box::new(crate::procedure_search::ProcedureSearch::new(procedural))
                }
                _ => unreachable!(),
            };
            return Ok(Some(tool));
        }
        "memory-read" | "archival-insert" | "archival-search" => {
            let embedder = init_embedder(model_cache_dir)?;
            let semantic = Arc::new(SemanticStore::open_with_embedder(data_dir, embedder)?);
            let tool: Box<dyn AgentTool> = match name {
                "memory-read" => Box::new(crate::memory_read::MemoryRead::new(semantic)),
                "archival-insert" => {
                    Box::new(crate::archival_insert::ArchivalInsert::new(semantic))
                }
                "archival-search" => {
                    Box::new(crate::archival_search::ArchivalSearch::new(semantic))
                }
                _ => unreachable!(),
            };
            return Ok(Some(tool));
        }
        _ => {}
    }

    let embedder = init_embedder(model_cache_dir)?;
    let semantic = Arc::new(SemanticStore::open_with_embedder(
        data_dir,
        embedder.clone(),
    )?);
    let episodic = Arc::new(EpisodicStore::open(data_dir)?);

    let tool: Box<dyn AgentTool> = match name {
        "memory-search" => Box::new(crate::memory_search::MemorySearch::new(semantic, episodic)),
        "memory-write" => Box::new(crate::memory_write::MemoryWrite::new(semantic, episodic)),
        "memory-delete" => Box::new(crate::memory_delete::MemoryDelete::new(semantic, episodic)),
        "memory-stats" => {
            let procedural = Arc::new(ProceduralStore::open_with_embedder(data_dir, embedder)?);
            Box::new(crate::memory_stats::MemoryStats::new(
                semantic, episodic, procedural,
            ))
        }
        _ => return Ok(None),
    };
    Ok(Some(tool))
}

fn init_embedder(model_cache_dir: &Path) -> Result<Arc<Embedder>, AgentOSError> {
    match Embedder::with_cache_dir(model_cache_dir) {
        Ok(embedder) => Ok(Arc::new(embedder)),
        Err(cache_err) => {
            warn!(
                error = %cache_err,
                cache_dir = %model_cache_dir.display(),
                "Failed to initialize sandbox embedder with configured cache dir; falling back to default cache"
            );
            Ok(Arc::new(Embedder::new().map_err(|e| {
                AgentOSError::StorageError(format!("Failed to initialize embedding model: {}", e))
            })?))
        }
    }
}

fn build_network_tool(name: &str) -> Result<Option<Box<dyn AgentTool>>, AgentOSError> {
    let tool: Box<dyn AgentTool> = match name {
        "http-client" => Box::new(crate::http_client::HttpClientTool::new().map_err(|e| {
            AgentOSError::ToolExecutionFailed {
                tool_name: "http-client".to_string(),
                reason: format!("Failed to initialize http-client: {}", e),
            }
        })?),
        "web-fetch" => Box::new(crate::web_fetch::WebFetch::new().map_err(|e| {
            AgentOSError::ToolExecutionFailed {
                tool_name: "web-fetch".to_string(),
                reason: format!("Failed to initialize web-fetch: {}", e),
            }
        })?),
        _ => return Ok(None),
    };
    Ok(Some(tool))
}

fn build_hal_tool(name: &str) -> Result<Option<Box<dyn AgentTool>>, AgentOSError> {
    let tool: Box<dyn AgentTool> = match name {
        "hardware-info" => Box::new(crate::hardware_info::HardwareInfoTool::new()),
        "sys-monitor" => Box::new(crate::sys_monitor::SysMonitorTool::new()),
        "process-manager" => Box::new(crate::process_manager::ProcessManagerTool::new()),
        "log-reader" => Box::new(crate::log_reader::LogReaderTool::new()),
        "network-monitor" => Box::new(crate::network_monitor::NetworkMonitorTool::new()),
        _ => return Ok(None),
    };
    Ok(Some(tool))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeSet, HashSet};
    use tempfile::TempDir;

    #[test]
    fn test_stateless_tools_build_without_heavy_dependencies() {
        let tmp = TempDir::new().unwrap();
        for name in [
            "datetime",
            "think",
            "file-reader",
            "file-writer",
            "file-editor",
            "file-glob",
            "file-grep",
            "file-delete",
            "file-move",
            "file-diff",
            "data-parser",
            "memory-block-write",
            "memory-block-read",
            "memory-block-list",
            "memory-block-delete",
        ] {
            let result = build_single_tool(name, tmp.path());
            assert!(
                result.is_ok(),
                "Failed to build {}: {:?}",
                name,
                result.err()
            );
            let tool = result.unwrap();
            assert!(tool.is_some(), "Tool {} returned None", name);
            assert_eq!(tool.unwrap().name(), name);
        }
    }

    #[test]
    fn test_category_classification() {
        assert_eq!(tool_category("datetime"), Some(ToolCategory::Stateless));
        assert_eq!(
            tool_category("memory-block-write"),
            Some(ToolCategory::Stateless)
        );
        assert_eq!(tool_category("memory-search"), Some(ToolCategory::Memory));
        assert_eq!(tool_category("web-fetch"), Some(ToolCategory::Network));
        assert_eq!(tool_category("hardware-info"), Some(ToolCategory::Hal));
        assert_eq!(tool_category("network-monitor"), Some(ToolCategory::Hal));
        assert_eq!(tool_category("agent-message"), None);
        assert_eq!(tool_category("agent-manual"), None);
        assert_eq!(tool_category("nonexistent"), None);
    }

    #[test]
    fn test_manifest_weight_override() {
        assert_eq!(
            tool_category_with_weight("http-client", Some("stateless")),
            Some(ToolCategory::Stateless)
        );
        assert_eq!(
            tool_category_with_weight("datetime", Some("NETWORK")),
            Some(ToolCategory::Network)
        );
    }

    #[test]
    fn test_unknown_manifest_weight_falls_back_to_name_based_detection() {
        assert_eq!(
            tool_category_with_weight("web-fetch", Some("custom-future-weight")),
            Some(ToolCategory::Network)
        );
        assert_eq!(
            tool_category_with_weight("nonexistent", Some("custom-future-weight")),
            None
        );
    }

    #[test]
    fn test_memory_block_tools_are_stateless() {
        for name in [
            "memory-block-write",
            "memory-block-read",
            "memory-block-list",
            "memory-block-delete",
        ] {
            assert_eq!(
                tool_category(name),
                Some(ToolCategory::Stateless),
                "memory block tool {name} should be stateless"
            );
        }
    }

    #[test]
    fn test_kernel_context_tools_return_none() {
        let tmp = TempDir::new().unwrap();
        for name in [
            "agent-message",
            "task-delegate",
            "agent-list",
            "task-status",
            "task-list",
            "shell-exec",
            "agent-manual",
            "agent-self",
        ] {
            let result = build_single_tool(name, tmp.path()).unwrap();
            assert!(
                result.is_none(),
                "Kernel-context tool {} should return None",
                name
            );
        }
    }

    #[test]
    fn test_unknown_tool_returns_none() {
        let tmp = TempDir::new().unwrap();
        let result = build_single_tool("nonexistent", tmp.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_factory_classifies_all_non_kernel_tools_registered_by_runner() {
        let tmp = TempDir::new().unwrap();
        let mut runner = crate::ToolRunner::new(tmp.path()).unwrap();
        runner.register_agent_manual(Vec::new());
        let tool_names = runner.list_tools();
        runner.register_agent_self(tool_names);
        let registered_tools: BTreeSet<String> = runner.list_tools().into_iter().collect();
        let explicitly_non_sandboxable: BTreeSet<String> = KERNEL_CONTEXT_TOOL_NAMES
            .iter()
            .chain(SPECIAL_CONTEXT_TOOL_NAMES.iter())
            .copied()
            .map(str::to_string)
            .collect();

        let missing_factory_coverage: Vec<String> = registered_tools
            .difference(&explicitly_non_sandboxable)
            .filter(|name| tool_category(name).is_none())
            .cloned()
            .collect();

        assert!(
            missing_factory_coverage.is_empty(),
            "Factory is missing sandbox classification for registered tools: {:?}",
            missing_factory_coverage
        );

        let unknown_non_sandboxable: Vec<String> = registered_tools
            .intersection(&explicitly_non_sandboxable)
            .filter(|name| tool_category(name).is_some())
            .cloned()
            .collect();

        assert!(
            unknown_non_sandboxable.is_empty(),
            "Non-sandboxable tools unexpectedly classified as sandboxable: {:?}",
            unknown_non_sandboxable
        );
    }

    #[test]
    fn test_factory_classification_lists_are_unique() {
        let all_known: HashSet<&str> = STATELESS_TOOL_NAMES
            .iter()
            .chain(MEMORY_TOOL_NAMES.iter())
            .chain(NETWORK_TOOL_NAMES.iter())
            .chain(HAL_TOOL_NAMES.iter())
            .chain(KERNEL_CONTEXT_TOOL_NAMES.iter())
            .chain(SPECIAL_CONTEXT_TOOL_NAMES.iter())
            .copied()
            .collect();

        let total = STATELESS_TOOL_NAMES.len()
            + MEMORY_TOOL_NAMES.len()
            + NETWORK_TOOL_NAMES.len()
            + HAL_TOOL_NAMES.len()
            + KERNEL_CONTEXT_TOOL_NAMES.len()
            + SPECIAL_CONTEXT_TOOL_NAMES.len();

        assert_eq!(
            all_known.len(),
            total,
            "Duplicate tool name found in factory classification lists"
        );
    }
}
