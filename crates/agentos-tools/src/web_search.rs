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
use std::collections::HashMap;
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

/// A single search result from any provider.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// A cached result set, with the instant it was stored.
struct CacheEntry {
    stored_at: Instant,
    results: Vec<SearchResult>,
}

/// How long a cached result set stays usable. Short enough that an agent
/// tracking a developing situation still sees movement; long enough to absorb
/// the repeat queries an autonomous loop issues within one line of reasoning.
const CACHE_TTL: Duration = Duration::from_secs(900);

/// Hard ceiling on cached queries. An unbounded cache in a kernel that runs for
/// weeks is a slow leak, not a cache.
const CACHE_MAX: usize = 256;

/// Queries longer than this are not cached. A model that pastes a document into
/// `query` would otherwise pin a copy of it for the full TTL, once per variant.
const MAX_CACHED_QUERY_LEN: usize = 1024;

/// After every rung fails, don't retry for this long. The cache only spares the
/// network on the *success* path; this is what stops a 10000-iteration
/// autonomous loop from hammering a provider that is already rate-limiting us.
const FAILURE_COOLDOWN: Duration = Duration::from_secs(60);

pub struct WebSearchTool {
    client: Client,
    /// Optional Brave Search API key (env: BRAVE_API_KEY).
    brave_key: Option<Zeroizing<String>>,
    /// Optional Tavily API key (env: TAVILY_API_KEY).
    tavily_key: Option<Zeroizing<String>>,
    /// Optional Serper API key (env: SERPER_API_KEY).
    serper_key: Option<Zeroizing<String>>,
    /// (query, limit) -> results. TTL-expired and size-capped.
    ///
    /// `std::sync::Mutex`, not tokio's: the guard is taken, used and dropped
    /// without crossing an await. Never hold it across a provider call.
    cache: std::sync::Mutex<HashMap<(String, usize), CacheEntry>>,
    /// When every rung last failed, and with what message. Drives `FAILURE_COOLDOWN`.
    last_total_failure: std::sync::Mutex<Option<(Instant, String)>>,
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
            cache: std::sync::Mutex::new(HashMap::new()),
            last_total_failure: std::sync::Mutex::new(None),
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

    /// Return a cached result set for this query, if one is present and fresh.
    /// Expired entries are swept on every lookup — with `CACHE_MAX` at 256 the
    /// scan is cheaper than tracking expiry separately.
    fn cache_get(&self, key: &(String, usize)) -> Option<Vec<SearchResult>> {
        // A cache has no invariant to protect, so recover from poisoning rather
        // than going silently no-op for the rest of the kernel's lifetime —
        // matches `runner.rs` and `file_lock.rs`.
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.retain(|_, e| e.stored_at.elapsed() < CACHE_TTL);
        cache.get(key).map(|e| e.results.clone())
    }

    /// Store a result set. Only non-empty results are cached — a provider
    /// outage must not be remembered as "no results" for the next 15 minutes.
    fn cache_put(&self, key: (String, usize), results: &[SearchResult]) {
        if results.is_empty() {
            return;
        }
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        // ponytail: O(n) oldest-entry eviction, n <= CACHE_MAX. Swap for an LRU
        // crate only if CACHE_MAX ever needs to be in the thousands.
        // `contains_key` first: refreshing an existing hot query overwrites in
        // place, so evicting for it would cost an unrelated entry for nothing.
        if cache.len() >= CACHE_MAX && !cache.contains_key(&key) {
            if let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, e)| e.stored_at)
                .map(|(k, _)| k.clone())
            {
                cache.remove(&oldest);
            }
        }
        cache.insert(
            key,
            CacheEntry {
                stored_at: Instant::now(),
                results: results.to_vec(),
            },
        );
    }

    /// Cache key for a query, or `None` when the query is too large to be worth
    /// pinning. Normalized: providers ignore case and surrounding whitespace, and
    /// a model re-issuing "the same" query across iterations drifts in both.
    fn cache_key(query: &str, limit: usize) -> Option<(String, usize)> {
        (query.len() <= MAX_CACHED_QUERY_LEN).then(|| (query.trim().to_lowercase(), limit))
    }

    /// The error to return immediately if every rung failed within the cooldown.
    fn failure_cooldown_active(&self) -> Option<String> {
        let guard = self
            .last_total_failure
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (at, msg) = guard.as_ref()?;
        (at.elapsed() < FAILURE_COOLDOWN)
            .then(|| format!("{msg} (not retried: every provider failed within the last 60s)"))
    }

    fn record_total_failure(&self, msg: &str) {
        let mut guard = self
            .last_total_failure
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *guard = Some((Instant::now(), msg.to_string()));
    }

    /// Try the cache, then the provider chain. Cache hits never touch the network.
    ///
    /// Autonomous runs re-issue identical queries; without this, every repeat is
    /// a live call against a quota or a scrape that can get the host IP banned.
    /// Returns the results and whether they came from the cache, so the caller
    /// can tell the agent — an agent polling a developing situation must be able
    /// to see that identical results are a cache hit, not an unchanged world.
    async fn search(&self, query: &str, limit: usize) -> Result<(Vec<SearchResult>, bool), String> {
        let key = Self::cache_key(query, limit);
        if let Some(hit) = key.as_ref().and_then(|k| self.cache_get(k)) {
            tracing::debug!(query, limit, count = hit.len(), "web-search: cache hit");
            return Ok((hit, true));
        }
        if let Some(cooled) = self.failure_cooldown_active() {
            tracing::warn!(query, "web-search: in failure cooldown, not retrying");
            return Err(cooled);
        }
        let results = match self.search_uncached(query, limit).await {
            Ok(r) => r,
            Err(e) => {
                self.record_total_failure(&e);
                return Err(e);
            }
        };
        if let Some(k) = key {
            self.cache_put(k, &results);
        }
        Ok((results, false))
    }

    /// Try all providers in order, returning results from the first that succeeds.
    /// Accumulates error messages for the final error if all fail.
    async fn search_uncached(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchResult>, String> {
        let mut errors: Vec<String> = Vec::new();

        match self.search_brave(query, limit).await {
            Ok(r) if !r.is_empty() => {
                // `query` deliberately not logged at info: it reaches the kernel
                // log, a different retention surface from the provider call.
                tracing::info!(rung = "brave", count = r.len(), "web-search answered");
                return Ok(r);
            }
            Ok(_) => errors.push("Brave: no results".to_string()),
            Err(e) => errors.push(format!("Brave: {e}")),
        }
        match self.search_tavily(query, limit).await {
            Ok(r) if !r.is_empty() => {
                // `query` deliberately not logged at info: it reaches the kernel
                // log, a different retention surface from the provider call.
                tracing::info!(rung = "tavily", count = r.len(), "web-search answered");
                return Ok(r);
            }
            Ok(_) => errors.push("Tavily: no results".to_string()),
            Err(e) => errors.push(format!("Tavily: {e}")),
        }
        match self.search_serper(query, limit).await {
            Ok(r) if !r.is_empty() => {
                // `query` deliberately not logged at info: it reaches the kernel
                // log, a different retention surface from the provider call.
                tracing::info!(rung = "serper", count = r.len(), "web-search answered");
                return Ok(r);
            }
            Ok(_) => errors.push("Serper: no results".to_string()),
            Err(e) => errors.push(format!("Serper: {e}")),
        }
        match self.search_ddg(query, limit).await {
            Ok(r) if !r.is_empty() => {
                // `query` deliberately not logged at info: it reaches the kernel
                // log, a different retention surface from the provider call.
                tracing::info!(rung = "ddg", count = r.len(), "web-search answered");
                return Ok(r);
            }
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

        let (results, cached) = self.search(query, limit).await.map_err(|reason| {
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
            // Identical results on a repeat poll mean "cached", not "unchanged".
            "cached": cached,
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
        let tool = WebSearchTool::new();
        let result = tool.execute(serde_json::json!({}), test_ctx()).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("query"), "error should mention 'query'");
    }

    fn test_ctx() -> crate::traits::ToolExecutionContext {
        use crate::traits::ToolExecutionContext;
        use agentos_types::{AgentID, TaskID, TraceID};
        ToolExecutionContext {
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
            workspace_paths_writable: vec![],
            workspace_paths_executable: vec![],
            capability_registry: None,
            capability_dispatcher: None,
            storage_zone_query: None,
            cancellation_token: tokio_util::sync::CancellationToken::new(),
            tool_categories: None,
            shared_dir: None,
        }
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

    fn one_result(url: &str) -> Vec<SearchResult> {
        vec![SearchResult {
            title: "t".into(),
            url: url.into(),
            snippet: String::new(),
        }]
    }

    #[test]
    fn cache_put_then_get_returns_results() {
        let tool = WebSearchTool::new();
        let key = ("rust".to_string(), 5);
        tool.cache_put(key.clone(), &one_result("https://rust-lang.org"));
        let hit = tool.cache_get(&key).expect("expected a cache hit");
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].url, "https://rust-lang.org");
    }

    #[test]
    fn cache_does_not_store_empty_results() {
        // A provider outage must not be cached as "no results".
        let tool = WebSearchTool::new();
        let key = ("rust".to_string(), 5);
        tool.cache_put(key.clone(), &[]);
        assert!(tool.cache_get(&key).is_none());
    }

    #[test]
    fn cache_distinguishes_limit() {
        // Same query at a different limit is a different key — a limit-3 hit
        // must not satisfy a limit-10 request with only three results.
        let tool = WebSearchTool::new();
        tool.cache_put(("rust".to_string(), 3), &one_result("https://example.com"));
        assert!(tool.cache_get(&("rust".to_string(), 10)).is_none());
    }

    #[test]
    fn cache_evicts_oldest_at_capacity() {
        let tool = WebSearchTool::new();
        let overflow = CACHE_MAX + 10;
        for i in 0..overflow {
            tool.cache_put((format!("q{i}"), 5), &one_result("https://example.com"));
        }
        // Evict-one-then-insert on distinct keys settles at exactly the cap.
        // `assert!(<=)` would also pass for a `clear()`, or for evicting the
        // *newest* entry — a one-character min/max slip that guts the cache.
        let len = tool.cache.lock().expect("cache lock").len();
        assert_eq!(len, CACHE_MAX, "cache should settle at exactly the cap");
        assert!(
            tool.cache_get(&("q0".to_string(), 5)).is_none(),
            "oldest entry should have been evicted"
        );
        assert!(
            tool.cache_get(&(format!("q{}", overflow - 1), 5)).is_some(),
            "newest entry should have survived"
        );
    }

    #[test]
    fn cache_get_drops_expired_entries() {
        let tool = WebSearchTool::new();
        let key = ("rust".to_string(), 5);
        let stale = Instant::now()
            .checked_sub(CACHE_TTL + Duration::from_secs(1))
            .expect("clock far enough from the epoch");
        tool.cache.lock().expect("cache lock").insert(
            key.clone(),
            CacheEntry {
                stored_at: stale,
                results: one_result("https://stale.example"),
            },
        );
        assert!(tool.cache_get(&key).is_none(), "expired entry must not hit");
    }

    #[test]
    fn cache_key_normalizes_case_and_whitespace() {
        // A model re-issuing "the same" query across iterations drifts in both.
        assert_eq!(
            WebSearchTool::cache_key("  Rust Tokio ", 5),
            WebSearchTool::cache_key("rust tokio", 5)
        );
        // Oversized queries are not cached at all.
        let huge = "x".repeat(MAX_CACHED_QUERY_LEN + 1);
        assert!(WebSearchTool::cache_key(&huge, 5).is_none());
    }

    #[tokio::test]
    async fn execute_serves_from_cache_without_touching_the_network() {
        // The invariant the whole phase turns on: execute() must go through the
        // caching wrapper. A hit returns before any provider call, so this is
        // deterministic whether or not API keys are set in the environment.
        let tool = WebSearchTool::new();
        tool.cache_put(
            ("rust".to_string(), 5),
            &one_result("https://cached.example"),
        );
        let out = tool
            .execute(serde_json::json!({"query": "Rust", "limit": 5}), test_ctx())
            .await
            .expect("cache hit should succeed offline");
        assert_eq!(out["results"][0]["url"], "https://cached.example");
        assert_eq!(out["cached"], true, "a hit must be reported to the agent");
    }

    #[test]
    fn failure_cooldown_suppresses_retries() {
        // Without this, a rate-limited provider gets hammered once per iteration
        // for the whole 10000-iteration autonomous budget.
        let tool = WebSearchTool::new();
        assert!(tool.failure_cooldown_active().is_none());
        tool.record_total_failure("All search providers failed: DDG: 429");
        let cooled = tool
            .failure_cooldown_active()
            .expect("cooldown should be active right after a total failure");
        assert!(cooled.contains("429"), "original error must be preserved");
        assert!(
            cooled.contains("not retried"),
            "should say it was suppressed"
        );
    }
}
