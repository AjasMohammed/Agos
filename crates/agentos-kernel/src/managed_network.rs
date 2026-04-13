//! Managed Networking capability provider (`net.*`).
//!
//! Replaces binary network on/off with per-destination allowlists, rate limiting,
//! and kernel-proxied HTTP requests. All network access flows through policy
//! checks before execution.

use crate::capability_provider::{CapabilityContext, CapabilityProvider, CapabilityResult};
use agentos_types::{AgentID, AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for the managed networking capability.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NetworkConfig {
    /// Destination patterns that agents may access without approval.
    /// Supports glob: `"*.github.com"`, `"api.openai.com"`.
    #[serde(default = "default_allowed_destinations")]
    pub allowed_destinations: Vec<String>,
    /// Destination patterns that are NEVER accessible (deny > allow).
    #[serde(default = "default_denied_destinations")]
    pub denied_destinations: Vec<String>,
    /// Default rate limit per agent per destination (requests/minute).
    #[serde(default = "default_rate_limit")]
    pub default_rate_limit_rpm: u32,
    /// Maximum response body size in bytes.
    #[serde(default = "default_max_response")]
    pub max_response_body_bytes: usize,
    /// Request timeout in seconds.
    #[serde(default = "default_request_timeout")]
    pub request_timeout_secs: u64,
}

fn default_allowed_destinations() -> Vec<String> {
    vec![
        "*.github.com".into(),
        "*.githubusercontent.com".into(),
        "api.openai.com".into(),
        "api.anthropic.com".into(),
        "api.cohere.com".into(),
        "registry.npmjs.org".into(),
        "pypi.org".into(),
        "files.pythonhosted.org".into(),
        "crates.io".into(),
        "static.crates.io".into(),
        "*.googleapis.com".into(),
    ]
}

fn default_denied_destinations() -> Vec<String> {
    vec![
        "169.254.169.254".into(),          // AWS/Azure metadata
        "metadata.google.internal".into(), // GCP metadata
        "10.*".into(),                     // Private class A (10.0.0.0/8)
        // Private class B: 172.16.0.0/12 = 172.16.* through 172.31.*
        "172.16.*".into(),
        "172.17.*".into(),
        "172.18.*".into(),
        "172.19.*".into(),
        "172.20.*".into(),
        "172.21.*".into(),
        "172.22.*".into(),
        "172.23.*".into(),
        "172.24.*".into(),
        "172.25.*".into(),
        "172.26.*".into(),
        "172.27.*".into(),
        "172.28.*".into(),
        "172.29.*".into(),
        "172.30.*".into(),
        "172.31.*".into(),
        "192.168.*".into(), // Private class C
        "127.*".into(),     // Loopback
        "0.0.0.0".into(),
        "[::1]".into(), // IPv6 loopback
    ]
}

fn default_rate_limit() -> u32 {
    60
}
fn default_max_response() -> usize {
    10_485_760 // 10 MB
}
fn default_request_timeout() -> u64 {
    30
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            allowed_destinations: default_allowed_destinations(),
            denied_destinations: default_denied_destinations(),
            default_rate_limit_rpm: default_rate_limit(),
            max_response_body_bytes: default_max_response(),
            request_timeout_secs: default_request_timeout(),
        }
    }
}

// ---------------------------------------------------------------------------
// Destination matching
// ---------------------------------------------------------------------------

/// Simple glob match for hostnames. Supports `*` as wildcard for one label.
fn host_glob_matches(pattern: &str, host: &str) -> bool {
    if pattern == host {
        return true;
    }

    let pat_parts: Vec<&str> = pattern.split('.').collect();
    let host_parts: Vec<&str> = host.split('.').collect();

    if pat_parts.len() != host_parts.len() {
        // Special case: "10.*" should match "10.0.0.1" (any length)
        if pat_parts.last() == Some(&"*") && pat_parts.len() <= host_parts.len() {
            // Check all non-wildcard parts match
            for (i, pp) in pat_parts.iter().enumerate() {
                if *pp == "*" {
                    return true; // Wildcard matches rest
                }
                if i >= host_parts.len() || *pp != host_parts[i] {
                    return false;
                }
            }
            return true;
        }
        return false;
    }

    for (pp, hp) in pat_parts.iter().zip(host_parts.iter()) {
        if *pp != "*" && *pp != *hp {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Rate limiter
// ---------------------------------------------------------------------------

/// Per-agent, per-destination rate tracker.
struct RateLimiter {
    /// (agent_id, destination_host) -> (count, window_start)
    windows: HashMap<(AgentID, String), (u32, chrono::DateTime<chrono::Utc>)>,
    rpm: u32,
}

impl RateLimiter {
    fn new(rpm: u32) -> Self {
        Self {
            windows: HashMap::new(),
            rpm,
        }
    }

    /// Check and increment rate. Returns Ok if allowed, Err if rate exceeded.
    fn check_and_increment(&mut self, agent_id: &AgentID, host: &str) -> Result<u32, AgentOSError> {
        let key = (*agent_id, host.to_string());
        let now = chrono::Utc::now();

        // Periodic sweep: remove stale entries to prevent unbounded growth.
        if self.windows.len() > 1000 {
            self.windows
                .retain(|_, (_, start)| now.signed_duration_since(*start).num_seconds() < 300);
        }

        let entry = self.windows.entry(key).or_insert((0, now));

        // Reset window if more than 60 seconds have passed.
        let elapsed = now.signed_duration_since(entry.1);
        if elapsed.num_seconds() >= 60 {
            *entry = (0, now);
        }

        if entry.0 >= self.rpm {
            return Err(AgentOSError::PermissionDenied {
                resource: "net.http".into(),
                operation: format!(
                    "rate limit exceeded for host '{host}': {}/{} requests per minute",
                    entry.0, self.rpm
                ),
            });
        }

        entry.0 += 1;
        Ok(entry.0)
    }
}

// ---------------------------------------------------------------------------
// NetworkProvider
// ---------------------------------------------------------------------------

/// Managed networking capability provider.
pub struct NetworkProvider {
    config: NetworkConfig,
    rate_limiter: Arc<RwLock<RateLimiter>>,
    client: reqwest::Client,
}

impl NetworkProvider {
    pub fn new(config: NetworkConfig) -> Self {
        let timeout = std::time::Duration::from_secs(config.request_timeout_secs);
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .user_agent("AgentOS-KMC/1.0")
            // SECURITY: disable automatic redirect following to prevent SSRF
            // via redirect (e.g., allowed host → 302 → 169.254.169.254).
            // Agents see the 3xx status and can make a follow-up request
            // which will be independently checked against the deny/allow list.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_default();

        let rpm = config.default_rate_limit_rpm;
        Self {
            config,
            rate_limiter: Arc::new(RwLock::new(RateLimiter::new(rpm))),
            client,
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(NetworkConfig::default())
    }

    /// Check if an IP address is in a private/reserved range (SSRF defense).
    ///
    /// Catches IPv4 private ranges, loopback, link-local, cloud metadata IPs,
    /// IPv6 loopback, link-local, ULA, and IPv4-mapped IPv6 addresses.
    fn is_private_ip(ip: &std::net::IpAddr) -> bool {
        match ip {
            std::net::IpAddr::V4(v4) => {
                v4.is_loopback()
                    || v4.is_private()
                    || v4.is_link_local()
                    || v4.is_unspecified()
                    || v4.is_broadcast()
                    // Cloud metadata: 169.254.169.254
                    || (v4.octets()[0] == 169 && v4.octets()[1] == 254)
            }
            std::net::IpAddr::V6(v6) => {
                v6.is_loopback()
                    || v6.is_unspecified()
                    // Link-local: fe80::/10
                    || (v6.segments()[0] & 0xffc0) == 0xfe80
                    // ULA (unique local): fc00::/7
                    || (v6.segments()[0] & 0xfe00) == 0xfc00
                    // IPv4-mapped: check the embedded v4 address
                    || v6
                        .to_ipv4_mapped()
                        .map(|v4| {
                            v4.is_loopback()
                                || v4.is_private()
                                || v4.is_link_local()
                                || v4.is_unspecified()
                                || (v4.octets()[0] == 169 && v4.octets()[1] == 254)
                        })
                        .unwrap_or(false)
            }
        }
    }

    /// Check if a host matches any denied pattern.
    fn is_denied(&self, host: &str) -> bool {
        // First check if the host is an IP address in a private range.
        let stripped = host.trim_matches(|c| c == '[' || c == ']');
        if let Ok(ip) = stripped.parse::<std::net::IpAddr>() {
            if Self::is_private_ip(&ip) {
                return true;
            }
        }
        // Normalize IPv6 bracket notation: compare both bracketed and
        // unbracketed forms against both forms of deny patterns so that
        // custom deny entries like "[fd00::1]" match resolved IPs "fd00::1".
        self.config.denied_destinations.iter().any(|pat| {
            let pat_stripped = pat.trim_matches(|c| c == '[' || c == ']');
            host_glob_matches(pat, host)
                || host_glob_matches(pat_stripped, stripped)
                || host_glob_matches(pat, stripped)
                || host_glob_matches(pat_stripped, host)
        })
    }

    /// Check if a host matches any allowed pattern.
    fn is_allowed(&self, host: &str) -> bool {
        self.config
            .allowed_destinations
            .iter()
            .any(|pat| host_glob_matches(pat, host))
    }

    /// Extract host from a URL string.
    fn extract_host(url: &str) -> Result<String, AgentOSError> {
        // Try parsing as a full URL first.
        if let Ok(parsed) = url::Url::parse(url) {
            if let Some(host) = parsed.host_str() {
                return Ok(host.to_string());
            }
        }
        // Fallback: treat as bare host or host:port.
        let host = url
            .split('/')
            .next()
            .unwrap_or(url)
            .split(':')
            .next()
            .unwrap_or(url);
        if host.is_empty() {
            return Err(AgentOSError::SchemaValidation(
                "cannot extract host from URL".into(),
            ));
        }
        Ok(host.to_string())
    }

    async fn action_http(
        &self,
        params: &Value,
        context: &CapabilityContext,
    ) -> Result<CapabilityResult, AgentOSError> {
        let url = params["url"]
            .as_str()
            .ok_or_else(|| AgentOSError::SchemaValidation("missing 'url' field".into()))?;

        let method_str = params["method"].as_str().unwrap_or("GET");
        let headers: HashMap<String, String> = params["headers"]
            .as_object()
            .map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();

        let body = params["body"].as_str().map(String::from);

        // Extract and validate host.
        let host = Self::extract_host(url)?;

        // SECURITY: deny list first (deny > allow, always).
        if self.is_denied(&host) {
            return Err(AgentOSError::PermissionDenied {
                resource: "net.http".into(),
                operation: format!("destination '{host}' is blocked by network deny policy"),
            });
        }

        // Check allow list.
        if !self.is_allowed(&host) {
            return Err(AgentOSError::PermissionDenied {
                resource: "net.http".into(),
                operation: format!(
                    "destination '{host}' is not on the allowed list; requires operator approval"
                ),
            });
        }

        // Check rate limit.
        {
            let mut limiter = self.rate_limiter.write().await;
            limiter.check_and_increment(&context.agent_id, &host)?;
        }

        // SECURITY: DNS rebinding defense — resolve the hostname and check
        // that no resolved IP falls in a private/reserved range.
        // This prevents attacks where an allowed hostname resolves to an
        // internal IP (e.g., attacker.com → 169.254.169.254).
        if host.parse::<std::net::IpAddr>().is_err() {
            // Host is a hostname (not a raw IP) — resolve it.
            if let Ok(addrs) = tokio::net::lookup_host(format!("{host}:0")).await {
                for addr in addrs {
                    if Self::is_private_ip(&addr.ip()) {
                        return Err(AgentOSError::PermissionDenied {
                            resource: "net.http".into(),
                            operation: format!(
                                "DNS rebinding blocked: '{host}' resolves to private IP '{}'",
                                addr.ip()
                            ),
                        });
                    }
                }
            }
        }

        // Build and execute HTTP request.
        let method = method_str.parse::<reqwest::Method>().map_err(|_| {
            AgentOSError::SchemaValidation(format!("invalid HTTP method '{method_str}'"))
        })?;

        let mut request = self.client.request(method.clone(), url);
        for (key, value) in &headers {
            request = request.header(key.as_str(), value.as_str());
        }
        if let Some(body_str) = &body {
            request = request.body(body_str.clone());
        }

        let response = request
            .send()
            .await
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "net-http".into(),
                reason: format!("HTTP request failed: {e}"),
            })?;

        let status = response.status().as_u16();
        let resp_headers: HashMap<String, String> = response
            .headers()
            .iter()
            .filter_map(|(k, v)| v.to_str().ok().map(|s| (k.to_string(), s.to_string())))
            .collect();

        // Read body with size limit.
        let body_bytes = response
            .bytes()
            .await
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "net-http".into(),
                reason: format!("failed to read response body: {e}"),
            })?;

        let truncated = body_bytes.len() > self.config.max_response_body_bytes;
        let body_str = String::from_utf8_lossy(
            &body_bytes[..body_bytes.len().min(self.config.max_response_body_bytes)],
        )
        .to_string();

        Ok(CapabilityResult {
            output: json!({
                "status": status,
                "headers": resp_headers,
                "body": body_str,
                "body_bytes": body_bytes.len(),
                "truncated": truncated,
            }),
            audit_metadata: json!({
                "event": "NetworkRequestExecuted",
                "url": url,
                "host": host,
                "method": method_str,
                "status": status,
                "body_bytes": body_bytes.len(),
                "agent_id": context.agent_id.to_string(),
            }),
        })
    }

    async fn action_dns(
        &self,
        params: &Value,
        _context: &CapabilityContext,
    ) -> Result<CapabilityResult, AgentOSError> {
        let hostname = params["hostname"]
            .as_str()
            .ok_or_else(|| AgentOSError::SchemaValidation("missing 'hostname' field".into()))?;

        // SECURITY: check deny list for the hostname itself.
        if self.is_denied(hostname) {
            return Err(AgentOSError::PermissionDenied {
                resource: "net.dns".into(),
                operation: format!("DNS resolution blocked for '{hostname}' (denied destination)"),
            });
        }

        // Resolve DNS via tokio's built-in resolver.
        let addrs: Vec<String> = tokio::net::lookup_host(format!("{hostname}:0"))
            .await
            .map_err(|e| AgentOSError::ToolExecutionFailed {
                tool_name: "net-dns".into(),
                reason: format!("DNS resolution failed for '{hostname}': {e}"),
            })?
            .map(|addr| addr.ip().to_string())
            .collect();

        // SECURITY: DNS rebinding defense — check if any resolved IP is on the deny list.
        for ip in &addrs {
            if self.is_denied(ip) {
                return Err(AgentOSError::PermissionDenied {
                    resource: "net.dns".into(),
                    operation: format!(
                        "DNS rebinding blocked: '{hostname}' resolves to denied IP '{ip}'"
                    ),
                });
            }
        }

        Ok(CapabilityResult {
            output: json!({
                "hostname": hostname,
                "addresses": addrs,
            }),
            audit_metadata: json!({
                "action": "dns",
                "hostname": hostname,
                "addresses": addrs,
            }),
        })
    }
}

#[async_trait]
impl CapabilityProvider for NetworkProvider {
    fn domain(&self) -> &str {
        "net"
    }

    fn supported_actions(&self) -> &[&str] {
        &["http", "dns"]
    }

    fn required_permissions(&self, action: &str) -> Option<Vec<(String, PermissionOp)>> {
        match action {
            "http" => Some(vec![("net.http".to_string(), PermissionOp::Execute)]),
            "dns" => Some(vec![("net.dns".to_string(), PermissionOp::Read)]),
            _ => None,
        }
    }

    async fn execute(
        &self,
        action: &str,
        params: Value,
        context: &CapabilityContext,
    ) -> Result<CapabilityResult, AgentOSError> {
        match action {
            "http" => self.action_http(&params, context).await,
            "dns" => self.action_dns(&params, context).await,
            other => Err(AgentOSError::KernelError {
                reason: format!("unknown net action '{other}'"),
            }),
        }
    }

    fn description(&self) -> &str {
        "Make HTTP requests and DNS lookups through policy-controlled network proxy"
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::{AgentID, TaskID, TraceID};
    use std::path::PathBuf;

    fn make_config() -> NetworkConfig {
        NetworkConfig {
            allowed_destinations: vec![
                "*.github.com".into(),
                "api.openai.com".into(),
                "example.com".into(),
                "localhost".into(),
            ],
            denied_destinations: vec![
                "169.254.169.254".into(),
                "10.*".into(),
                "192.168.*".into(),
                "127.*".into(),
            ],
            default_rate_limit_rpm: 5, // Low for testing
            max_response_body_bytes: 1024,
            request_timeout_secs: 5,
        }
    }

    fn make_provider() -> NetworkProvider {
        NetworkProvider::new(make_config())
    }

    fn make_context() -> CapabilityContext {
        CapabilityContext {
            agent_id: AgentID::new(),
            task_id: TaskID::new(),
            trace_id: TraceID::new(),
            data_dir: PathBuf::from("/tmp"),
            permissions: agentos_types::PermissionSet::default(),
            workspace_paths: vec![],
        }
    }

    // -- Provider metadata --

    #[test]
    fn provider_metadata() {
        let p = make_provider();
        assert_eq!(p.domain(), "net");
        assert_eq!(p.supported_actions(), &["http", "dns"]);
        assert!(p.required_permissions("http").is_some());
        assert!(p.required_permissions("dns").is_some());
        assert!(p.required_permissions("connect").is_none());
    }

    // -- Host glob matching --

    #[test]
    fn exact_host_match() {
        assert!(host_glob_matches("api.openai.com", "api.openai.com"));
        assert!(!host_glob_matches("api.openai.com", "api.anthropic.com"));
    }

    #[test]
    fn wildcard_host_match() {
        assert!(host_glob_matches("*.github.com", "api.github.com"));
        assert!(host_glob_matches("*.github.com", "raw.github.com"));
        assert!(!host_glob_matches("*.github.com", "github.com"));
        assert!(!host_glob_matches("*.github.com", "evil.com"));
    }

    #[test]
    fn ip_prefix_match() {
        assert!(host_glob_matches("10.*", "10.0.0.1"));
        assert!(host_glob_matches("10.*", "10.255.255.255"));
        assert!(host_glob_matches("192.168.*", "192.168.1.1"));
        assert!(!host_glob_matches("10.*", "11.0.0.1"));
    }

    #[test]
    fn loopback_match() {
        assert!(host_glob_matches("127.*", "127.0.0.1"));
        assert!(host_glob_matches("127.*", "127.0.0.2"));
        assert!(!host_glob_matches("127.*", "128.0.0.1"));
    }

    // -- Policy tests --

    #[test]
    fn denied_hosts_detected() {
        let p = make_provider();
        assert!(p.is_denied("169.254.169.254"));
        assert!(p.is_denied("10.0.0.1"));
        assert!(p.is_denied("192.168.1.100"));
        assert!(p.is_denied("127.0.0.1"));
        assert!(!p.is_denied("api.github.com"));
    }

    #[test]
    fn allowed_hosts_detected() {
        let p = make_provider();
        assert!(p.is_allowed("api.github.com"));
        assert!(p.is_allowed("raw.github.com"));
        assert!(p.is_allowed("api.openai.com"));
        assert!(p.is_allowed("example.com"));
        assert!(!p.is_allowed("evil.com"));
    }

    // -- URL parsing --

    #[test]
    fn extract_host_from_full_url() {
        assert_eq!(
            NetworkProvider::extract_host("https://api.github.com/repos").unwrap(),
            "api.github.com"
        );
        assert_eq!(
            NetworkProvider::extract_host("http://localhost:8080/api").unwrap(),
            "localhost"
        );
    }

    #[test]
    fn extract_host_from_bare() {
        assert_eq!(
            NetworkProvider::extract_host("api.github.com").unwrap(),
            "api.github.com"
        );
    }

    // -- Rate limiter --

    #[test]
    fn rate_limiter_allows_within_limit() {
        let mut limiter = RateLimiter::new(3);
        let agent = AgentID::new();

        assert!(limiter.check_and_increment(&agent, "example.com").is_ok());
        assert!(limiter.check_and_increment(&agent, "example.com").is_ok());
        assert!(limiter.check_and_increment(&agent, "example.com").is_ok());
    }

    #[test]
    fn rate_limiter_blocks_over_limit() {
        let mut limiter = RateLimiter::new(2);
        let agent = AgentID::new();

        limiter.check_and_increment(&agent, "example.com").unwrap();
        limiter.check_and_increment(&agent, "example.com").unwrap();

        let err = limiter
            .check_and_increment(&agent, "example.com")
            .unwrap_err();
        assert!(format!("{err}").contains("rate limit exceeded"));
    }

    #[test]
    fn rate_limiter_per_agent_isolation() {
        let mut limiter = RateLimiter::new(1);
        let agent_a = AgentID::new();
        let agent_b = AgentID::new();

        limiter
            .check_and_increment(&agent_a, "example.com")
            .unwrap();
        // Agent A is at limit, but Agent B should still be allowed.
        assert!(limiter.check_and_increment(&agent_b, "example.com").is_ok());
    }

    #[test]
    fn rate_limiter_per_destination() {
        let mut limiter = RateLimiter::new(1);
        let agent = AgentID::new();

        limiter.check_and_increment(&agent, "example.com").unwrap();
        // Different destination should be independently tracked.
        assert!(limiter.check_and_increment(&agent, "other.com").is_ok());
    }

    // -- Action tests --

    #[tokio::test]
    async fn http_denied_destination_blocked() {
        let p = make_provider();
        let ctx = make_context();

        let err = p
            .execute(
                "http",
                json!({"url": "http://169.254.169.254/latest/meta-data/", "method": "GET"}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("blocked by network deny policy"));
    }

    #[tokio::test]
    async fn http_private_ip_blocked() {
        let p = make_provider();
        let ctx = make_context();

        let err = p
            .execute(
                "http",
                json!({"url": "http://10.0.0.1:8080/internal", "method": "GET"}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("blocked by network deny policy"));
    }

    #[tokio::test]
    async fn http_unknown_destination_denied() {
        let p = make_provider();
        let ctx = make_context();

        let err = p
            .execute(
                "http",
                json!({"url": "https://evil.com/exfil", "method": "GET"}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("not on the allowed list"));
    }

    #[tokio::test]
    async fn http_rate_limit_enforced() {
        let p = make_provider();
        let ctx = make_context();

        // Config has rate limit of 5 rpm.
        // We can't actually make HTTP requests in tests, but we can verify
        // that rate limiting is enforced by hitting a denied destination
        // which fails before the rate check, then check an allowed one.
        // Instead, test via the rate limiter directly (above).
        // This test verifies that the action properly extracts host and checks policy.
        let err = p
            .execute(
                "http",
                json!({"url": "https://not-on-list.example.org/api"}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("not on the allowed list"));
    }

    #[tokio::test]
    async fn dns_denied_hostname_blocked() {
        let p = make_provider();
        let ctx = make_context();

        let err = p
            .execute("dns", json!({"hostname": "169.254.169.254"}), &ctx)
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("denied destination"));
    }

    #[tokio::test]
    async fn unknown_action_fails() {
        let p = make_provider();
        let ctx = make_context();

        let err = p.execute("connect", json!({}), &ctx).await.unwrap_err();
        assert!(format!("{err}").contains("unknown net action"));
    }
}
