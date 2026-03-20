//! Shared helpers for building tool payloads and parsing tool call responses
//! across LLM adapters (OpenAI, Anthropic, Gemini).

use crate::types::InferenceToolCall;
use serde_json::{json, Value};
use tracing::warn;

/// Maximum serialised payload size in bytes. Payloads exceeding this limit are
/// dropped to prevent oversized requests from consuming kernel resources.
/// Kept in sync with `MAX_PARSED_PAYLOAD_BYTES` in `agentos-kernel/src/tool_call.rs`.
pub const MAX_TOOL_PAYLOAD_BYTES: usize = 64 * 1024;

/// Infer an intent type string from a permission set.
///
/// Scans `ops` suffixes (after the `:`) for `x` (execute), `w` (write),
/// `r` (read) and returns the highest-privilege match.
pub fn infer_intent_type_from_permissions(permissions: &[String]) -> String {
    let mut has_read = false;
    let mut has_write = false;
    let mut has_execute = false;

    for permission in permissions {
        let ops = permission
            .split_once(':')
            .map(|(_, suffix)| suffix)
            .unwrap_or_default();
        if ops.contains('x') {
            has_execute = true;
        }
        if ops.contains('w') {
            has_write = true;
        }
        if ops.contains('r') {
            has_read = true;
        }
    }

    if has_execute {
        "execute".to_string()
    } else if has_write {
        "write".to_string()
    } else if has_read {
        "read".to_string()
    } else {
        "query".to_string()
    }
}

/// Ensure an input schema is a valid JSON Schema object.
///
/// If the schema is missing or not an object, returns a minimal
/// `{"type": "object", "properties": {}}` placeholder.
pub fn normalize_tool_input_schema(input_schema: Option<&Value>) -> Value {
    match input_schema.cloned() {
        Some(Value::Object(mut obj)) => {
            obj.entry("type".to_string())
                .or_insert_with(|| Value::String("object".to_string()));
            Value::Object(obj)
        }
        _ => json!({
            "type": "object",
            "properties": {}
        }),
    }
}

/// Render tool calls as legacy ```json blocks for backward compatibility
/// with the kernel text-based parser.
pub fn render_legacy_tool_blocks(tool_calls: &[InferenceToolCall]) -> String {
    tool_calls
        .iter()
        .map(|call| {
            format!(
                "```json\n{}\n```",
                json!({
                    "tool": call.tool_name,
                    "intent_type": call.intent_type,
                    "payload": call.payload
                })
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Check whether a serialised payload exceeds the size limit.
/// Returns `true` if the payload is within limits, `false` (with a warning) if oversized.
pub fn check_payload_size(tool_name: &str, payload: &Value) -> bool {
    let payload_bytes = serde_json::to_vec(payload).map(|b| b.len()).unwrap_or(0);
    if payload_bytes > MAX_TOOL_PAYLOAD_BYTES {
        warn!(
            tool_name,
            payload_bytes, "Skipping tool call with oversized payload"
        );
        return false;
    }
    true
}

/// Validate that a payload is a JSON object. Non-object values are
/// wrapped in `{"_raw": <value>}` with a warning.
pub fn validate_payload_object(tool_name: &str, provider: &str, value: Option<Value>) -> Value {
    match value {
        Some(Value::Object(obj)) => Value::Object(obj),
        Some(Value::Null) | None => json!({}),
        Some(other) => {
            warn!(
                tool_name,
                provider, "Tool call input was not an object; wrapping in _raw"
            );
            json!({"_raw": other})
        }
    }
}

/// Append legacy tool call blocks to text, preserving any existing reasoning.
pub fn append_legacy_blocks(text: &str, tool_calls: &[InferenceToolCall]) -> String {
    if tool_calls.is_empty() {
        return text.to_string();
    }
    let legacy_blocks = render_legacy_tool_blocks(tool_calls);
    if text.trim().is_empty() {
        legacy_blocks
    } else {
        format!("{text}\n\n{legacy_blocks}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_infer_intent_type_from_permissions() {
        assert_eq!(
            infer_intent_type_from_permissions(&["fs.user_data:r".to_string()]),
            "read"
        );
        assert_eq!(
            infer_intent_type_from_permissions(&["fs.user_data:rw".to_string()]),
            "write"
        );
        assert_eq!(
            infer_intent_type_from_permissions(&["shell:x".to_string()]),
            "execute"
        );
        assert_eq!(
            infer_intent_type_from_permissions(&["memory:".to_string()]),
            "query"
        );
        assert_eq!(infer_intent_type_from_permissions(&[]), "query");
    }

    #[test]
    fn test_normalize_tool_input_schema_adds_type() {
        let schema = json!({"properties": {"path": {"type": "string"}}});
        let normalized = normalize_tool_input_schema(Some(&schema));
        assert_eq!(normalized["type"], "object");
        assert_eq!(normalized["properties"]["path"]["type"], "string");
    }

    #[test]
    fn test_normalize_tool_input_schema_none() {
        let normalized = normalize_tool_input_schema(None);
        assert_eq!(normalized["type"], "object");
    }

    #[test]
    fn test_check_payload_size_within_limit() {
        let payload = json!({"key": "value"});
        assert!(check_payload_size("test-tool", &payload));
    }

    #[test]
    fn test_check_payload_size_oversized() {
        let big = "x".repeat(MAX_TOOL_PAYLOAD_BYTES + 1);
        let payload = json!({"data": big});
        assert!(!check_payload_size("test-tool", &payload));
    }

    #[test]
    fn test_validate_payload_object_with_object() {
        let val = Some(json!({"key": "value"}));
        let result = validate_payload_object("tool", "test", val);
        assert_eq!(result["key"], "value");
    }

    #[test]
    fn test_validate_payload_object_with_string() {
        let val = Some(json!("not an object"));
        let result = validate_payload_object("tool", "test", val);
        assert_eq!(result["_raw"], "not an object");
    }

    #[test]
    fn test_validate_payload_object_none() {
        let result = validate_payload_object("tool", "test", None);
        assert_eq!(result, json!({}));
    }

    #[test]
    fn test_render_legacy_tool_blocks() {
        let calls = vec![InferenceToolCall {
            id: Some("call_1".to_string()),
            tool_name: "file-reader".to_string(),
            intent_type: "read".to_string(),
            payload: json!({"path": "test.txt"}),
        }];
        let rendered = render_legacy_tool_blocks(&calls);
        assert!(rendered.contains("\"tool\":\"file-reader\""));
        assert!(rendered.contains("\"intent_type\":\"read\""));
    }

    #[test]
    fn test_append_legacy_blocks_empty_text() {
        let calls = vec![InferenceToolCall {
            id: None,
            tool_name: "tool".to_string(),
            intent_type: "query".to_string(),
            payload: json!({}),
        }];
        let result = append_legacy_blocks("", &calls);
        assert!(result.starts_with("```json"));
    }

    #[test]
    fn test_append_legacy_blocks_with_existing_text() {
        let calls = vec![InferenceToolCall {
            id: None,
            tool_name: "tool".to_string(),
            intent_type: "query".to_string(),
            payload: json!({}),
        }];
        let result = append_legacy_blocks("Reasoning here.", &calls);
        assert!(result.starts_with("Reasoning here."));
        assert!(result.contains("```json"));
    }

    #[test]
    fn test_append_legacy_blocks_no_calls() {
        let result = append_legacy_blocks("Just text.", &[]);
        assert_eq!(result, "Just text.");
    }
}
