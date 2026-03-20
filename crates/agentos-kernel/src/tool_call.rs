use agentos_types::IntentType;
use regex::Regex;
use std::sync::LazyLock;
use tracing::warn;

static JSON_BLOCK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"```json\s*\n([\s\S]*?)\n```").expect("valid regex"));

const MAX_PARSED_PAYLOAD_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct ToolCallRequest {
    pub tool_name: String,
    pub intent_type: IntentType,
    pub payload: serde_json::Value,
}

pub type ParsedToolCall = ToolCallRequest;

pub fn parse_intent_type(intent_type_str: &str) -> Option<IntentType> {
    match intent_type_str {
        "read" => Some(IntentType::Read),
        "write" => Some(IntentType::Write),
        "execute" => Some(IntentType::Execute),
        "query" => Some(IntentType::Query),
        "observe" => Some(IntentType::Observe),
        "delegate" => Some(IntentType::Delegate),
        "message" => Some(IntentType::Message),
        "broadcast" => Some(IntentType::Broadcast),
        "escalate" => Some(IntentType::Escalate),
        "subscribe" => Some(IntentType::Subscribe),
        "unsubscribe" => Some(IntentType::Unsubscribe),
        _ => None,
    }
}

fn parse_call_value(value: serde_json::Value) -> Option<ToolCallRequest> {
    let intent_type_str = value.get("intent_type").and_then(|v| v.as_str())?;
    let intent_type = parse_intent_type(intent_type_str)?;

    let tool_name = value
        .get("tool")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| match intent_type {
            // Runtime event subscription intents can target the kernel directly.
            IntentType::Subscribe | IntentType::Unsubscribe => {
                Some("event-subscription".to_string())
            }
            _ => None,
        })?;

    let payload = value
        .get("payload")
        .cloned()
        .unwrap_or(serde_json::json!({}));
    let payload_size = serde_json::to_vec(&payload)
        .map(|bytes| bytes.len())
        .unwrap_or(0);
    if payload_size > MAX_PARSED_PAYLOAD_BYTES {
        warn!(
            payload_size_bytes = payload_size,
            payload_limit_bytes = MAX_PARSED_PAYLOAD_BYTES,
            tool = %tool_name,
            "Skipping tool call with oversized payload"
        );
        return None;
    }

    Some(ToolCallRequest {
        tool_name,
        intent_type,
        payload,
    })
}

/// Parse all valid tool calls from the LLM's text response.
///
/// Looks for ```json ... ``` blocks containing
/// {"tool": "...", "intent_type": "...", "payload": {...}} and returns every
/// valid call in source order.
pub fn parse_tool_calls(text: &str) -> Vec<ToolCallRequest> {
    let mut calls = Vec::new();

    for cap in JSON_BLOCK_RE.captures_iter(text) {
        let Some(json_str) = cap.get(1) else {
            continue;
        };
        match serde_json::from_str::<serde_json::Value>(json_str.as_str()) {
            Ok(value) => {
                if let Some(call) = parse_call_value(value) {
                    calls.push(call);
                } else {
                    warn!("Skipping malformed tool call JSON block");
                }
            }
            Err(error) => {
                warn!(%error, "Skipping invalid tool call JSON block");
            }
        }
    }

    calls
}

/// Parse the LLM's text response for a tool call JSON block.
/// Looks for ```json ... ``` blocks containing {"tool": "...", "intent_type": "...", "payload": {...}}
pub fn parse_tool_call(text: &str) -> Option<ToolCallRequest> {
    parse_tool_calls(text).into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_tool_call_valid() {
        let text = r#"I need to read a file. Let me do that.
```json
{"tool": "file-reader", "intent_type": "read", "payload": {"path": "/data/report.txt"}}
```"#;
        let call = parse_tool_call(text).unwrap();
        assert_eq!(call.tool_name, "file-reader");
        assert!(matches!(call.intent_type, IntentType::Read));
    }

    #[test]
    fn test_parse_tool_calls_returns_all_valid_blocks() {
        let text = r#"```json
{"tool": "file-reader", "intent_type": "read", "payload": {"path": "/tmp/a.txt"}}
```
```json
{"tool": "memory-read", "intent_type": "query", "payload": {"key": "project"}}
```"#;

        let calls = parse_tool_calls(text);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].tool_name, "file-reader");
        assert_eq!(calls[1].tool_name, "memory-read");
    }

    #[test]
    fn test_parse_tool_call_no_json() {
        let text = "Here is my final answer: the report is complete.";
        assert!(parse_tool_call(text).is_none());
    }

    #[test]
    fn test_parse_tool_call_invalid_json() {
        let text = "```json\n{invalid json}\n```";
        assert!(parse_tool_call(text).is_none());
    }

    #[test]
    fn test_parse_tool_call_subscribe_without_tool_name() {
        let text = r#"```json
{"intent_type": "subscribe", "payload": {"event_filter": "SecurityEvents.*", "duration": "Task"}}
```"#;
        let call = parse_tool_call(text).unwrap();
        assert_eq!(call.tool_name, "event-subscription");
        assert!(matches!(call.intent_type, IntentType::Subscribe));
    }

    #[test]
    fn test_parse_tool_call_skips_invalid_block_and_finds_valid_one() {
        let text = r#"```json
{"tool": "file-reader", "payload": {"path": "/tmp/a.txt"}}
```
```json
{"tool": "file-reader", "intent_type": "read", "payload": {"path": "/tmp/b.txt"}}
```"#;

        let call = parse_tool_call(text).expect("expected second valid block to be parsed");
        assert_eq!(call.tool_name, "file-reader");
        assert!(matches!(call.intent_type, IntentType::Read));
        assert_eq!(call.payload["path"], "/tmp/b.txt");
    }

    #[test]
    fn test_parse_tool_call_skips_oversized_payload() {
        let oversized = "x".repeat(MAX_PARSED_PAYLOAD_BYTES + 1);
        let text = format!(
            r#"```json
{{"tool": "file-reader", "intent_type": "read", "payload": {{"blob": "{}"}}}}
```
```json
{{"tool": "file-reader", "intent_type": "read", "payload": {{"path": "/tmp/ok.txt"}}}}
```"#,
            oversized
        );

        let calls = parse_tool_calls(&text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].payload["path"], "/tmp/ok.txt");
    }
}
