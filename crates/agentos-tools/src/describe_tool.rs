use crate::agent_manual::{AgentManualTool, SharedToolSummaries};
use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::json;

pub struct DescribeToolTool {
    tool_summaries: SharedToolSummaries,
}

impl DescribeToolTool {
    pub fn new(tool_summaries: SharedToolSummaries) -> Self {
        Self { tool_summaries }
    }

    /// Generate an example call object from an input schema's required fields.
    fn make_example(input_schema: &serde_json::Value) -> Option<serde_json::Value> {
        let properties = input_schema.get("properties")?.as_object()?;
        let required: Vec<&str> = input_schema
            .get("required")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        if required.is_empty() {
            return None;
        }

        let mut example = serde_json::Map::new();
        for field_name in &required {
            if let Some(field_schema) = properties.get(*field_name) {
                // Prefer first enum value when the field has an explicit enum constraint.
                if let Some(first) = field_schema
                    .get("enum")
                    .and_then(|v| v.as_array())
                    .and_then(|arr| arr.first())
                {
                    example.insert(field_name.to_string(), first.clone());
                    continue;
                }
                let type_str = field_schema
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("string");
                let value = match type_str {
                    "integer" | "number" => json!(0),
                    "boolean" => json!(false),
                    "array" => json!([]),
                    "object" => json!({}),
                    _ => json!("example"),
                };
                example.insert(field_name.to_string(), value);
            }
        }

        if example.is_empty() {
            None
        } else {
            Some(serde_json::Value::Object(example))
        }
    }
}

#[async_trait]
impl AgentTool for DescribeToolTool {
    fn name(&self) -> &str {
        "describe-tool"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        _context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        let name = payload
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AgentOSError::SchemaValidation("describe-tool requires 'name'".into())
            })?;
        let verbose = payload
            .get("verbose")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let tool = {
            let guard = self.tool_summaries.read().await;
            guard.iter().find(|t| t.name == name).cloned()
        }
        .ok_or_else(|| AgentOSError::ToolNotFound(name.to_string()))?;

        let input_schema_docs =
            AgentManualTool::public_summarize_input_schema(tool.input_schema.as_ref());

        let example = tool
            .input_schema
            .as_ref()
            .and_then(Self::make_example)
            .unwrap_or(serde_json::Value::Null);

        let mut result = json!({
            "name": tool.name,
            "description": tool.description,
            "version": tool.version,
            "trust_tier": tool.trust_tier,
            "category": tool.category,
            "tags": tool.tags,
            "risk_class": tool.risk_class,
            "permissions": tool.permissions,
            "capability_tags": tool.capability_tags,
            "input_schema_docs": input_schema_docs,
            "example": example,
        });

        if verbose {
            result["input_schema"] = tool.input_schema.clone().unwrap_or(serde_json::Value::Null);
        }

        Ok(result)
    }
}
