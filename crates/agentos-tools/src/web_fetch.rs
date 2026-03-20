use crate::ssrf::is_private_ip;
use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::{Client, Url};

pub struct WebFetch {
    client: Client,
}

// Maximum body size before download is aborted (10 MB as raw bytes).
const MAX_BODY_BYTES: u64 = 10 * 1024 * 1024;

impl WebFetch {
    pub fn new() -> Result<Self, AgentOSError> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 5 {
                    return attempt.error("too many redirects (limit: 5)");
                }
                let block_reason: Option<String> = {
                    let url = attempt.url();
                    url.host_str().and_then(|host| {
                        if let Ok(ip) = host.parse::<std::net::IpAddr>() {
                            if is_private_ip(&ip) {
                                return Some(format!(
                                    "SSRF: redirect to private IP blocked: {}",
                                    ip
                                ));
                            }
                        } else {
                            let lower = host.to_lowercase();
                            if lower == "localhost"
                                || lower.ends_with(".localhost")
                                || lower.ends_with(".local")
                            {
                                return Some(format!(
                                    "SSRF: redirect to local hostname blocked: {}",
                                    host
                                ));
                            }
                        }
                        None
                    })
                };
                if let Some(reason) = block_reason {
                    attempt.error(reason)
                } else {
                    attempt.follow()
                }
            }))
            .user_agent("AgentOS/1.0")
            .build()
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "web-fetch".into(),
                reason: format!("Failed to build HTTP client: {}", e),
            })?;

        Ok(Self { client })
    }
}

/// Truncate to at most `max_chars` Unicode codepoints, preserving valid UTF-8.
fn safe_truncate(s: String, max_chars: usize) -> String {
    // char_indices().nth(max_chars) lands on the first character we want to drop.
    if let Some((byte_boundary, _)) = s.char_indices().nth(max_chars) {
        format!(
            "{}... [truncated at {} chars]",
            &s[..byte_boundary],
            max_chars
        )
    } else {
        s
    }
}

#[async_trait]
impl AgentTool for WebFetch {
    fn name(&self) -> &str {
        "web-fetch"
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("network.outbound".to_string(), PermissionOp::Execute)]
    }

    async fn execute(
        &self,
        payload: serde_json::Value,
        context: ToolExecutionContext,
    ) -> Result<serde_json::Value, AgentOSError> {
        if !context
            .permissions
            .check("network.outbound", PermissionOp::Execute)
        {
            return Err(AgentOSError::PermissionDenied {
                resource: "network.outbound".to_string(),
                operation: "Execute".to_string(),
            });
        }

        let url_str = payload.get("url").and_then(|v| v.as_str()).ok_or_else(|| {
            AgentOSError::SchemaValidation("web-fetch requires 'url' field".into())
        })?;

        // Parse and validate URL scheme
        let parsed = Url::parse(url_str)
            .map_err(|e| AgentOSError::SchemaValidation(format!("Invalid URL: {}", e)))?;

        if parsed.scheme() != "http" && parsed.scheme() != "https" {
            return Err(AgentOSError::SchemaValidation(
                "web-fetch only supports http:// and https:// URLs".into(),
            ));
        }

        // SSRF protection: block private/loopback/local hosts.
        // The bypass env var is only effective in debug/test builds; in release
        // builds it is compiled out and `is_test` is always false.
        #[cfg(any(test, debug_assertions))]
        let is_test = std::env::var("AGENTOS_TEST_ALLOW_LOCAL").is_ok();
        #[cfg(not(any(test, debug_assertions)))]
        let is_test = false;
        if is_test {
            tracing::warn!("web-fetch: SSRF protection bypassed via AGENTOS_TEST_ALLOW_LOCAL — do not set this in production");
        }
        if let Some(host_str) = parsed.host_str() {
            if let Ok(ip) = host_str.parse::<std::net::IpAddr>() {
                if !is_test && is_private_ip(&ip) {
                    return Err(AgentOSError::PermissionDenied {
                        resource: "network.outbound".into(),
                        operation: format!(
                            "SSRF protection blocked access to local/private IP: {}",
                            ip
                        ),
                    });
                }
            } else {
                let lower = host_str.to_lowercase();
                if !is_test
                    && (lower == "localhost"
                        || lower.ends_with(".localhost")
                        || lower.ends_with(".local"))
                {
                    return Err(AgentOSError::PermissionDenied {
                        resource: "network.outbound".into(),
                        operation: format!(
                            "SSRF protection blocked access to local hostname: {}",
                            host_str
                        ),
                    });
                }
                // DNS pre-resolution check: detect hostnames that resolve to private/local
                // IPs at request time. This mitigates DNS rebinding where an attacker-
                // controlled domain initially resolves to a public IP (passing the string
                // checks above) but later resolves to a private range.
                // Note: a TOCTOU window still exists between this check and the actual TCP
                // connection; a network-level firewall provides the strongest guarantee.
                if !is_test {
                    let port =
                        parsed
                            .port()
                            .unwrap_or(if parsed.scheme() == "https" { 443 } else { 80 });
                    match tokio::net::lookup_host(format!("{}:{}", host_str, port)).await {
                        Ok(addrs) => {
                            for addr in addrs {
                                let ip = addr.ip();
                                if is_private_ip(&ip) {
                                    return Err(AgentOSError::PermissionDenied {
                                        resource: "network.outbound".into(),
                                        operation: format!(
                                            "SSRF protection: '{}' resolves to private/local IP: {}",
                                            host_str, ip
                                        ),
                                    });
                                }
                            }
                        }
                        Err(e) => {
                            return Err(AgentOSError::ToolExecutionFailed {
                                tool_name: "web-fetch".into(),
                                reason: format!("DNS resolution failed for '{}': {}", host_str, e),
                            });
                        }
                    }
                }
            }
        } else {
            return Err(AgentOSError::SchemaValidation("URL missing host".into()));
        }

        let extract_text = payload
            .get("extract_text")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let max_chars = payload
            .get("max_chars")
            .and_then(|v| v.as_u64())
            .unwrap_or(32_000)
            .min(100_000) as usize;

        let response = tokio::select! {
            result = self.client.get(url_str).send() => {
                result.map_err(|e| AgentOSError::ToolExecutionFailed {
                    tool_name: "web-fetch".into(),
                    reason: format!("HTTP request failed: {}", e),
                })?
            }
            _ = context.cancellation_token.cancelled() => {
                return Err(AgentOSError::ToolExecutionFailed {
                    tool_name: "web-fetch".into(),
                    reason: "Tool execution cancelled".into(),
                });
            }
        };

        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        // Reject oversized responses before downloading body.
        if let Some(len) = response.content_length() {
            if len > MAX_BODY_BYTES {
                return Err(AgentOSError::ToolExecutionFailed {
                    tool_name: "web-fetch".into(),
                    reason: format!(
                        "Response body too large ({} bytes, max {} bytes)",
                        len, MAX_BODY_BYTES
                    ),
                });
            }
        }

        // Stream body in chunks, aborting early if it exceeds MAX_BODY_BYTES.
        // This prevents memory exhaustion when the server omits Content-Length.
        let mut buf: Vec<u8> = Vec::new();
        let mut stream = response.bytes_stream();
        loop {
            tokio::select! {
                chunk_opt = stream.next() => {
                    match chunk_opt {
                        Some(Ok(chunk)) => {
                            buf.extend_from_slice(&chunk);
                            if buf.len() as u64 > MAX_BODY_BYTES {
                                return Err(AgentOSError::ToolExecutionFailed {
                                    tool_name: "web-fetch".into(),
                                    reason: format!(
                                        "Response body too large (>{} bytes, max {} bytes)",
                                        buf.len(),
                                        MAX_BODY_BYTES
                                    ),
                                });
                            }
                        }
                        Some(Err(e)) => {
                            return Err(AgentOSError::ToolExecutionFailed {
                                tool_name: "web-fetch".into(),
                                reason: format!("Failed to read response body: {}", e),
                            });
                        }
                        None => break,
                    }
                }
                _ = context.cancellation_token.cancelled() => {
                    return Err(AgentOSError::ToolExecutionFailed {
                        tool_name: "web-fetch".into(),
                        reason: "Tool execution cancelled".into(),
                    });
                }
            }
        }
        let body = String::from_utf8_lossy(&buf).into_owned();

        let (content, was_extracted) = if extract_text && content_type.contains("html") {
            let text = html2text::from_read(body.as_bytes(), 80);
            (safe_truncate(text, max_chars), true)
        } else {
            (safe_truncate(body, max_chars), false)
        };

        let char_count = content.chars().count();

        Ok(serde_json::json!({
            "url": url_str,
            "status_code": status,
            "content_type": content_type,
            "text_extracted": was_extracted,
            "content": content,
            "char_count": char_count,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::*;

    fn ctx_no_perms() -> ToolExecutionContext {
        ToolExecutionContext {
            data_dir: std::path::PathBuf::from("/tmp"),
            task_id: TaskID::new(),
            agent_id: AgentID::new(),
            trace_id: TraceID::new(),
            permissions: PermissionSet::new(),
            vault: None,
            hal: None,
            file_lock_registry: None,
            agent_registry: None,
            task_registry: None,
            workspace_paths: vec![],
            cancellation_token: tokio_util::sync::CancellationToken::new(),
        }
    }

    fn ctx_with_network() -> ToolExecutionContext {
        let mut permissions = PermissionSet::new();
        permissions.grant("network.outbound".to_string(), false, false, true, None);
        ToolExecutionContext {
            permissions,
            ..ctx_no_perms()
        }
    }

    #[tokio::test]
    async fn web_fetch_rejects_non_http_scheme() {
        let tool = WebFetch::new().unwrap();
        let result = tool
            .execute(
                serde_json::json!({"url": "ftp://example.com"}),
                ctx_with_network(),
            )
            .await;
        assert!(matches!(result, Err(AgentOSError::SchemaValidation(_))));
    }

    #[tokio::test]
    async fn web_fetch_requires_network_permission() {
        let tool = WebFetch::new().unwrap();
        let result = tool
            .execute(
                serde_json::json!({"url": "https://example.com"}),
                ctx_no_perms(),
            )
            .await;
        assert!(matches!(result, Err(AgentOSError::PermissionDenied { .. })));
    }

    #[tokio::test]
    async fn web_fetch_requires_url_field() {
        let tool = WebFetch::new().unwrap();
        let result = tool
            .execute(serde_json::json!({}), ctx_with_network())
            .await;
        assert!(matches!(result, Err(AgentOSError::SchemaValidation(_))));
    }

    #[tokio::test]
    async fn web_fetch_blocks_loopback_ssrf() {
        let tool = WebFetch::new().unwrap();
        let result = tool
            .execute(
                serde_json::json!({"url": "http://127.0.0.1/"}),
                ctx_with_network(),
            )
            .await;
        assert!(matches!(result, Err(AgentOSError::PermissionDenied { .. })));
    }

    #[tokio::test]
    async fn web_fetch_blocks_private_ip_ssrf() {
        let tool = WebFetch::new().unwrap();
        let result = tool
            .execute(
                serde_json::json!({"url": "http://192.168.1.1/"}),
                ctx_with_network(),
            )
            .await;
        assert!(matches!(result, Err(AgentOSError::PermissionDenied { .. })));
    }

    #[tokio::test]
    async fn web_fetch_blocks_metadata_ip_ssrf() {
        let tool = WebFetch::new().unwrap();
        // 169.254.169.254 is link-local — AWS/GCP metadata endpoint
        let result = tool
            .execute(
                serde_json::json!({"url": "http://169.254.169.254/latest/meta-data/"}),
                ctx_with_network(),
            )
            .await;
        assert!(matches!(result, Err(AgentOSError::PermissionDenied { .. })));
    }

    #[tokio::test]
    async fn web_fetch_blocks_localhost_hostname() {
        let tool = WebFetch::new().unwrap();
        let result = tool
            .execute(
                serde_json::json!({"url": "http://localhost/"}),
                ctx_with_network(),
            )
            .await;
        assert!(matches!(result, Err(AgentOSError::PermissionDenied { .. })));
    }

    #[test]
    fn safe_truncate_handles_multibyte_utf8() {
        // Japanese characters are 3 bytes each; slicing at byte 3 would be mid-char
        let s = "日本語テスト".to_string(); // 6 chars, 18 bytes
        let truncated = safe_truncate(s, 3);
        assert!(truncated.starts_with("日本語"));
        assert!(truncated.contains("truncated"));
    }

    #[test]
    fn safe_truncate_no_truncation_when_short() {
        let s = "hello".to_string();
        let result = safe_truncate(s.clone(), 100);
        assert_eq!(result, s);
    }

    #[test]
    fn safe_truncate_exact_boundary() {
        let s = "abcde".to_string();
        let result = safe_truncate(s.clone(), 5);
        assert_eq!(result, s);
    }
}
