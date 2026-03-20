use crate::ssrf::is_private_ip;
use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use base64::{engine::general_purpose, Engine as _};
use futures_util::StreamExt;
use reqwest::{Client, Method, Url};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tracing::{info, warn};

pub struct HttpClientTool {
    /// Client with redirects disabled (default).
    client: Client,
    /// Client that follows up to 10 redirects with SSRF checks on each hop.
    client_redirect: Client,
}

impl HttpClientTool {
    pub fn new() -> Result<Self, AgentOSError> {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("AgentOS/1.0")
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "http-client".into(),
                reason: format!("Failed to build HTTP client: {}", e),
            })?;

        // Custom policy: follow up to 10 hops but re-apply SSRF checks on
        // every redirect target so an attacker-controlled server cannot issue
        // a 302 to a private/loopback address.
        let client_redirect = Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("AgentOS/1.0")
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 10 {
                    return attempt.error("too many redirects (limit: 10)");
                }
                // Materialize any error message before consuming `attempt`.
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
            .build()
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "http-client".into(),
                reason: format!("Failed to build HTTP redirect client: {}", e),
            })?;

        Ok(Self {
            client,
            client_redirect,
        })
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
        // Defense-in-depth: verify network.outbound permission even if the
        // kernel already checked it, in case the tool is called directly.
        if !context
            .permissions
            .check("network.outbound", PermissionOp::Execute)
        {
            return Err(AgentOSError::PermissionDenied {
                resource: "network.outbound".to_string(),
                operation: "Execute".to_string(),
            });
        }

        // ── 1. Method ─────────────────────────────────────────────────────────
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

        // ── 2. URL + SSRF Protection ──────────────────────────────────────────
        let url_str = payload.get("url").and_then(|v| v.as_str()).ok_or_else(|| {
            AgentOSError::SchemaValidation("http-client requires 'url' field".into())
        })?;

        let parsed_url = Url::parse(url_str)
            .map_err(|e| AgentOSError::SchemaValidation(format!("Invalid URL: {}", e)))?;

        if parsed_url.scheme() != "http" && parsed_url.scheme() != "https" {
            return Err(AgentOSError::SchemaValidation(
                "http-client only supports http:// and https:// URLs".into(),
            ));
        }

        // The bypass env var is only effective in debug/test builds; in release
        // builds it is compiled out and `is_test` is always false.
        #[cfg(any(test, debug_assertions))]
        let is_test = std::env::var("AGENTOS_TEST_ALLOW_LOCAL").is_ok();
        #[cfg(not(any(test, debug_assertions)))]
        let is_test = false;
        if is_test {
            warn!("AGENTOS_TEST_ALLOW_LOCAL is set — SSRF protection DISABLED");
        }

        if let Some(host_str) = parsed_url.host_str() {
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
                // DNS pre-resolution check: detect hostnames that resolve to
                // private/local IPs at request time. Mitigates DNS rebinding.
                if !is_test {
                    let port = parsed_url
                        .port()
                        .unwrap_or(if parsed_url.scheme() == "https" {
                            443
                        } else {
                            80
                        });
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
                                tool_name: "http-client".into(),
                                reason: format!("DNS resolution failed for '{}': {}", host_str, e),
                            });
                        }
                    }
                }
            }
        } else {
            return Err(AgentOSError::SchemaValidation("URL missing host".into()));
        }

        // ── 3. Options ────────────────────────────────────────────────────────
        let timeout_ms = payload
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(10_000);

        let follow_redirects = payload
            .get("follow_redirects")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let stream_sse = payload
            .get("stream")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let save_to = payload.get("save_to").and_then(|v| v.as_str());

        // ── 4. save_to path validation (must happen before network I/O) ───────
        // Require fs.user_data:Write when writing to disk. The trait cannot
        // declare this statically since it depends on the payload, so we check
        // it here at runtime (defense-in-depth on top of the kernel's pre-check).
        let dest_path: Option<PathBuf> =
            if let Some(rel_path) = save_to {
                if !context
                    .permissions
                    .check("fs.user_data", PermissionOp::Write)
                {
                    return Err(AgentOSError::PermissionDenied {
                        resource: "fs.user_data".into(),
                        operation: "Write (required by save_to parameter)".into(),
                    });
                }

                // Resolve relative to data_dir; strip leading `/` so absolute
                // paths don't escape data_dir via PathBuf::join semantics.
                let requested = Path::new(rel_path);
                let resolved = if requested.is_absolute() {
                    let stripped = requested.strip_prefix("/").unwrap_or(requested);
                    context.data_dir.join(stripped)
                } else {
                    context.data_dir.join(requested)
                };

                // Lexical normalization (file may not exist yet, can't canonicalize).
                let normalized = normalize_path(&resolved);
                let canonical_data_dir = context.data_dir.canonicalize().map_err(|e| {
                    AgentOSError::ToolExecutionFailed {
                        tool_name: "http-client".into(),
                        reason: format!("Data directory error: {}", e),
                    }
                })?;

                if !normalized.starts_with(&canonical_data_dir) {
                    return Err(AgentOSError::PermissionDenied {
                        resource: "fs.user_data".into(),
                        operation: format!("Path traversal denied in save_to: {}", rel_path),
                    });
                }
                Some(normalized)
            } else {
                None
            };

        // ── 5. Build request ──────────────────────────────────────────────────
        let active_client = if follow_redirects {
            &self.client_redirect
        } else {
            &self.client
        };

        let mut req_builder = active_client
            .request(method.clone(), url_str)
            .timeout(Duration::from_millis(timeout_ms));

        // Standard headers
        if let Some(headers) = payload.get("headers").and_then(|v| v.as_object()) {
            for (k, v) in headers {
                if let Some(v_str) = v.as_str() {
                    req_builder = req_builder.header(k, v_str);
                }
            }
        }

        // ── 6. Secret headers via vault (zero-exposure, Spec §3) ──────────────
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
                    let mut final_header_val = v_str.to_string();

                    if let Some(dollar_idx) = v_str.find('$') {
                        let parts: Vec<&str> = v_str[dollar_idx..]
                            .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '$')
                            .collect();
                        for part in parts {
                            if part.starts_with('$') && part.len() > 1 {
                                let secret_name = &part[1..];
                                let secret_val =
                                    vault.get(secret_name, agent_id).await.map_err(|e| {
                                        AgentOSError::ToolExecutionFailed {
                                            tool_name: "http-client".into(),
                                            reason: format!(
                                                "Failed to resolve secret '{}': {}",
                                                secret_name, e
                                            ),
                                        }
                                    })?;
                                final_header_val =
                                    final_header_val.replace(part, secret_val.as_str());
                            }
                        }
                    } else {
                        let secret_val = vault.get(v_str, agent_id).await.map_err(|e| {
                            AgentOSError::ToolExecutionFailed {
                                tool_name: "http-client".into(),
                                reason: format!("Failed to resolve secret '{}': {}", v_str, e),
                            }
                        })?;
                        final_header_val = secret_val.as_str().to_string();
                    }

                    req_builder = req_builder.header(k, final_header_val);
                }
            }
        }

        // ── 7. Body (multipart takes precedence over body) ────────────────────
        if let Some(multipart_fields) = payload.get("multipart_fields").and_then(|v| v.as_object())
        {
            let mut form = reqwest::multipart::Form::new();
            for (key, value) in multipart_fields {
                if let Some(text) = value.as_str() {
                    form = form.text(key.clone(), text.to_string());
                } else if let Some(obj) = value.as_object() {
                    if let Some(b64) = obj.get("base64").and_then(|v| v.as_str()) {
                        let bytes = general_purpose::STANDARD.decode(b64).map_err(|e| {
                            AgentOSError::SchemaValidation(format!(
                                "Invalid base64 in multipart field '{}': {}",
                                key, e
                            ))
                        })?;
                        let mut part = reqwest::multipart::Part::bytes(bytes);
                        if let Some(filename) = obj.get("filename").and_then(|v| v.as_str()) {
                            part = part.file_name(filename.to_string());
                        }
                        if let Some(ct) = obj.get("content_type").and_then(|v| v.as_str()) {
                            part = part.mime_str(ct).map_err(|e| {
                                AgentOSError::SchemaValidation(format!(
                                    "Invalid content_type for multipart field '{}': {}",
                                    key, e
                                ))
                            })?;
                        }
                        form = form.part(key.clone(), part);
                    } else {
                        return Err(AgentOSError::SchemaValidation(format!(
                            "Multipart field '{}' object must contain a 'base64' key",
                            key
                        )));
                    }
                } else {
                    return Err(AgentOSError::SchemaValidation(format!(
                        "Multipart field '{}' must be a string or an object with a 'base64' key",
                        key
                    )));
                }
            }
            req_builder = req_builder.multipart(form);
        } else if let Some(body) = payload.get("body") {
            if body.is_object() || body.is_array() {
                req_builder = req_builder.json(body);
            } else if let Some(s) = body.as_str() {
                req_builder = req_builder.body(s.to_string());
            }
        }

        info!(
            "http-client {} {} (timeout={}ms redirects={} stream={} save_to={:?})",
            method, url_str, timeout_ms, follow_redirects, stream_sse, save_to
        );

        // ── 8. Send ───────────────────────────────────────────────────────────
        let start_time = std::time::Instant::now();
        let response = tokio::select! {
            result = req_builder.send() => {
                result.map_err(|e| Self::map_reqwest_error(e, url_str))?
            }
            _ = context.cancellation_token.cancelled() => {
                return Err(AgentOSError::ToolExecutionFailed {
                    tool_name: "http-client".into(),
                    reason: "Tool execution cancelled".into(),
                });
            }
        };
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

        let mut stream = response.bytes_stream();

        // ── 9a. SSE streaming mode ────────────────────────────────────────────
        if stream_sse {
            let mut raw = Vec::new();
            const SSE_MAX_BYTES: usize = 10 * 1024 * 1024;
            loop {
                tokio::select! {
                    chunk_opt = stream.next() => {
                        match chunk_opt {
                            Some(Ok(chunk)) => {
                                raw.extend_from_slice(&chunk);
                                if raw.len() > SSE_MAX_BYTES {
                                    warn!("http-client SSE stream exceeded 10MB, truncating");
                                    break;
                                }
                            }
                            Some(Err(e)) => {
                                return Err(AgentOSError::ToolExecutionFailed {
                                    tool_name: "http-client".into(),
                                    reason: format!("SSE stream error: {}", e),
                                });
                            }
                            None => break,
                        }
                    }
                    _ = context.cancellation_token.cancelled() => {
                        return Err(AgentOSError::ToolExecutionFailed {
                            tool_name: "http-client".into(),
                            reason: "Tool execution cancelled".into(),
                        });
                    }
                }
            }
            let text = String::from_utf8_lossy(&raw);
            let events = parse_sse_text(&text, 1000);
            let count = events.len();
            return Ok(serde_json::json!({
                "status": status,
                "headers": resp_headers,
                "events": events,
                "count": count,
                "latency_ms": latency_ms,
            }));
        }

        // ── 9b. Download-to-file mode ─────────────────────────────────────────
        if let Some(dest) = dest_path {
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent).await.map_err(|e| {
                    AgentOSError::ToolExecutionFailed {
                        tool_name: "http-client".into(),
                        reason: format!("Failed to create directory: {}", e),
                    }
                })?;
            }
            let mut file = tokio::fs::File::create(&dest).await.map_err(|e| {
                AgentOSError::ToolExecutionFailed {
                    tool_name: "http-client".into(),
                    reason: format!("Failed to create file {}: {}", dest.display(), e),
                }
            })?;
            let mut total_bytes = 0usize;
            const MAX_DOWNLOAD_BYTES: usize = 100 * 1024 * 1024; // 100 MB
            loop {
                tokio::select! {
                    chunk_opt = stream.next() => {
                        match chunk_opt {
                            Some(Ok(chunk)) => {
                                file.write_all(&chunk)
                                    .await
                                    .map_err(|e| AgentOSError::ToolExecutionFailed {
                                        tool_name: "http-client".into(),
                                        reason: format!("Failed to write to file: {}", e),
                                    })?;
                                total_bytes += chunk.len();
                                if total_bytes > MAX_DOWNLOAD_BYTES {
                                    warn!(
                                        "http-client download exceeded {}MB limit, aborting",
                                        MAX_DOWNLOAD_BYTES / (1024 * 1024)
                                    );
                                    drop(file);
                                    let _ = tokio::fs::remove_file(&dest).await;
                                    return Err(AgentOSError::ToolExecutionFailed {
                                        tool_name: "http-client".into(),
                                        reason: format!(
                                            "Download exceeded {}MB limit",
                                            MAX_DOWNLOAD_BYTES / (1024 * 1024)
                                        ),
                                    });
                                }
                            }
                            Some(Err(e)) => {
                                drop(file);
                                let _ = tokio::fs::remove_file(&dest).await;
                                return Err(AgentOSError::ToolExecutionFailed {
                                    tool_name: "http-client".into(),
                                    reason: format!("Download chunk error: {}", e),
                                });
                            }
                            None => break,
                        }
                    }
                    _ = context.cancellation_token.cancelled() => {
                        drop(file);
                        let _ = tokio::fs::remove_file(&dest).await;
                        return Err(AgentOSError::ToolExecutionFailed {
                            tool_name: "http-client".into(),
                            reason: "Tool execution cancelled".into(),
                        });
                    }
                }
            }
            return Ok(serde_json::json!({
                "status": status,
                "headers": resp_headers,
                "saved_to": save_to,
                "size_bytes": total_bytes,
                "content_type": content_type,
                "latency_ms": latency_ms,
            }));
        }

        // ── 9c. Standard buffered response (10 MB cap) ────────────────────────
        const MAX_BODY_BYTES: usize = 10 * 1024 * 1024;
        let mut buffer = Vec::new();
        let mut is_truncated = false;
        loop {
            tokio::select! {
                chunk_opt = stream.next() => {
                    match chunk_opt {
                        Some(Ok(chunk)) => {
                            buffer.extend_from_slice(&chunk);
                            if buffer.len() > MAX_BODY_BYTES {
                                is_truncated = true;
                                warn!("http-client response exceeded 10MB limit! Truncating.");
                                break;
                            }
                        }
                        Some(Err(e)) => {
                            return Err(AgentOSError::ToolExecutionFailed {
                                tool_name: "http-client".into(),
                                reason: format!("Failed to read response chunk: {}", e),
                            });
                        }
                        None => break,
                    }
                }
                _ = context.cancellation_token.cancelled() => {
                    return Err(AgentOSError::ToolExecutionFailed {
                        tool_name: "http-client".into(),
                        reason: "Tool execution cancelled".into(),
                    });
                }
            }
        }

        let processing_bytes = if is_truncated {
            &buffer[..MAX_BODY_BYTES]
        } else {
            &buffer
        };

        let body_json =
            if content_type.contains("application/json") || content_type.ends_with("+json") {
                match serde_json::from_slice::<Value>(processing_bytes) {
                    Ok(mut j) => {
                        if is_truncated {
                            if let Some(obj) = j.as_object_mut() {
                                obj.insert(
                                    "_warning".to_string(),
                                    serde_json::json!(
                                        "Response truncated to 10MB limit. JSON may be invalid."
                                    ),
                                );
                            }
                        }
                        j
                    }
                    Err(e) => {
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

/// Parse a buffered SSE response body into structured events.
///
/// Follows the WHATWG Server-Sent Events specification:
/// <https://html.spec.whatwg.org/multipage/server-sent-events.html>
///
/// Events are separated by blank lines; recognized fields are `data:`,
/// `event:`, `id:`, and `retry:` (retry is ignored). The leading space after
/// the colon is optional per spec. Data lines are joined with `\n` and
/// JSON-parsed when possible, otherwise returned as a plain string.
fn parse_sse_text(text: &str, max_events: usize) -> Vec<Value> {
    // Normalize CRLF and bare CR to LF (WHATWG spec §9.2.6).
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");

    let mut events: Vec<Value> = Vec::new();
    let mut current_data: Vec<String> = Vec::new();
    let mut current_event_type = "message".to_string();
    let mut current_id: Option<String> = None;

    for event_block in normalized.split("\n\n") {
        if event_block.trim().is_empty() {
            continue;
        }

        for line in event_block.lines() {
            // Strip field name and optional single leading space from value.
            if let Some(rest) = line.strip_prefix("data:") {
                current_data.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
            } else if line == "data" {
                current_data.push(String::new());
            } else if let Some(rest) = line.strip_prefix("event:") {
                current_event_type = rest.strip_prefix(' ').unwrap_or(rest).to_string();
            } else if let Some(rest) = line.strip_prefix("id:") {
                current_id = Some(rest.strip_prefix(' ').unwrap_or(rest).to_string());
            }
            // Lines starting with ':' are comments; retry: lines are ignored.
        }

        if !current_data.is_empty() {
            let data_str = current_data.join("\n");
            let data_value =
                serde_json::from_str::<Value>(&data_str).unwrap_or(Value::String(data_str));

            let mut event = serde_json::json!({
                "type": current_event_type,
                "data": data_value,
            });
            if let Some(id) = &current_id {
                event["id"] = Value::String(id.clone());
            }
            events.push(event);

            if events.len() >= max_events {
                return events;
            }
        }

        current_data.clear();
        current_event_type = "message".to_string();
        current_id = None;
    }

    events
}

/// Lexically normalize a path by resolving `.` and `..` without touching
/// the filesystem. Mirrors the same helper in `file_writer.rs`.
fn normalize_path(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            other => result.push(other),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sse_text_basic() {
        let text = "data: hello\n\ndata: world\n\n";
        let events = parse_sse_text(text, 100);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["type"], "message");
        assert_eq!(events[0]["data"], "hello");
        assert_eq!(events[1]["data"], "world");
    }

    #[test]
    fn test_parse_sse_text_json_data() {
        let text = "data: {\"key\": \"value\"}\n\n";
        let events = parse_sse_text(text, 100);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["data"]["key"], "value");
    }

    #[test]
    fn test_parse_sse_text_event_type() {
        let text = "event: update\ndata: payload\n\n";
        let events = parse_sse_text(text, 100);
        assert_eq!(events[0]["type"], "update");
        assert_eq!(events[0]["data"], "payload");
    }

    #[test]
    fn test_parse_sse_text_with_id() {
        let text = "id: 42\ndata: msg\n\n";
        let events = parse_sse_text(text, 100);
        assert_eq!(events[0]["id"], "42");
    }

    #[test]
    fn test_parse_sse_text_max_events_cap() {
        let text = "data: a\n\ndata: b\n\ndata: c\n\n";
        let events = parse_sse_text(text, 2);
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn test_parse_sse_text_multiline_data() {
        let text = "data: line1\ndata: line2\n\n";
        let events = parse_sse_text(text, 100);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["data"], "line1\nline2");
    }

    #[test]
    fn test_parse_sse_text_empty_blocks_ignored() {
        let text = "\n\ndata: real\n\n\n\n";
        let events = parse_sse_text(text, 100);
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn test_parse_sse_text_no_space_after_colon() {
        // Spec allows `data:value` without a leading space.
        let text = "data:nospace\nevent:ping\n\n";
        let events = parse_sse_text(text, 100);
        assert_eq!(events[0]["data"], "nospace");
        assert_eq!(events[0]["type"], "ping");
    }

    #[test]
    fn test_parse_sse_text_crlf_line_endings() {
        let text = "data: hello\r\n\r\ndata: world\r\n\r\n";
        let events = parse_sse_text(text, 100);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["data"], "hello");
        assert_eq!(events[1]["data"], "world");
    }

    #[test]
    fn test_normalize_path_rejects_traversal() {
        let base = PathBuf::from("/data/agent");
        let requested = base.join("../../etc/passwd");
        let normalized = normalize_path(&requested);
        assert!(!normalized.starts_with(&base));
    }

    #[test]
    fn test_normalize_path_allows_subdir() {
        let base = PathBuf::from("/data/agent");
        let requested = base.join("subdir/file.txt");
        let normalized = normalize_path(&requested);
        assert!(normalized.starts_with(&base));
    }
}
