/// A2A outbound client — discover and delegate tasks to external A2A agents.
use super::agent_card::AgentCard;
use super::task::{A2ATask, SubmitTaskRequest};

/// Client for interacting with a remote A2A-compliant agent.
pub struct A2AClient {
    http: reqwest::Client,
    base_url: String,
    token: Option<String>,
}

impl A2AClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            http: reqwest::Client::new(),
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
