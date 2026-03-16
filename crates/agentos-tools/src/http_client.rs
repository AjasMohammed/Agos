use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use reqwest::{Client, Method, Url};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;
use tracing::{info, warn};

pub struct HttpClientTool {
    client: Client,
}

impl HttpClientTool {
    pub fn new() -> Self {
        let client = Client::builder()
            // High global timeout as fallback; actual request timeout is per-call
            .timeout(Duration::from_secs(30))
            .user_agent("AgentOS/1.0")
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("Failed to build HTTP client");
        Self { client }
    }
}

impl Default for HttpClientTool {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpClientTool {
    fn map_reqwest_error(err: reqwest::Error, url_str: &str) -> AgentOSError {
        let reason = if err.is_timeout() {
            "Request timed out".to_string()
        } else if err.is_connect() {
            "Connection failed (DNS or connect error)".to_string()
        } else {
            format!("HTTP error: {}", err)
        };

        AgentOSError::ToolExecutionFailed {
            tool_name: "http-client".into(),
            reason: format!("Failed to request {}: {}", url_str, reason),
        }
    }
}

#[async_trait]
impl AgentTool for HttpClientTool {
    fn name(&self) -> &str {
        "http-client"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("network.outbound".into(), PermissionOp::Execute)]
    }

    async fn execute(
        &self,
        payload: Value,
        context: ToolExecutionContext,
    ) -> Result<Value, AgentOSError> {
        let method_str = payload
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("GET");
        let method = match method_str.to_uppercase().as_str() {
            "GET" => Method::GET,
            "POST" => Method::POST,
            "PUT" => Method::PUT,
            "PATCH" => Method::PATCH,
            "DELETE" => Method::DELETE,
            "HEAD" => Method::HEAD,
            _ => {
                return Err(AgentOSError::SchemaValidation(format!(
                    "Invalid HTTP method: {}",
                    method_str
                )))
            }
        };

        let url_str = payload.get("url").and_then(|v| v.as_str()).ok_or_else(|| {
            AgentOSError::SchemaValidation("http-client requires 'url' field".into())
        })?;

        let parsed_url = Url::parse(url_str)
            .map_err(|e| AgentOSError::SchemaValidation(format!("Invalid URL: {}", e)))?;

        // 2. SSRF Protection: Reject private and loopback IP ranges
        let is_test = std::env::var("AGENTOS_TEST_ALLOW_LOCAL").is_ok();

        if let Some(host_str) = parsed_url.host_str() {
            // Check if it parses as an IP address
            if let Ok(ip) = host_str.parse::<std::net::IpAddr>() {
                if !is_test
                    && (ip.is_loopback()
                        || is_private_ip(&ip)
                        || ip.is_unspecified()
                        || ip.is_multicast())
                {
                    return Err(AgentOSError::PermissionDenied {
                        resource: "network.outbound".into(),
                        operation: format!(
                            "SSRF protection blocked access to local/private IP: {}",
                            ip
                        ),
                    });
                }
            } else {
                // Determine if it resolves to a private IP (Best effort sync check,
                // but real protection requires configuring the actual HTTP client,
                // e.g. using a custom DNS resolver, which is complex in reqwest).
                // For now, we block literal 'localhost' and known local names.
                let lower_host = host_str.to_lowercase();
                if !is_test
                    && (lower_host == "localhost"
                        || lower_host.ends_with(".localhost")
                        || lower_host.ends_with(".local"))
                {
                    return Err(AgentOSError::PermissionDenied {
                        resource: "network.outbound".into(),
                        operation: format!(
                            "SSRF protection blocked access to local hostname: {}",
                            host_str
                        ),
                    });
                }
            }
        } else {
            return Err(AgentOSError::SchemaValidation("URL missing host".into()));
        }

        let timeout_ms = payload
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(10_000);

        let mut req_builder = self
            .client
            .request(method.clone(), url_str)
            .timeout(Duration::from_millis(timeout_ms));

        // 3. Optional standard Headers
        if let Some(headers) = payload.get("headers").and_then(|v| v.as_object()) {
            for (k, v) in headers {
                if let Some(v_str) = v.as_str() {
                    req_builder = req_builder.header(k, v_str);
                }
            }
        }

        // 4. Resolve and Inject Secret Headers (via ProxyVault — zero-exposure, Spec §3)
        if let Some(secret_headers) = payload.get("secret_headers").and_then(|v| v.as_object()) {
            let vault = context
                .vault
                .ok_or_else(|| AgentOSError::ToolExecutionFailed {
                    tool_name: "http-client".into(),
                    reason: "Context does not have vault access, cannot inject secret_headers"
                        .into(),
                })?;
            let agent_id = context.agent_id;

            for (k, v) in secret_headers {
                if let Some(v_str) = v.as_str() {
                    // We expect patterns like "Bearer $MY_TOKEN" or just "$MY_TOKEN"
                    let mut final_header_val = v_str.to_string();

                    // Simple variable substitution syntax: replace $VAR_NAME with actual vault string
                    if let Some(dollar_idx) = v_str.find('$') {
                        // Extract words starting with $
                        let parts: Vec<&str> = v_str[dollar_idx..]
                            .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '$')
                            .collect();
                        for part in parts {
                            if part.starts_with('$') && part.len() > 1 {
                                let secret_name = &part[1..]; // Strip '$'
                                let secret_val = vault
                                    .get(secret_name, agent_id)
                                    .await
                                    .map_err(|e| AgentOSError::ToolExecutionFailed {
                                        tool_name: "http-client".into(),
                                        reason: format!(
                                            "Failed to resolve secret '{}': {}",
                                            secret_name, e
                                        ),
                                    })?;

                                final_header_val =
                                    final_header_val.replace(part, secret_val.as_str());
                            }
                        }
                    } else {
                        // If no '$' found, treat the whole string as the secret name
                        let secret_val = vault
                            .get(v_str, agent_id)
                            .await
                            .map_err(|e| AgentOSError::ToolExecutionFailed {
                                tool_name: "http-client".into(),
                                reason: format!("Failed to resolve secret '{}': {}", v_str, e),
                            })?;
                        final_header_val = secret_val.as_str().to_string();
                    }

                    req_builder = req_builder.header(k, final_header_val);
                }
            }
        }

        // 5. Body
        if let Some(body) = payload.get("body") {
            if body.is_object() || body.is_array() {
                req_builder = req_builder.json(body);
            } else if let Some(s) = body.as_str() {
                req_builder = req_builder.body(s.to_string());
            }
        }

        info!(
            "http-client executing {} {} (timeout: {}ms)",
            method, url_str, timeout_ms
        );

        let start_time = std::time::Instant::now();
        let response = req_builder
            .send()
            .await
            .map_err(|e| Self::map_reqwest_error(e, url_str))?;
        let latency_ms = start_time.elapsed().as_millis() as u64;

        let status = response.status().as_u16();

        let mut resp_headers = HashMap::new();
        for (k, v) in response.headers() {
            if let Ok(v_str) = v.to_str() {
                resp_headers.insert(k.as_str().to_string(), v_str.to_string());
            }
        }

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_lowercase();

        // 6. Response processing + Truncation limits (10 MB cap)
        const MAX_BODY_BYTES: usize = 10 * 1024 * 1024;

        // Stream the response body with a size cap to avoid OOM
        let mut buffer = Vec::new();
        let mut is_truncated = false;
        let mut stream = response.bytes_stream();
        use futures_util::StreamExt;
        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "http-client".into(),
                reason: format!("Failed to read response chunk: {}", e),
            })?;
            buffer.extend_from_slice(&chunk);
            if buffer.len() > MAX_BODY_BYTES {
                is_truncated = true;
                warn!("http-client response exceeded 10MB limit! Truncating.");
                break;
            }
        }

        let processing_bytes = if is_truncated {
            &buffer[..MAX_BODY_BYTES]
        } else {
            &buffer
        };

        let body_json = if content_type.contains("application/json")
            || content_type.ends_with("+json")
        {
            // Attempt to parse JSON
            match serde_json::from_slice::<Value>(processing_bytes) {
                Ok(mut j) => {
                    if is_truncated {
                        if let Some(obj) = j.as_object_mut() {
                            obj.insert("_warning".to_string(), serde_json::json!("Response truncated to 10MB limit. JSON may be invalid at the tail."));
                        }
                    }
                    j
                }
                Err(e) => {
                    // Fall back to string if JSON parsing fails
                    let mut s = String::from_utf8_lossy(processing_bytes).into_owned();
                    if is_truncated {
                        s.push_str("\n\n...[TRUNCATED to 10MB]");
                    }
                    serde_json::json!({
                        "error_parsing_json": e.to_string(),
                        "raw_text": s
                    })
                }
            }
        } else if content_type.contains("text/")
            || content_type.contains("application/xml")
            || content_type.contains("application/x-www-form-urlencoded")
            || content_type.is_empty()
        {
            let mut s = String::from_utf8_lossy(processing_bytes).into_owned();
            if is_truncated {
                s.push_str("\n\n...[TRUNCATED to 10MB]");
            }
            serde_json::Value::String(s)
        } else {
            // Binary types: base64 encode
            use base64::{engine::general_purpose, Engine as _};
            let b64 = general_purpose::STANDARD.encode(processing_bytes);
            serde_json::json!({
                "base64_encoded": b64,
                "warning": if is_truncated { "Truncated to 10MB before encoding" } else { "" }
            })
        };

        Ok(serde_json::json!({
            "status": status,
            "headers": resp_headers,
            "body": body_json,
            "latency_ms": latency_ms,
            "truncated": is_truncated,
        }))
    }
}

fn is_private_ip(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ipv4) => ipv4.is_private() || ipv4.is_link_local(),
        std::net::IpAddr::V6(ipv6) => {
            ipv6.is_loopback()
                || ipv6.is_unspecified()
                || ipv6.is_multicast()
                // fc00::/7 - unique local
                || (ipv6.segments()[0] & 0xfe00) == 0xfc00
                // fe80::/10 - link local
                || (ipv6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}
