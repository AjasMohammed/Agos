/// Web search tool — queries multiple search providers and returns results.
///
/// Tries providers in preference order: Brave → Tavily → Serper → DuckDuckGo (scrape).
/// API keys are read from environment variables at construction time.
/// The last-resort DDG scraper requires no key but is less reliable.
use crate::ssrf::is_private_ip;
use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use reqwest::{Client, Url};
use serde_json::{json, Value};
use zeroize::Zeroizing;

/// A single search result from any provider.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

pub struct WebSearchTool {
    client: Client,
    /// Optional Brave Search API key (env: BRAVE_API_KEY).
    brave_key: Option<Zeroizing<String>>,
    /// Optional Tavily API key (env: TAVILY_API_KEY).
    tavily_key: Option<Zeroizing<String>>,
    /// Optional Serper API key (env: SERPER_API_KEY).
    serper_key: Option<Zeroizing<String>>,
}

impl WebSearchTool {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Default for WebSearchTool {
    fn default() -> Self {
        Self {
            // Client::new() cannot fail — avoids `.expect()` in production paths.
            client: Client::new(),
            brave_key: std::env::var("BRAVE_API_KEY").ok().map(Zeroizing::new),
            tavily_key: std::env::var("TAVILY_API_KEY").ok().map(Zeroizing::new),
            serper_key: std::env::var("SERPER_API_KEY").ok().map(Zeroizing::new),
        }
    }
}

/// Return `true` if the URL's host should be blocked for SSRF protection.
/// Covers IP literals, DNS-rebinding-prone hostnames, and mDNS domains.
fn is_ssrf_blocked_url(url_str: &str) -> bool {
    let Ok(parsed) = Url::parse(url_str) else {
        return true; // unparseable → block
    };
    let Some(host) = parsed.host_str() else {
        return true; // no host → block
    };

    // Block known-dangerous hostnames regardless of DNS resolution.
    let host_lower = host.to_lowercase();
    if host_lower == "localhost"
        || host_lower.ends_with(".localhost")
        || host_lower.ends_with(".local")
        || host_lower.ends_with(".internal")
        || host_lower.ends_with(".corp")
    {
        return true;
    }

    // Block IP literals (private, loopback, link-local, etc.).
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return is_private_ip(&ip);
    }

    false
}

impl WebSearchTool {
    async fn search_brave(&self, query: &str, limit: usize) -> anyhow::Result<Vec<SearchResult>> {
        let key = self
            .brave_key
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("BRAVE_API_KEY not set"))?;
        let url = format!(
            "https://api.search.brave.com/res/v1/web/search?q={}&count={}",
            urlencoding::encode(query),
            limit.min(20)
        );
        let resp: Value = self
            .client
            .get(&url)
            .header("Accept", "application/json")
            // Brave Search API uses X-Subscription-Token (not Bearer Authorization).
            .header("X-Subscription-Token", key.as_str())
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let results = resp["web"]["results"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .take(limit)
            .map(|r| SearchResult {
                title: r["title"].as_str().unwrap_or("").to_string(),
                url: r["url"].as_str().unwrap_or("").to_string(),
                snippet: r["description"].as_str().unwrap_or("").to_string(),
            })
            .filter(|r| !r.url.is_empty())
            .collect();
        Ok(results)
    }

    async fn search_tavily(&self, query: &str, limit: usize) -> anyhow::Result<Vec<SearchResult>> {
        let key = self
            .tavily_key
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("TAVILY_API_KEY not set"))?;
        let body = json!({
            "api_key": key.as_str(),
            "query": query,
            "max_results": limit.min(10),
        });
        let resp: Value = self
            .client
            .post("https://api.tavily.com/search")
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let results = resp["results"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .take(limit)
            .map(|r| SearchResult {
                title: r["title"].as_str().unwrap_or("").to_string(),
                url: r["url"].as_str().unwrap_or("").to_string(),
                snippet: r["content"].as_str().unwrap_or("").to_string(),
            })
            .filter(|r| !r.url.is_empty())
            .collect();
        Ok(results)
    }

    async fn search_serper(&self, query: &str, limit: usize) -> anyhow::Result<Vec<SearchResult>> {
        let key = self
            .serper_key
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("SERPER_API_KEY not set"))?;
        let body = json!({ "q": query, "num": limit.min(10) });
        let resp: Value = self
            .client
            .post("https://google.serper.dev/search")
            .header("X-API-KEY", key.as_str())
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let results = resp["organic"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .take(limit)
            .map(|r| SearchResult {
                title: r["title"].as_str().unwrap_or("").to_string(),
                url: r["link"].as_str().unwrap_or("").to_string(),
                snippet: r["snippet"].as_str().unwrap_or("").to_string(),
            })
            .filter(|r| !r.url.is_empty())
            .collect();
        Ok(results)
    }

    /// DuckDuckGo HTML scrape — no API key required, last-resort fallback.
    ///
    /// Uses safe byte-boundary slicing via `str::get()` to avoid panics on
    /// non-ASCII HTML (international characters, encoded entities, etc.).
    async fn search_ddg(&self, query: &str, limit: usize) -> anyhow::Result<Vec<SearchResult>> {
        let url = format!(
            "https://html.duckduckgo.com/html/?q={}",
            urlencoding::encode(query)
        );
        let html = self
            .client
            .get(&url)
            .header("Accept", "text/html")
            .header(
                "User-Agent",
                "Mozilla/5.0 (compatible; AgentOS/1.0; +https://agentos.ai)",
            )
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;

        let mut results = Vec::new();
        let mut pos = 0;

        while results.len() < limit {
            // Find a result anchor with class="result__a"
            let href_offset = match html.get(pos..).and_then(|s| s.find("class=\"result__a\"")) {
                Some(o) => o,
                None => break,
            };
            let abs = pos + href_offset;

            // Find href=" before the result__a marker
            let url_start = match html.get(..abs).and_then(|s| s.rfind("href=\"")) {
                Some(i) => i + 6, // skip past `href="`
                None => {
                    pos = abs + 1;
                    continue;
                }
            };

            let url_raw_str = match html.get(url_start..) {
                Some(s) => s,
                None => break,
            };
            let url_end = match url_raw_str.find('"') {
                Some(e) => e,
                None => break,
            };
            let url_raw = match url_raw_str.get(..url_end) {
                Some(s) => s,
                None => break,
            };

            // Resolve DDG redirect URLs (//duckduckgo.com/l/?uddg=<encoded_url>)
            let final_url = if url_raw.starts_with("//duckduckgo.com/l/") {
                url_raw
                    .find("uddg=")
                    .and_then(|i| {
                        let encoded = &url_raw[i + 5..];
                        let end = encoded.find('&').unwrap_or(encoded.len());
                        urlencoding::decode(encoded.get(..end)?)
                            .ok()
                            .map(|c| c.to_string())
                    })
                    .unwrap_or_else(|| url_raw.to_string())
            } else {
                url_raw.to_string()
            };

            // Extract anchor text as the title (text between '>' and '</a>').
            let title = html
                .get(abs..)
                .and_then(|s| {
                    let tag_open_end = s.find('>')?;
                    let after_tag = s.get(tag_open_end + 1..)?;
                    let close = after_tag.find("</a>").unwrap_or(after_tag.len().min(120));
                    after_tag.get(..close).map(|t| {
                        // Strip inline HTML tags (e.g. <b> highlights) from title.
                        let mut out = String::with_capacity(close);
                        let mut in_tag = false;
                        for ch in t.chars() {
                            match ch {
                                '<' => in_tag = true,
                                '>' => in_tag = false,
                                _ if !in_tag => out.push(ch),
                                _ => {}
                            }
                        }
                        out.trim().to_string()
                    })
                })
                .unwrap_or_else(|| final_url.clone());

            // Extract snippet text (result__snippet class).
            let snippet = html
                .get(abs..)
                .and_then(|s| {
                    let snip_start = s.find("result__snippet")?;
                    let after_class = s.get(snip_start..)?;
                    let text_start = after_class.find('>')? + 1;
                    let after_open = after_class.get(text_start..)?;
                    let text_end = after_open.find("</").unwrap_or(after_open.len().min(200));
                    after_open.get(..text_end).map(|raw| {
                        raw.replace("&amp;", "&")
                            .replace("&lt;", "<")
                            .replace("&gt;", ">")
                            .replace("&quot;", "\"")
                            .replace("&#39;", "'")
                            .trim()
                            .to_string()
                    })
                })
                .unwrap_or_default();

            // SSRF guard: block private IPs and dangerous hostnames on scraped URLs.
            if final_url.starts_with("http") && !is_ssrf_blocked_url(&final_url) {
                results.push(SearchResult {
                    title,
                    url: final_url,
                    snippet,
                });
            }

            pos = abs + 1;
        }

        Ok(results)
    }

    /// Try all providers in order, returning results from the first that succeeds.
    /// Accumulates error messages for the final error if all fail.
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>, String> {
        let mut errors: Vec<String> = Vec::new();

        match self.search_brave(query, limit).await {
            Ok(r) if !r.is_empty() => return Ok(r),
            Ok(_) => errors.push("Brave: no results".to_string()),
            Err(e) => errors.push(format!("Brave: {e}")),
        }
        match self.search_tavily(query, limit).await {
            Ok(r) if !r.is_empty() => return Ok(r),
            Ok(_) => errors.push("Tavily: no results".to_string()),
            Err(e) => errors.push(format!("Tavily: {e}")),
        }
        match self.search_serper(query, limit).await {
            Ok(r) if !r.is_empty() => return Ok(r),
            Ok(_) => errors.push("Serper: no results".to_string()),
            Err(e) => errors.push(format!("Serper: {e}")),
        }
        match self.search_ddg(query, limit).await {
            Ok(r) if !r.is_empty() => return Ok(r),
            Ok(_) => errors.push("DDG: no results".to_string()),
            Err(e) => errors.push(format!("DDG: {e}")),
        }

        Err(format!(
            "All search providers failed: {}. \
             Set BRAVE_API_KEY, TAVILY_API_KEY, or SERPER_API_KEY for better results.",
            errors.join("; ")
        ))
    }
}

#[async_trait]
impl AgentTool for WebSearchTool {
    fn name(&self) -> &str {
        "web-search"
    }

    async fn execute(
        &self,
        payload: Value,
        _context: ToolExecutionContext,
    ) -> Result<Value, AgentOSError> {
        let query = payload["query"]
            .as_str()
            .ok_or_else(|| AgentOSError::ToolExecutionFailed {
                tool_name: "web-search".into(),
                reason: "Missing required field: query".into(),
            })?;

        let limit = payload["limit"].as_u64().unwrap_or(5).clamp(1, 20) as usize;

        let results = self.search(query, limit).await.map_err(|reason| {
            AgentOSError::ToolExecutionFailed {
                tool_name: "web-search".into(),
                reason,
            }
        })?;

        let json_results: Vec<Value> = results
            .iter()
            .map(|r| json!({ "title": r.title, "url": r.url, "snippet": r.snippet }))
            .collect();

        Ok(json!({
            "query": query,
            "results": json_results,
            "count": json_results.len(),
        }))
    }

    fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
        vec![("network.outbound".to_string(), PermissionOp::Execute)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_web_search_tool_name() {
        let tool = WebSearchTool::new();
        assert_eq!(tool.name(), "web-search");
    }

    #[test]
    fn test_ssrf_blocked_localhost() {
        assert!(is_ssrf_blocked_url("http://localhost/admin"));
        assert!(is_ssrf_blocked_url("http://localhost:8080/api"));
        assert!(is_ssrf_blocked_url("http://sub.localhost/data"));
        assert!(is_ssrf_blocked_url("http://internal.local/"));
        assert!(is_ssrf_blocked_url("http://service.internal/"));
    }

    #[test]
    fn test_ssrf_blocked_private_ip() {
        assert!(is_ssrf_blocked_url("http://192.168.1.1/"));
        assert!(is_ssrf_blocked_url("http://10.0.0.1/secret"));
        assert!(is_ssrf_blocked_url("http://127.0.0.1/"));
        assert!(is_ssrf_blocked_url(
            "http://169.254.169.254/latest/meta-data/"
        ));
    }

    #[test]
    fn test_ssrf_allowed_public_urls() {
        assert!(!is_ssrf_blocked_url("https://example.com/page"));
        assert!(!is_ssrf_blocked_url("https://api.search.brave.com/results"));
    }

    #[test]
    fn test_ssrf_blocks_unparseable_url() {
        assert!(is_ssrf_blocked_url("not-a-url"));
        assert!(is_ssrf_blocked_url(""));
    }

    #[tokio::test]
    async fn test_web_search_missing_query_returns_error() {
        use crate::traits::ToolExecutionContext;
        use agentos_types::{AgentID, TaskID, TraceID};
        let tool = WebSearchTool::new();
        let ctx = ToolExecutionContext {
            data_dir: std::path::PathBuf::from("/tmp"),
            task_id: TaskID::new(),
            agent_id: AgentID::new(),
            trace_id: TraceID::new(),
            permissions: Default::default(),
            vault: None,
            hal: None,
            file_lock_registry: None,
            agent_registry: None,
            task_registry: None,
            escalation_query: None,
            workspace_paths: vec![],
            capability_registry: None,
            capability_dispatcher: None,
            storage_zone_query: None,
            cancellation_token: tokio_util::sync::CancellationToken::new(),
        };
        let result = tool.execute(serde_json::json!({}), ctx).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("query"), "error should mention 'query'");
    }

    #[test]
    fn test_search_result_keys_use_zeroizing_secrets() {
        // Verify API keys use Zeroizing<String> — this is a compile-time check
        // that would fail if we regressed to plain String.
        let tool = WebSearchTool::new();
        // Keys are Option<Zeroizing<String>> — deref to Option<&String> then to &str
        let _brave: Option<&str> = tool.brave_key.as_deref().map(|k| k.as_str());
        let _tavily: Option<&str> = tool.tavily_key.as_deref().map(|k| k.as_str());
        let _serper: Option<&str> = tool.serper_key.as_deref().map(|k| k.as_str());
    }
}
