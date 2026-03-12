use agentos_types::IntentType;
use regex::Regex;

#[derive(Debug, Clone)]
pub struct ParsedToolCall {
    pub tool_name: String,
    pub intent_type: IntentType,
    pub payload: serde_json::Value,
}

/// Parse the LLM's text response for a tool call JSON block.
/// Looks for ```json ... ``` blocks containing {"tool": "...", "intent_type": "...", "payload": {...}}
pub fn parse_tool_call(text: &str) -> Option<ParsedToolCall> {
    let json_block_re = Regex::new(r"```json\s*\n([\s\S]*?)\n```").ok()?;

    for cap in json_block_re.captures_iter(text) {
        if let Some(json_str) = cap.get(1) {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(json_str.as_str()) {
                if let (Some(tool), Some(intent_type_str)) = (
                    value.get("tool").and_then(|v| v.as_str()),
                    value.get("intent_type").and_then(|v| v.as_str()),
                ) {
                    let intent_type = match intent_type_str {
                        "read" => IntentType::Read,
                        "write" => IntentType::Write,
                        "execute" => IntentType::Execute,
                        "query" => IntentType::Query,
                        "observe" => IntentType::Observe,
                        "delegate" => IntentType::Delegate,
                        "message" => IntentType::Message,
                        "broadcast" => IntentType::Broadcast,
                        "escalate" => IntentType::Escalate,
                        _ => return None,
                    };

                    return Some(ParsedToolCall {
                        tool_name: tool.to_string(),
                        intent_type,
                        payload: value
                            .get("payload")
                            .cloned()
                            .unwrap_or(serde_json::json!({})),
                    });
                }
            }
        }
    }
    None
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
    fn test_parse_tool_call_no_json() {
        let text = "Here is my final answer: the report is complete.";
        assert!(parse_tool_call(text).is_none());
    }

    #[test]
    fn test_parse_tool_call_invalid_json() {
        let text = "```json\n{invalid json}\n```";
        assert!(parse_tool_call(text).is_none());
    }
}
