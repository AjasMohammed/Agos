use crate::tool_helpers;
use crate::traits::LLMCore;
use crate::types::{InferenceResult, InferenceToolCall, ModelCapabilities, TokenUsage};
use agentos_types::*;
use async_trait::async_trait;
use reqwest::Client;
use secrecy::{ExposeSecret, SecretString};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::Instant;
use tracing::warn;

/// OpenAI API adapter for models like gpt-4o, gpt-3.5-turbo, etc.
pub struct OpenAICore {
    client: Client,
    api_key: SecretString,
    model: String,
    base_url: String,
    capabilities: ModelCapabilities,
}

impl OpenAICore {
    /// Create a new OpenAI adapter using the default base URL.
    pub fn new(api_key: SecretString, model: String) -> Self {
        Self::with_base_url(api_key, model, "https://api.openai.com/v1".to_string())
    }

    /// Create a new OpenAI adapter with a custom base URL.
    pub fn with_base_url(api_key: SecretString, model: String, base_url: String) -> Self {
        Self {
            client: Client::builder()
                .connect_timeout(std::time::Duration::from_secs(10))
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .expect("HTTP client TLS initialization failed"),
            api_key,
            model,
            base_url,
            capabilities: ModelCapabilities {
                context_window_tokens: 128_000,
                supports_images: true, // Typical for modern OpenAI models
                supports_tool_calling: true,
                supports_json_mode: true,
                max_output_tokens: 0,
            },
        }
    }

    /// Convert our internal `ContextWindow` to OpenAI's messages array format.
    fn format_messages(&self, context: &ContextWindow) -> Vec<serde_json::Value> {
        let mut messages = Vec::new();

        for entry in context.active_entries() {
            let role = match entry.role {
                ContextRole::User => "user",
                ContextRole::Assistant => "assistant",
                ContextRole::ToolResult => "user", // OpenAI doesn't have a distinct role for tool execution outputs without their specific tool call machinery, so we pass it as user/system equivalent.
                ContextRole::System => "system",
            };

            let content = match entry.role {
                ContextRole::ToolResult => {
                    // Prepend label
                    format!("Tool Result:\n{}", entry.content)
                }
                _ => entry.content.clone(),
            };

            messages.push(json!({
                "role": role,
                "content": content,
            }));
        }

        messages
    }

    fn build_openai_tools_payload(
        &self,
        tools: &[ToolManifest],
    ) -> (Vec<Value>, HashMap<String, String>) {
        let mut openai_tools = Vec::new();
        let mut intent_by_tool = HashMap::new();
        let mut seen_names = HashSet::new();

        for manifest in tools {
            let tool_name = manifest.manifest.name.trim();
            if tool_name.is_empty() {
                continue;
            }
            if !seen_names.insert(tool_name.to_string()) {
                continue;
            }

            let intent_type = tool_helpers::infer_intent_type_from_permissions(
                &manifest.capabilities_required.permissions,
            );
            intent_by_tool.insert(tool_name.to_string(), intent_type);

            openai_tools.push(json!({
                "type": "function",
                "function": {
                    "name": tool_name,
                    "description": manifest.manifest.description,
                    "parameters": tool_helpers::normalize_tool_input_schema(manifest.input_schema.as_ref()),
                }
            }));
        }

        (openai_tools, intent_by_tool)
    }

    fn build_request_body(
        &self,
        messages: Vec<Value>,
        tools: &[ToolManifest],
    ) -> (Value, HashMap<String, String>) {
        let (openai_tools, intent_by_tool) = self.build_openai_tools_payload(tools);

        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "stream": false
        });

        if !openai_tools.is_empty() {
            body["tools"] = Value::Array(openai_tools);
            body["tool_choice"] = json!("auto");
        }

        (body, intent_by_tool)
    }

    fn parse_message_content(message: &Value) -> String {
        match message.get("content") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(parts)) => parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        }
    }

    fn parse_tool_call_payload(tool_name: &str, arguments: Option<&Value>) -> Value {
        match arguments {
            Some(Value::String(raw)) => {
                if raw.trim().is_empty() {
                    json!({})
                } else {
                    match serde_json::from_str::<Value>(raw) {
                        Ok(value) => value,
                        Err(error) => {
                            warn!(
                                tool_name = tool_name,
                                error = %error,
                                "OpenAI tool call arguments were not valid JSON; using empty payload"
                            );
                            json!({})
                        }
                    }
                }
            }
            Some(Value::Object(_)) | Some(Value::Array(_)) => {
                arguments.cloned().unwrap_or_default()
            }
            Some(Value::Null) | None => json!({}),
            Some(_) => {
                warn!(
                    tool_name = tool_name,
                    "OpenAI tool call arguments were not an object/string; using empty payload"
                );
                json!({})
            }
        }
    }

    fn parse_openai_tool_calls(
        message: &Value,
        intent_by_tool: &HashMap<String, String>,
    ) -> Vec<InferenceToolCall> {
        let Some(calls) = message.get("tool_calls").and_then(Value::as_array) else {
            return Vec::new();
        };

        let mut parsed = Vec::new();
        for call in calls {
            if call.get("type").and_then(Value::as_str) != Some("function") {
                continue;
            }

            let Some(function_obj) = call.get("function").and_then(Value::as_object) else {
                continue;
            };
            let Some(tool_name) = function_obj
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
            else {
                continue;
            };

            let raw_payload =
                Self::parse_tool_call_payload(tool_name, function_obj.get("arguments"));
            let (payload, explicit_intent) = match raw_payload {
                Value::Object(mut obj) => {
                    // Extract explicit intent_type if the model included one.
                    // All remaining keys become the payload — we do NOT
                    // destructure a "payload" key because OpenAI function
                    // arguments are already the payload and a tool could
                    // legitimately use "payload" as an argument name.
                    let explicit_intent = obj
                        .remove("intent_type")
                        .and_then(|v| v.as_str().map(str::to_string));
                    (Value::Object(obj), explicit_intent)
                }
                other => (other, None),
            };

            if !tool_helpers::check_payload_size(tool_name, &payload) {
                continue;
            }

            let intent_type = explicit_intent.unwrap_or_else(|| {
                intent_by_tool
                    .get(tool_name)
                    .cloned()
                    .unwrap_or_else(|| "query".to_string())
            });

            parsed.push(InferenceToolCall {
                id: call.get("id").and_then(Value::as_str).map(str::to_string),
                tool_name: tool_name.to_string(),
                intent_type,
                payload,
            });
        }

        parsed
    }

    fn parse_response_json(
        &self,
        json_resp: &Value,
        intent_by_tool: &HashMap<String, String>,
        duration_ms: u64,
    ) -> Result<InferenceResult, AgentOSError> {
        let message = json_resp
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("message"))
            .ok_or_else(|| AgentOSError::LLMError {
                provider: "openai".to_string(),
                reason: "Missing choices[0].message in OpenAI response".to_string(),
            })?;

        let text = Self::parse_message_content(message);
        let tool_calls = Self::parse_openai_tool_calls(message, intent_by_tool);
        let text = tool_helpers::append_legacy_blocks(&text, &tool_calls);

        let prompt_tokens = json_resp["usage"]["prompt_tokens"].as_u64().unwrap_or(0);
        let completion_tokens = json_resp["usage"]["completion_tokens"]
            .as_u64()
            .unwrap_or(0);
        let total_tokens = json_resp["usage"]["total_tokens"].as_u64().unwrap_or(0);

        Ok(InferenceResult {
            text,
            tokens_used: TokenUsage {
                prompt_tokens,
                completion_tokens,
                total_tokens,
            },
            model: self.model.clone(),
            duration_ms,
            tool_calls,
            uncertainty: None,
        })
    }
}

#[async_trait]
impl LLMCore for OpenAICore {
    async fn infer(&self, context: &ContextWindow) -> Result<InferenceResult, AgentOSError> {
        self.infer_with_tools(context, &[]).await
    }

    async fn infer_with_tools(
        &self,
        context: &ContextWindow,
        tools: &[ToolManifest],
    ) -> Result<InferenceResult, AgentOSError> {
        let start_time = Instant::now();
        let url = format!("{}/chat/completions", self.base_url);
        let messages = self.format_messages(context);
        let (body, intent_by_tool) = self.build_request_body(messages, tools);

        let req = self
            .client
            .post(&url)
            .header(
                "Authorization",
                format!("Bearer {}", self.api_key.expose_secret()),
            )
            .header("Content-Type", "application/json")
            .json(&body);

        let res = req.send().await.map_err(|e| AgentOSError::LLMError {
            provider: "openai".to_string(),
            reason: format!("Reqwest failed: {}", e),
        })?;

        if !res.status().is_success() {
            let status = res.status();
            let text = res.text().await.unwrap_or_default();
            return Err(AgentOSError::LLMError {
                provider: "openai".to_string(),
                reason: format!("OpenAI API error {}: {}", status, text),
            });
        }

        let json_resp: serde_json::Value =
            res.json().await.map_err(|e| AgentOSError::LLMError {
                provider: "openai".to_string(),
                reason: format!("Failed to parse JSON response: {}", e),
            })?;
        self.parse_response_json(
            &json_resp,
            &intent_by_tool,
            start_time.elapsed().as_millis() as u64,
        )
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    async fn health_check(&self) -> crate::types::HealthStatus {
        use crate::types::HealthStatus;
        let start = std::time::Instant::now();
        let url = format!("{}/models", self.base_url);
        match self
            .client
            .get(&url)
            .header(
                "Authorization",
                format!("Bearer {}", self.api_key.expose_secret()),
            )
            .send()
            .await
        {
            Ok(res) if res.status().is_success() => {
                let latency = start.elapsed();
                if latency > std::time::Duration::from_secs(2) {
                    HealthStatus::Degraded {
                        reason: format!("High latency: {}ms", latency.as_millis()),
                    }
                } else {
                    HealthStatus::Healthy
                }
            }
            Ok(res) => HealthStatus::Unhealthy {
                reason: format!("HTTP {}", res.status()),
            },
            Err(e) => HealthStatus::Unhealthy {
                reason: format!("Connection failed: {e}"),
            },
        }
    }

    fn provider_name(&self) -> &str {
        "openai"
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::tool::{ToolCapabilities, ToolExecutor, ToolInfo, ToolOutputs, ToolSchema};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    #[test]
    fn test_format_messages() {
        let mut ctx = ContextWindow::new(5);
        ctx.push(ContextEntry {
            role: ContextRole::System,
            content: "You are a helpful assistant.".to_string(),
            metadata: None,
            timestamp: chrono::Utc::now(),
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::default(),
            category: ContextCategory::History,
            is_summary: false,
        });
        ctx.push(ContextEntry {
            role: ContextRole::User,
            content: "Hello".to_string(),
            metadata: None,
            timestamp: chrono::Utc::now(),
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::default(),
            category: ContextCategory::History,
            is_summary: false,
        });
        ctx.push(ContextEntry {
            role: ContextRole::ToolResult,
            content: "status: ok".to_string(),
            metadata: None,
            timestamp: chrono::Utc::now(),
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::default(),
            category: ContextCategory::History,
            is_summary: false,
        });

        let adapter = OpenAICore::new(SecretString::new("fake".into()), "gpt-4".into());
        let messages = adapter.format_messages(&ctx);

        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "Hello");
        assert_eq!(messages[2]["role"], "user");
        assert_eq!(messages[2]["content"], "Tool Result:\nstatus: ok");
    }

    fn make_manifest(
        name: &str,
        description: &str,
        permissions: Vec<&str>,
        input_schema: Option<Value>,
    ) -> ToolManifest {
        ToolManifest {
            manifest: ToolInfo {
                name: name.to_string(),
                version: "1.0.0".to_string(),
                description: description.to_string(),
                author: "agentos-core".to_string(),
                checksum: None,
                author_pubkey: None,
                signature: None,
                trust_tier: TrustTier::Core,
            },
            capabilities_required: ToolCapabilities {
                permissions: permissions.into_iter().map(str::to_string).collect(),
            },
            capabilities_provided: ToolOutputs { outputs: vec![] },
            intent_schema: ToolSchema {
                input: "Input".to_string(),
                output: "Output".to_string(),
            },
            input_schema,
            sandbox: ToolSandbox {
                network: false,
                fs_write: false,
                gpu: false,
                max_memory_mb: 64,
                max_cpu_ms: 1000,
                syscalls: vec![],
            },
            executor: ToolExecutor::default(),
        }
    }

    fn find_header_terminator(buf: &[u8]) -> Option<usize> {
        buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
    }

    fn parse_content_length(headers: &str) -> usize {
        headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                if name.eq_ignore_ascii_case("content-length") {
                    value.trim().parse::<usize>().ok()
                } else {
                    None
                }
            })
            .unwrap_or(0)
    }

    fn read_http_body(stream: &mut std::net::TcpStream) -> String {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 2048];
        let mut expected_total_len: Option<usize> = None;

        loop {
            let n = stream.read(&mut chunk).expect("failed to read request");
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);

            if expected_total_len.is_none() {
                if let Some(headers_end) = find_header_terminator(&buf) {
                    let headers = String::from_utf8_lossy(&buf[..headers_end]);
                    let content_len = parse_content_length(&headers);
                    expected_total_len = Some(headers_end + content_len);
                }
            }

            if let Some(total_len) = expected_total_len {
                if buf.len() >= total_len {
                    break;
                }
            }
        }

        let headers_end = find_header_terminator(&buf).expect("missing HTTP header terminator");
        String::from_utf8_lossy(&buf[headers_end..]).into_owned()
    }

    #[test]
    fn test_build_request_body_includes_openai_tools() {
        let adapter = OpenAICore::new(SecretString::new("fake".into()), "gpt-4o".into());
        let manifest = make_manifest(
            "file-reader",
            "Read a file",
            vec!["fs.user_data:r"],
            Some(json!({
                "type": "object",
                "required": ["path"],
                "properties": {
                    "path": { "type": "string" }
                }
            })),
        );

        let messages = vec![json!({
            "role": "user",
            "content": "read /tmp/a.txt"
        })];

        let (body, intent_map) = adapter.build_request_body(messages, &[manifest]);
        assert_eq!(body["tool_choice"], "auto");
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "file-reader");
        assert_eq!(
            body["tools"][0]["function"]["parameters"]["properties"]["path"]["type"],
            "string"
        );
        assert_eq!(intent_map.get("file-reader"), Some(&"read".to_string()));
    }

    #[test]
    fn test_parse_response_extracts_tool_calls() {
        let adapter = OpenAICore::new(SecretString::new("fake".into()), "gpt-4o".into());
        let mut intent_map = HashMap::new();
        intent_map.insert("file-reader".to_string(), "read".to_string());

        let response = json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call_abc",
                        "type": "function",
                        "function": {
                            "name": "file-reader",
                            "arguments": "{\"path\":\"test.txt\"}"
                        }
                    }]
                }
            }],
            "usage": {
                "prompt_tokens": 12,
                "completion_tokens": 3,
                "total_tokens": 15
            }
        });

        let result = adapter
            .parse_response_json(&response, &intent_map, 42)
            .expect("response should parse");
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].id.as_deref(), Some("call_abc"));
        assert_eq!(result.tool_calls[0].tool_name, "file-reader");
        assert_eq!(result.tool_calls[0].intent_type, "read");
        assert_eq!(result.tool_calls[0].payload["path"], "test.txt");
        assert!(result.text.contains("\"tool\":\"file-reader\""));
        assert!(result.text.contains("\"intent_type\":\"read\""));
        assert_eq!(result.tokens_used.total_tokens, 15);
    }

    #[test]
    fn test_parse_response_preserves_reasoning_text_with_tool_calls() {
        let adapter = OpenAICore::new(SecretString::new("fake".into()), "gpt-4o".into());
        let mut intent_map = HashMap::new();
        intent_map.insert("file-reader".to_string(), "read".to_string());

        let response = json!({
            "choices": [{
                "message": {
                    "content": "I will read the file first.",
                    "tool_calls": [{
                        "id": "call_abc",
                        "type": "function",
                        "function": {
                            "name": "file-reader",
                            "arguments": "{\"path\":\"test.txt\"}"
                        }
                    }]
                }
            }],
            "usage": {}
        });

        let result = adapter
            .parse_response_json(&response, &intent_map, 9)
            .expect("response should parse");
        assert!(result.text.starts_with("I will read the file first."));
        assert!(result.text.contains("\"tool\":\"file-reader\""));
        assert_eq!(result.tool_calls.len(), 1);
    }

    #[test]
    fn test_parse_response_with_text_only() {
        let adapter = OpenAICore::new(SecretString::new("fake".into()), "gpt-4o".into());
        let response = json!({
            "choices": [{
                "message": {
                    "content": "Final answer"
                }
            }],
            "usage": {
                "prompt_tokens": 4,
                "completion_tokens": 2,
                "total_tokens": 6
            }
        });

        let result = adapter
            .parse_response_json(&response, &HashMap::new(), 7)
            .expect("response should parse");
        assert_eq!(result.text, "Final answer");
        assert!(result.tool_calls.is_empty());
        assert_eq!(result.tokens_used.total_tokens, 6);
    }

    #[tokio::test]
    async fn test_infer_with_tools_sends_openai_tools_and_parses_tool_calls() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        let (tx, rx) = mpsc::channel::<Value>();

        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept connection");
            let req_body = read_http_body(&mut stream);
            let req_json: Value = serde_json::from_str(&req_body).expect("valid JSON body");
            tx.send(req_json).expect("send request body to test");

            let response_body = json!({
                "choices": [{
                    "message": {
                        "content": "Thinking before calling tool.",
                        "tool_calls": [{
                            "id": "call_abc",
                            "type": "function",
                            "function": {
                                "name": "file-reader",
                                "arguments": "{\"path\":\"test.txt\"}"
                            }
                        }]
                    }
                }],
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 5,
                    "total_tokens": 15
                }
            })
            .to_string();

            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        });

        let adapter = OpenAICore::with_base_url(
            SecretString::new("fake-key".into()),
            "gpt-4o".into(),
            format!("http://{}", addr),
        );

        let mut ctx = ContextWindow::new(5);
        ctx.push(ContextEntry {
            role: ContextRole::User,
            content: "Read test.txt".to_string(),
            metadata: None,
            timestamp: chrono::Utc::now(),
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: ContextPartition::default(),
            category: ContextCategory::History,
            is_summary: false,
        });

        let manifest = make_manifest(
            "file-reader",
            "Read a file",
            vec!["fs.user_data:r"],
            Some(json!({
                "type": "object",
                "required": ["path"],
                "properties": {
                    "path": {"type": "string"}
                }
            })),
        );

        let result = adapter
            .infer_with_tools(&ctx, &[manifest])
            .await
            .expect("inference should succeed");

        let captured = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("captured request");
        assert_eq!(captured["tool_choice"], "auto");
        assert_eq!(captured["tools"][0]["type"], "function");
        assert_eq!(captured["tools"][0]["function"]["name"], "file-reader");
        assert_eq!(
            captured["tools"][0]["function"]["parameters"]["properties"]["path"]["type"],
            "string"
        );

        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].tool_name, "file-reader");
        assert_eq!(result.tool_calls[0].intent_type, "read");
        assert_eq!(result.tool_calls[0].payload["path"], "test.txt");
        assert!(result.text.contains("Thinking before calling tool."));
        assert!(result.text.contains("\"tool\":\"file-reader\""));

        server.join().expect("server thread should complete");
    }
}
