/// A2A outbound client — discover and delegate tasks to external A2A agents.
use super::agent_card::AgentCard;
use super::task::{A2ATask, SubmitTaskRequest};

/// Client for interacting with a remote A2A-compliant agent.
pub struct A2AClient {
    http: reqwest::Client,
    base_url: String,
    token: Option<String>,
}

/// Redirects are never followed: a public peer answering `302` to
/// `169.254.169.254` or loopback would otherwise walk past the caller's SSRF
/// check, which only sees the first URL.
fn http_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(30))
}

impl A2AClient {
    pub fn new(base_url: &str) -> Self {
        Self::with_http(base_url, http_builder())
    }

    /// Like [`Self::new`], but connects `host` only to `addrs`: the addresses
    /// the caller already resolved and checked. Without pinning, reqwest
    /// resolves again and a short-TTL name can answer public for the check and
    /// private for the request (DNS rebinding).
    pub fn pinned(base_url: &str, host: &str, addrs: &[std::net::SocketAddr]) -> Self {
        Self::with_http(base_url, http_builder().resolve_to_addrs(host, addrs))
    }

    fn with_http(base_url: &str, builder: reqwest::ClientBuilder) -> Self {
        Self {
            // Building only fails if the TLS backend cannot initialise, which
            // `Client::new()` (used here before) turned into a panic as well.
            http: builder.build().expect("A2A HTTP client"),
            base_url: base_url.trim_end_matches('/').to_string(),
            token: None,
        }
    }

    pub fn with_token(mut self, token: &str) -> Self {
        self.token = Some(token.to_string());
        self
    }

    /// Discover the remote agent's identity and capabilities.
    pub async fn discover(&self) -> anyhow::Result<AgentCard> {
        let url = format!("{}/.well-known/agent.json", self.base_url);
        let resp = self.http.get(&url).send().await?.error_for_status()?;
        Ok(resp.json::<AgentCard>().await?)
    }

    /// Submit a task delegation to the remote agent.
    /// Returns the task ID immediately (async execution on remote side).
    pub async fn submit_task(
        &self,
        capability: &str,
        input: serde_json::Value,
        sender_url: &str,
    ) -> anyhow::Result<String> {
        let url = format!("{}/a2a/tasks", self.base_url);
        let req = SubmitTaskRequest {
            sender: sender_url.to_string(),
            capability: capability.to_string(),
            input,
        };
        let mut builder = self.http.post(&url).json(&req);
        if let Some(ref t) = self.token {
            builder = builder.header("Authorization", format!("Bearer {}", t));
        }
        let resp = builder.send().await?.error_for_status()?;
        let json: serde_json::Value = resp.json().await?;
        json["id"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("Response missing 'id' field"))
    }

    /// Poll the status of a previously submitted task.
    pub async fn poll_task(&self, task_id: &str) -> anyhow::Result<A2ATask> {
        let url = format!("{}/a2a/tasks/{}", self.base_url, task_id);
        let mut builder = self.http.get(&url);
        if let Some(ref t) = self.token {
            builder = builder.header("Authorization", format!("Bearer {}", t));
        }
        let resp = builder.send().await?.error_for_status()?;
        Ok(resp.json::<A2ATask>().await?)
    }

    /// Cancel a running task.
    pub async fn cancel_task(&self, task_id: &str) -> anyhow::Result<()> {
        let url = format!("{}/a2a/tasks/{}/cancel", self.base_url, task_id);
        let mut builder = self.http.post(&url).json(&serde_json::json!({}));
        if let Some(ref t) = self.token {
            builder = builder.header("Authorization", format!("Bearer {}", t));
        }
        builder.send().await?.error_for_status()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// One-response HTTP server; counts requests it sees.
    async fn serve(response: String) -> (std::net::SocketAddr, Arc<AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let _ = sock.write_all(response.as_bytes()).await;
            }
        });
        (addr, hits)
    }

    fn ok_card() -> String {
        let body =
            r#"{"name":"x","description":"","url":"http://x","version":"1","capabilities":[]}"#;
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    #[tokio::test]
    async fn redirects_are_not_followed() {
        let (target, target_hits) = serve(ok_card()).await;
        let (peer, _) = serve(format!(
            "HTTP/1.1 302 Found\r\nlocation: http://{target}/.well-known/agent.json\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
        ))
        .await;
        let client = A2AClient::new(&format!("http://{peer}"));
        assert!(client.discover().await.is_err());
        assert_eq!(
            target_hits.load(Ordering::SeqCst),
            0,
            "redirect was followed"
        );
    }

    #[tokio::test]
    async fn pinned_client_connects_only_to_checked_addrs() {
        let (server, hits) = serve(ok_card()).await;
        // `.invalid` never resolves, so reaching the server proves the pin
        // replaced DNS for this host.
        let client = A2AClient::pinned(
            &format!("http://peer.invalid:{}", server.port()),
            "peer.invalid",
            &[server],
        );
        let _ = client.discover().await;
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }
}
