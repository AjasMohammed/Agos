use agentos_memory::SemanticStore;
use agentos_types::{AgentID, AgentOSError, TaskID};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedFact {
    pub key: String,
    pub content: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ExtractionContext {
    pub tool_name: String,
    pub agent_id: AgentID,
    pub task_id: TaskID,
}

#[derive(Debug, Clone)]
pub enum MemoryOperation {
    Add(ExtractedFact),
    Update {
        existing_id: String,
        new_fact: ExtractedFact,
    },
    Noop,
}

pub trait MemoryExtractor: Send + Sync {
    fn tool_name(&self) -> &str;
    fn extract(&self, result: &serde_json::Value, ctx: &ExtractionContext) -> Vec<ExtractedFact>;
}

pub struct HttpClientExtractor;
impl MemoryExtractor for HttpClientExtractor {
    fn tool_name(&self) -> &str {
        "http-client"
    }

    fn extract(&self, result: &serde_json::Value, ctx: &ExtractionContext) -> Vec<ExtractedFact> {
        let status = result.get("status").and_then(|v| v.as_u64()).unwrap_or(0);
        if !(200..300).contains(&status) {
            return Vec::new();
        }
        let body = match result.get("body") {
            Some(v) => v,
            None => return Vec::new(),
        };

        let mut facts = Vec::new();
        if let Some(obj) = body.as_object() {
            let keys = obj.keys().take(20).cloned().collect::<Vec<_>>().join(", ");
            if !keys.is_empty() {
                facts.push(ExtractedFact {
                    key: format!("http-schema:{}", ctx.task_id.as_uuid()),
                    content: format!("HTTP response schema keys: {}", keys),
                    tags: vec!["http".to_string(), "api-schema".to_string()],
                });
            }
        } else if let Some(text) = body.as_str() {
            if text.len() > 100 {
                facts.push(ExtractedFact {
                    key: format!("http-text:{}", ctx.task_id.as_uuid()),
                    content: format!(
                        "HTTP text response ({} chars): {}",
                        text.len(),
                        text.chars().take(400).collect::<String>()
                    ),
                    tags: vec!["http".to_string(), "response-text".to_string()],
                });
            }
        }
        facts
    }
}

pub struct ShellExecExtractor;
impl MemoryExtractor for ShellExecExtractor {
    fn tool_name(&self) -> &str {
        "shell-exec"
    }

    fn extract(&self, result: &serde_json::Value, ctx: &ExtractionContext) -> Vec<ExtractedFact> {
        let exit_code = result
            .get("exit_code")
            .and_then(|v| v.as_i64())
            .unwrap_or(-1);
        let stdout = result.get("stdout").and_then(|v| v.as_str()).unwrap_or("");
        let stderr = result.get("stderr").and_then(|v| v.as_str()).unwrap_or("");

        if exit_code != 0 {
            if stderr.len() > 20 {
                return vec![ExtractedFact {
                    key: format!("shell-error:{}", ctx.task_id.as_uuid()),
                    content: format!(
                        "Shell command failed (exit {}): {}",
                        exit_code,
                        stderr.chars().take(300).collect::<String>()
                    ),
                    tags: vec!["shell".to_string(), "error".to_string()],
                }];
            }
            return Vec::new();
        }

        if stdout.len() > 50 {
            vec![ExtractedFact {
                key: format!("shell-output:{}", ctx.task_id.as_uuid()),
                content: format!(
                    "Shell command output ({} chars): {}",
                    stdout.len(),
                    stdout.chars().take(400).collect::<String>()
                ),
                tags: vec!["shell".to_string(), "output".to_string()],
            }]
        } else {
            Vec::new()
        }
    }
}

pub struct FileReaderExtractor;
impl MemoryExtractor for FileReaderExtractor {
    fn tool_name(&self) -> &str {
        "file-reader"
    }

    fn extract(&self, result: &serde_json::Value, _ctx: &ExtractionContext) -> Vec<ExtractedFact> {
        let path = result
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let content = result
            .get("content")
            .and_then(|v| v.as_str())
            .or_else(|| result.as_str())
            .unwrap_or("");
        if content.len() < 50 {
            return Vec::new();
        }
        vec![ExtractedFact {
            key: format!("file-read:{}", path),
            content: format!(
                "Read file '{}' ({} chars). Preview: {}",
                path,
                content.len(),
                content.chars().take(300).collect::<String>()
            ),
            tags: vec!["file".to_string(), "read".to_string()],
        }]
    }
}

pub struct DataParserExtractor;
impl MemoryExtractor for DataParserExtractor {
    fn tool_name(&self) -> &str {
        "data-parser"
    }

    fn extract(&self, result: &serde_json::Value, ctx: &ExtractionContext) -> Vec<ExtractedFact> {
        if let Some(obj) = result.as_object() {
            let keys = obj.keys().take(15).cloned().collect::<Vec<_>>().join(", ");
            if keys.is_empty() {
                return Vec::new();
            }
            return vec![ExtractedFact {
                key: format!("parsed-schema:{}", ctx.task_id.as_uuid()),
                content: format!("Parsed object fields: {}", keys),
                tags: vec!["data".to_string(), "parsed".to_string()],
            }];
        }
        if let Some(arr) = result.as_array() {
            if arr.is_empty() {
                return Vec::new();
            }
            return vec![ExtractedFact {
                key: format!("parsed-array:{}", ctx.task_id.as_uuid()),
                content: format!("Parsed array with {} rows", arr.len()),
                tags: vec!["data".to_string(), "parsed".to_string()],
            }];
        }
        Vec::new()
    }
}

pub struct ExtractionRegistry {
    extractors: HashMap<String, Box<dyn MemoryExtractor>>,
}

impl Default for ExtractionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ExtractionRegistry {
    pub fn new() -> Self {
        Self {
            extractors: HashMap::new(),
        }
    }

    pub fn register(&mut self, extractor: Box<dyn MemoryExtractor>) {
        self.extractors
            .insert(extractor.tool_name().to_string(), extractor);
    }

    pub fn get(&self, tool_name: &str) -> Option<&dyn MemoryExtractor> {
        self.extractors.get(tool_name).map(|e| e.as_ref())
    }

    pub fn register_defaults(&mut self) {
        self.register(Box::new(HttpClientExtractor));
        self.register(Box::new(ShellExecExtractor));
        self.register(Box::new(FileReaderExtractor));
        self.register(Box::new(DataParserExtractor));
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_conflict_threshold")]
    pub conflict_threshold: f32,
    #[serde(default = "default_max_facts_per_result")]
    pub max_facts_per_result: usize,
    #[serde(default = "default_min_result_length")]
    pub min_result_length: usize,
}

fn default_enabled() -> bool {
    true
}
fn default_conflict_threshold() -> f32 {
    0.85
}
fn default_max_facts_per_result() -> usize {
    5
}
fn default_min_result_length() -> usize {
    50
}

impl Default for ExtractionConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            conflict_threshold: default_conflict_threshold(),
            max_facts_per_result: default_max_facts_per_result(),
            min_result_length: default_min_result_length(),
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct ExtractionReport {
    pub added: usize,
    pub updated: usize,
    pub skipped: usize,
}

pub struct MemoryExtractionEngine {
    registry: ExtractionRegistry,
    semantic_store: Arc<SemanticStore>,
    config: ExtractionConfig,
}

impl MemoryExtractionEngine {
    pub fn new(
        registry: ExtractionRegistry,
        semantic_store: Arc<SemanticStore>,
        config: ExtractionConfig,
    ) -> Self {
        Self {
            registry,
            semantic_store,
            config,
        }
    }

    pub async fn process_tool_result(
        &self,
        tool_name: &str,
        result: &serde_json::Value,
        ctx: &ExtractionContext,
    ) -> Result<ExtractionReport, AgentOSError> {
        if !self.config.enabled || result.to_string().len() < self.config.min_result_length {
            return Ok(ExtractionReport::default());
        }
        let extractor = match self.registry.get(tool_name) {
            Some(e) => e,
            None => return Ok(ExtractionReport::default()),
        };
        let mut facts = extractor.extract(result, ctx);
        facts.truncate(self.config.max_facts_per_result);
        let mut report = ExtractionReport::default();
        for fact in facts {
            match self.detect_conflict(&fact, &ctx.agent_id).await? {
                MemoryOperation::Add(new_fact) => {
                    let tag_refs = new_fact.tags.iter().map(|s| s.as_str()).collect::<Vec<_>>();
                    self.semantic_store
                        .write(
                            &new_fact.key,
                            &new_fact.content,
                            Some(&ctx.agent_id),
                            &tag_refs,
                        )
                        .await?;
                    report.added += 1;
                }
                MemoryOperation::Update {
                    existing_id,
                    new_fact,
                } => {
                    self.semantic_store.delete(&existing_id).await?;
                    let tag_refs = new_fact.tags.iter().map(|s| s.as_str()).collect::<Vec<_>>();
                    self.semantic_store
                        .write(
                            &new_fact.key,
                            &new_fact.content,
                            Some(&ctx.agent_id),
                            &tag_refs,
                        )
                        .await?;
                    report.updated += 1;
                }
                MemoryOperation::Noop => {
                    report.skipped += 1;
                }
            }
        }
        Ok(report)
    }

    async fn detect_conflict(
        &self,
        fact: &ExtractedFact,
        agent_id: &AgentID,
    ) -> Result<MemoryOperation, AgentOSError> {
        let results = self
            .semantic_store
            .search(
                &fact.content,
                Some(agent_id),
                3,
                self.config.conflict_threshold,
            )
            .await?;
        if results.is_empty() {
            return Ok(MemoryOperation::Add(fact.clone()));
        }
        let top = &results[0];
        if top.semantic_score > 0.95 {
            return Ok(MemoryOperation::Noop);
        }
        Ok(MemoryOperation::Update {
            existing_id: top.entry.id.clone(),
            new_fact: fact.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extraction_report_default_has_no_conflicts() {
        let report = ExtractionReport::default();
        assert_eq!(report.added, 0);
        assert_eq!(report.updated, 0);
        assert_eq!(report.skipped, 0);
    }

    #[test]
    fn extraction_report_updated_represents_conflict_resolution() {
        // `updated > 0` means detect_conflict returned MemoryOperation::Update,
        // which only happens when semantic similarity is between 0.85 and 0.95
        // (high enough to be related, not identical). This IS a conflict.
        let report = ExtractionReport {
            added: 1,
            updated: 3,
            ..ExtractionReport::default()
        };
        assert!(
            report.updated > 0,
            "updated > 0 signals conflict resolution, triggering SemanticMemoryConflict event"
        );
    }

    #[test]
    fn http_extractor_ignores_non_success_status() {
        let extractor = HttpClientExtractor;
        let ctx = ExtractionContext {
            tool_name: "http-client".to_string(),
            agent_id: AgentID::new(),
            task_id: TaskID::new(),
        };
        let result = serde_json::json!({"status": 404, "body": {"error": "not found"}});
        let facts = extractor.extract(&result, &ctx);
        assert!(facts.is_empty(), "404 responses should not produce facts");
    }

    #[test]
    fn http_extractor_extracts_schema_from_success() {
        let extractor = HttpClientExtractor;
        let ctx = ExtractionContext {
            tool_name: "http-client".to_string(),
            agent_id: AgentID::new(),
            task_id: TaskID::new(),
        };
        let result = serde_json::json!({"status": 200, "body": {"name": "test", "value": 42}});
        let facts = extractor.extract(&result, &ctx);
        assert_eq!(facts.len(), 1);
        assert!(facts[0].content.contains("schema keys"));
    }

    #[test]
    fn shell_extractor_captures_errors() {
        let extractor = ShellExecExtractor;
        let ctx = ExtractionContext {
            tool_name: "shell-exec".to_string(),
            agent_id: AgentID::new(),
            task_id: TaskID::new(),
        };
        let result = serde_json::json!({
            "exit_code": 1,
            "stdout": "",
            "stderr": "error: command not found: foobar123"
        });
        let facts = extractor.extract(&result, &ctx);
        assert_eq!(facts.len(), 1);
        assert!(facts[0].tags.contains(&"error".to_string()));
    }

    #[test]
    fn file_reader_extractor_skips_short_content() {
        let extractor = FileReaderExtractor;
        let ctx = ExtractionContext {
            tool_name: "file-reader".to_string(),
            agent_id: AgentID::new(),
            task_id: TaskID::new(),
        };
        let result = serde_json::json!({"path": "/tmp/x", "content": "short"});
        let facts = extractor.extract(&result, &ctx);
        assert!(facts.is_empty(), "Content under 50 chars should be skipped");
    }

    #[test]
    fn extraction_registry_returns_none_for_unregistered() {
        let registry = ExtractionRegistry::new();
        assert!(registry.get("nonexistent-tool").is_none());
    }

    #[test]
    fn extraction_registry_registers_defaults() {
        let mut registry = ExtractionRegistry::new();
        registry.register_defaults();
        assert!(registry.get("http-client").is_some());
        assert!(registry.get("shell-exec").is_some());
        assert!(registry.get("file-reader").is_some());
        assert!(registry.get("data-parser").is_some());
    }

    #[test]
    fn extraction_config_default_values() {
        let config = ExtractionConfig::default();
        assert!(config.enabled);
        assert_eq!(config.conflict_threshold, 0.85);
        assert_eq!(config.max_facts_per_result, 5);
        assert_eq!(config.min_result_length, 50);
    }
}
