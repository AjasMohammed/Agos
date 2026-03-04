use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::*;
use async_trait::async_trait;

pub struct DataParser;

impl DataParser {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DataParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for DataParser {
    fn name(&self) -> &str {
        "data-parser"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![] // No special permissions needed — operates only on input data
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let data = payload
            .get("data")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("data-parser requires 'data' field (string)".into())
            })?;

        let format = payload
            .get("format")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation(
                    "data-parser requires 'format' field (json|csv|toml)".into(),
                )
            })?;

        let parsed = match format.to_lowercase().as_str() {
            "json" => {
                let value: serde_json::Value =
                    serde_json::from_str(data).map_err(|e| AgentOSError::ToolExecutionFailed {
                        tool_name: "data-parser".into(),
                        reason: format!("Invalid JSON: {}", e),
                    })?;
                value
            }
            "csv" => {
                let mut reader = csv::ReaderBuilder::new()
                    .has_headers(true)
                    .from_reader(data.as_bytes());

                let headers: Vec<String> = reader
                    .headers()
                    .map_err(|e| AgentOSError::ToolExecutionFailed {
                        tool_name: "data-parser".into(),
                        reason: format!("Invalid CSV headers: {}", e),
                    })?
                    .iter()
                    .map(|h| h.to_string())
                    .collect();

                let mut rows = Vec::new();
                for record in reader.records() {
                    let record = record.map_err(|e| AgentOSError::ToolExecutionFailed {
                        tool_name: "data-parser".into(),
                        reason: format!("Invalid CSV row: {}", e),
                    })?;
                    let mut row = serde_json::Map::new();
                    for (i, field) in record.iter().enumerate() {
                        if let Some(header) = headers.get(i) {
                            row.insert(header.clone(), serde_json::Value::String(field.to_string()));
                        }
                    }
                    rows.push(serde_json::Value::Object(row));
                }

                serde_json::json!({
                    "headers": headers,
                    "rows": rows,
                    "row_count": rows.len(),
                })
            }
            "toml" => {
                let value: toml::Value =
                    toml::from_str(data).map_err(|e| AgentOSError::ToolExecutionFailed {
                        tool_name: "data-parser".into(),
                        reason: format!("Invalid TOML: {}", e),
                    })?;
                serde_json::to_value(value).unwrap_or(serde_json::json!(null))
            }
            other => {
                return Err(AgentOSError::SchemaValidation(format!(
                    "Unsupported format: '{}'. Supported: json, csv, toml",
                    other
                )));
            }
        };

        Ok(serde_json::json!({
            "format": format,
            "parsed": parsed,
        }))
    }
}
