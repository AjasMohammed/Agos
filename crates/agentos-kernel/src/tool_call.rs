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
                let Some(intent_type_str) = value.get("intent_type").and_then(|v| v.as_str())
                else {
                    continue;
                };
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
                    "subscribe" => IntentType::Subscribe,
                    "unsubscribe" => IntentType::Unsubscribe,
                    _ => continue,
                };

                let Some(tool_name) = value
                    .get("tool")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .or_else(|| match intent_type {
                        // Runtime event subscription intents can target the kernel directly.
                        IntentType::Subscribe | IntentType::Unsubscribe => {
                            Some("event-subscription".to_string())
                        }
                        _ => None,
                    })
                else {
                    continue;
                };

                return Some(ParsedToolCall {
                    tool_name,
                    intent_type,
                    payload: value
                        .get("payload")
                        .cloned()
                        .unwrap_or(serde_json::json!({})),
                });
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
}
