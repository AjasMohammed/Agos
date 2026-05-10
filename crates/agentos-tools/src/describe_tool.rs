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
                let type_str = Self::resolve_example_type(field_schema);
                let value = Self::placeholder_for_field(field_name, &type_str, field_schema);
                example.insert(field_name.to_string(), value);
            }
        }

        if example.is_empty() {
            None
        } else {
            Some(serde_json::Value::Object(example))
        }
    }

    /// Resolve a single representative type for example generation. Walks
    /// `oneOf`/`anyOf` so schemas like `{"oneOf": [{"type": "string"}, {"type": "array"}]}`
    /// (used by gmail_send.to) yield `"string"` instead of falling back to
    /// the generic placeholder. Picks the first scalar variant; otherwise the
    /// first variant's resolved type.
    fn resolve_example_type(field_schema: &serde_json::Value) -> String {
        if let Some(t) = field_schema.get("type").and_then(|v| v.as_str()) {
            return t.to_string();
        }
        for key in ["oneOf", "anyOf"] {
            if let Some(arr) = field_schema.get(key).and_then(|v| v.as_array()) {
                let scalar = arr.iter().find_map(|v| {
                    let t = v.get("type").and_then(|t| t.as_str())?;
                    matches!(t, "string" | "integer" | "number" | "boolean").then(|| t.to_string())
                });
                if let Some(s) = scalar {
                    return s;
                }
                if let Some(first) = arr.first() {
                    return Self::resolve_example_type(first);
                }
            }
        }
        "string".to_string()
    }

    /// Produce a realistic placeholder value. Field-name-aware: small models
    /// (gemma4, llama3.1-8b) copy examples verbatim, so generic `"example"`
    /// strings teach them nothing about valid email/URL/path shape and they
    /// invent synonym keys (`recipient`, `email_to`) instead. Real-shaped
    /// placeholders make the schema self-documenting.
    fn placeholder_for_field(
        field_name: &str,
        type_str: &str,
        field_schema: &serde_json::Value,
    ) -> serde_json::Value {
        match type_str {
            "integer" | "number" => return json!(0),
            "boolean" => return json!(false),
            "array" => {
                let item_placeholder = field_schema
                    .get("items")
                    .map(|items| {
                        let item_type = Self::resolve_example_type(items);
                        Self::placeholder_for_field(field_name, &item_type, items)
                    })
                    .unwrap_or(json!("example"));
                return json!([item_placeholder]);
            }
            "object" => return json!({}),
            _ => {}
        }
        let lower = field_name.to_ascii_lowercase();
        let placeholder = match lower.as_str() {
            "to" | "recipient" | "recipients" | "from" | "cc" | "bcc" | "email"
            | "email_address" | "to_address" | "from_address" => "user@example.com",
            "subject" => "Subject line",
            "body" | "body_text" | "message" | "text" | "content" => "Message body text",
            "body_html" | "html" => "<p>Message body HTML</p>",
            "url" | "uri" | "link" | "href" => "https://example.com/path",
            "path" | "file_path" | "filepath" | "filename" | "file" => "/absolute/path/to/file",
            "id" | "uuid" => "00000000-0000-0000-0000-000000000000",
            "name" => "tool-or-resource-name",
            "query" | "search" | "q" => "search query terms",
            "command" | "cmd" => "echo hello",
            "key" => "key",
            "value" => "value",
            "title" => "Title",
            "description" | "desc" => "Short description",
            "agent_id" => "agent-id",
            "task_id" => "task-id",
            "tool_name" => "agent-manual",
            "section" => "index",
            "session_id" => "session-id",
            "channel" | "channel_id" => "channel-id",
            "user_id" => "user-id",
            "language" | "lang" => "en",
            "model" => "gpt-4o-mini",
            "provider" => "openai",
            "phone" | "phone_number" => "+15551234567",
            "date" => "2026-01-01",
            "datetime" | "timestamp" => "2026-01-01T00:00:00Z",
            _ => "example",
        };
        json!(placeholder)
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
        context: ToolExecutionContext,
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

        // Task-scoped allowlist enforcement: a hidden tool returns
        // `ToolNotFound` (uniform with non-existent — no information leak about
        // categories the agent is restricted from seeing).
        if let Some(allowlist) = context.tool_categories.as_ref() {
            if !allowlist
                .iter()
                .any(|c| c.eq_ignore_ascii_case(&tool.category))
            {
                return Err(AgentOSError::ToolNotFound(name.to_string()));
            }
        }

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
