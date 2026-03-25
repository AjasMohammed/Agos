use agentos_types::IntentType;

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
