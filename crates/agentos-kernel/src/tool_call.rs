use agentos_types::IntentType;
use regex::Regex;
use std::sync::OnceLock;

#[derive(Debug, Clone)]
pub struct ToolCallRequest {
    /// Provider-native tool call ID (e.g. OpenAI "call_abc", Anthropic "toolu_xyz").
    /// Threaded through to push_tool_result so the adapter can emit the correct
    /// tool_call_id in the context entry and reconstruct valid multi-turn sequences.
    pub id: Option<String>,
    pub tool_name: String,
    pub intent_type: IntentType,
    pub payload: serde_json::Value,
}

pub type ParsedToolCall = ToolCallRequest;

/// Try to parse a JSON tool call from plain LLM text output.
///
/// Handles models that output tool calls as text rather than using native function
/// calling. Supports the AgentOS tool call schema:
///   `{"tool": "name", "intent_type": "read", "payload": {...}}`
///
/// Strips leading/trailing whitespace and optional markdown code fences before parsing.
pub fn parse_tool_call_from_text(text: &str) -> Option<ToolCallRequest> {
    // Strip markdown code fences (```json ... ``` or ``` ... ```)
    let stripped = text.trim();
    let stripped = if stripped.starts_with("```") {
        let inner = stripped.trim_start_matches('`').trim_start_matches("json");
        if let Some(end) = inner.rfind("```") {
            inner[..end].trim()
        } else {
            inner.trim()
        }
    } else {
        stripped
    };

    // Must start with '{' to be a JSON object
    if !stripped.starts_with('{') {
        return None;
    }

    let value: serde_json::Value = serde_json::from_str(stripped).ok()?;
    let obj = value.as_object()?;

    let tool_name = obj.get("tool")?.as_str()?.to_string();
    if tool_name.is_empty() {
        return None;
    }

    let intent_type_str = obj
        .get("intent_type")
        .and_then(|v| v.as_str())
        .unwrap_or("query");
    let intent_type =
        parse_intent_type(intent_type_str).unwrap_or(agentos_types::IntentType::Query);

    let payload = obj
        .get("payload")
        .cloned()
        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

    Some(ToolCallRequest {
        id: None,
        tool_name,
        intent_type,
        payload,
    })
}

/// Extract every tool call embedded in fenced JSON code blocks within `content`.
///
/// Small models (gemma, llama3, phi-3) often emit multiple tool calls as a series
/// of ```json {...} ``` blocks in the response text instead of structured tool-use
/// blocks. This scans every fenced block, parses each as a tool call, and returns
/// the ones that match the AgentOS schema. Non-tool JSON (e.g. data payloads) is
/// silently skipped.
pub fn extract_text_tool_calls(content: &str) -> Vec<ToolCallRequest> {
    static FENCE_RE: OnceLock<Regex> = OnceLock::new();
    let re = FENCE_RE.get_or_init(|| {
        Regex::new(r"(?s)```(?:json)?\s*(\{.*?\})\s*```")
            .expect("FENCE_RE: static fence regex must compile")
    });

    let mut calls = Vec::new();
    for cap in re.captures_iter(content) {
        let json_str = &cap[1];
        let Ok(value) = serde_json::from_str::<serde_json::Value>(json_str) else {
            continue;
        };
        let Some(obj) = value.as_object() else {
            continue;
        };

        let Some(tool_name) = obj
            .get("tool")
            .or_else(|| obj.get("name"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from)
        else {
            continue;
        };

        let intent_type_str = obj
            .get("intent_type")
            .and_then(|v| v.as_str())
            .unwrap_or("query");
        let intent_type = parse_intent_type(intent_type_str).unwrap_or(IntentType::Query);

        let payload = obj
            .get("payload")
            .or_else(|| obj.get("arguments"))
            .or_else(|| obj.get("input"))
            .cloned()
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

        calls.push(ToolCallRequest {
            id: None,
            tool_name,
            intent_type,
            payload,
        });
    }
    calls
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_single_fenced_tool_call() {
        let content = "I'll do this:\n```json\n{\"tool\": \"agent-self\", \"payload\": {}}\n```";
        let calls = extract_text_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool_name, "agent-self");
    }

    #[test]
    fn extract_multiple_fenced_tool_calls() {
        let content = "step one:\n```json\n{\"tool\": \"memory-search\", \"payload\": {\"query\": \"x\"}}\n```\nthen:\n```json\n{\"tool\": \"memory-write\", \"arguments\": {\"content\": \"y\"}}\n```";
        let calls = extract_text_tool_calls(content);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].tool_name, "memory-search");
        assert_eq!(calls[1].tool_name, "memory-write");
        assert_eq!(calls[1].payload["content"], "y");
    }

    #[test]
    fn extract_skips_non_tool_json() {
        let content = "data:\n```json\n{\"hello\": \"world\"}\n```";
        assert!(extract_text_tool_calls(content).is_empty());
    }

    #[test]
    fn extract_skips_invalid_json_continues_with_others() {
        let content =
            "broken:\n```json\nnot json\n```\nvalid:\n```json\n{\"tool\": \"agent-self\"}\n```";
        let calls = extract_text_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool_name, "agent-self");
    }

    #[test]
    fn extract_uses_name_field_alias() {
        let content = "```json\n{\"name\": \"agent-list\", \"input\": {\"limit\": 5}}\n```";
        let calls = extract_text_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool_name, "agent-list");
        assert_eq!(calls[0].payload["limit"], 5);
    }

    #[test]
    fn extract_returns_empty_for_no_fences() {
        assert!(extract_text_tool_calls("just plain text, no fences").is_empty());
    }
}
