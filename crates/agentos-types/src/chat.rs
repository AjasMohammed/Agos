use serde::Serialize;

/// Structured SSE frame sent from the web server to the browser during chat streaming.
///
/// The kernel emits `ChatStreamEvent` (in `agentos-kernel`). The web SSE handler converts
/// each event into a `ChatStreamFrame`, serialises it as JSON, and sends it over a single
/// `chat-stream` EventSource event name. The browser dispatches on the `type` discriminator.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ChatStreamFrame {
    /// LLM is thinking (start of an inference iteration).
    Thinking { iteration: u32 },
    /// An incremental text chunk (one or more tokens).
    TextDelta { text: String },
    /// A tool call was detected; execution is starting.
    ToolStart { tool_name: String, iteration: u32 },
    /// A tool call completed.
    ToolResult {
        tool_name: String,
        result_preview: String,
        duration_ms: u64,
        success: bool,
    },
    /// New inference iteration started.
    Iteration { iteration: u32, reason: String },
    /// Inference completed — final answer.
    Done {
        answer: String,
        iterations: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        tokens_used: Option<u64>,
    },
    /// An error occurred during inference.
    Error { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serde_tag_discriminator() {
        let frame = ChatStreamFrame::TextDelta {
            text: "hello".into(),
        };
        let json = serde_json::to_string(&frame).unwrap();
        assert!(json.contains(r#""type":"text-delta""#));

        let frame = ChatStreamFrame::Thinking { iteration: 2 };
        let json = serde_json::to_string(&frame).unwrap();
        assert!(json.contains(r#""type":"thinking""#));
        assert!(json.contains(r#""iteration":2"#));

        let frame = ChatStreamFrame::Done {
            answer: "done".into(),
            iterations: 3,
            tokens_used: Some(150),
        };
        let json = serde_json::to_string(&frame).unwrap();
        assert!(json.contains(r#""type":"done""#));
        assert!(json.contains(r#""tokens_used":150"#));

        let frame = ChatStreamFrame::Done {
            answer: "done".into(),
            iterations: 1,
            tokens_used: None,
        };
        let json = serde_json::to_string(&frame).unwrap();
        assert!(!json.contains("tokens_used"));

        let frame = ChatStreamFrame::Error {
            message: "boom".into(),
        };
        let json = serde_json::to_string(&frame).unwrap();
        assert!(json.contains(r#""type":"error""#));
    }
}
