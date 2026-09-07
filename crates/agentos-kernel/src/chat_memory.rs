//! Chat-path memory parity helpers.
//!
//! The task executor writes episodic rows, injects the agent's context memory
//! and fires `TaskStart`/`TaskEnd` hooks. The chat path (`chat_infer_with_tools`
//! and `chat_infer_streaming`) historically did none of that, so chat turns
//! were invisible to `memory-search`, the background review hook and the
//! consolidation engine, and the context memory an agent curated never showed
//! up in its own chat prompt. These helpers give both chat paths the same
//! behaviour with one call at each lifecycle point.

use crate::kernel::Kernel;
use agentos_memory::{EpisodeRecordInput, EpisodeType};
use agentos_types::{AgentID, HookEvent, HookResult, TaskID, TraceID};

/// Prompt shown above the agent's curated context memory document.
pub(crate) const CONTEXT_MEMORY_HEADER: &str = "Your self-curated context memory. Update via context-memory-update tool. Write compressed: key:value pairs, short phrases, no prose. Every token here costs context budget.";

/// Bootstrapping hint injected when the agent has no context memory yet.
pub(crate) const CONTEXT_MEMORY_EMPTY_HINT: &str = "<agent-context-memory>\nEmpty context memory. Use context-memory-update to save reusable knowledge for future tasks. Write compressed: key:value, short phrases, no prose. Budget is limited — every token counts.\n</agent-context-memory>";

impl Kernel {
    /// The `<agent-context-memory>` knowledge block for `agent_id`, or `None`
    /// when context memory is disabled or the store read fails. Shared by the
    /// task executor and both chat paths so the wording stays identical.
    ///
    /// `pub` only so `tests/e2e/` can assert on it (same rationale as
    /// `build_chat_tool_manifests`); treat as semver-unstable internal API.
    pub async fn context_memory_block(&self, agent_id: &AgentID) -> Option<String> {
        if !self.config.memory.context.enabled {
            return None;
        }
        match self
            .context_memory_store
            .read_content(&agent_id.to_string())
            .await
        {
            Ok(Some(content)) => Some(format!(
                "<agent-context-memory>\n{}\n\n{}\n</agent-context-memory>",
                CONTEXT_MEMORY_HEADER, content
            )),
            Ok(None) => Some(CONTEXT_MEMORY_EMPTY_HINT.to_string()),
            Err(e) => {
                tracing::warn!(
                    agent_id = %agent_id,
                    error = %e,
                    "Failed to read agent context memory, skipping injection"
                );
                None
            }
        }
    }

    /// Start of a chat turn: fire `TaskStart` (a `Pre` hook — `Abort` is
    /// honoured) and record the user prompt to episodic memory.
    pub(crate) async fn chat_turn_begin(
        &self,
        agent_id: AgentID,
        task_id: TaskID,
        trace_id: TraceID,
        user_message: &str,
        session_id: Option<&str>,
    ) -> Result<(), String> {
        if let HookResult::Abort(reason) = self
            .hook_registry
            .fire(&HookEvent::TaskStart { task_id, agent_id })
            .await
        {
            return Err(format!("Chat turn aborted by hook: {reason}"));
        }
        self.record_chat_episode(EpisodeRecordInput {
            task_id: &task_id,
            agent_id: &agent_id,
            entry_type: EpisodeType::UserPrompt,
            content: user_message,
            summary: Some("User prompt received (chat)"),
            metadata: Some(serde_json::json!({
                "origin": "chat",
                "session_id": session_id,
            })),
            trace_id: &trace_id,
        })
        .await;
        Ok(())
    }

    /// Record one executed (non-dedup-replayed) chat tool call and its result.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn chat_record_tool(
        &self,
        agent_id: AgentID,
        task_id: TaskID,
        trace_id: TraceID,
        tool_name: &str,
        intent_type: &str,
        payload: &serde_json::Value,
        result: &serde_json::Value,
        success: bool,
        duration_ms: u64,
        iteration: u32,
    ) {
        self.record_chat_episode(EpisodeRecordInput {
            task_id: &task_id,
            agent_id: &agent_id,
            entry_type: EpisodeType::ToolCall,
            content: &format!("Tool: {} Payload: {}", tool_name, payload),
            summary: Some(&format!("Called tool: {} ({})", tool_name, intent_type)),
            metadata: Some(serde_json::json!({
                "tool": tool_name,
                "intent_type": intent_type,
                "iteration": iteration,
                "origin": "chat",
            })),
            trace_id: &trace_id,
        })
        .await;
        let result_text = serde_json::to_string(result).unwrap_or_default();
        let result_text = truncate_chars(&result_text, 2000);
        self.record_chat_episode(EpisodeRecordInput {
            task_id: &task_id,
            agent_id: &agent_id,
            entry_type: EpisodeType::ToolResult,
            content: &result_text,
            summary: Some(&format!(
                "Tool {} {} ({}ms)",
                tool_name,
                if success { "succeeded" } else { "failed" },
                duration_ms
            )),
            metadata: Some(serde_json::json!({
                "tool": tool_name,
                "success": success,
                "duration_ms": duration_ms,
                "iteration": iteration,
                "origin": "chat",
            })),
            trace_id: &trace_id,
        })
        .await;
    }

    /// End of a chat turn that produced an answer: record it + a `SystemEvent`
    /// summary (the row the consolidation engine and background review key
    /// off), fire `TaskEnd`, and tick the consolidation counter.
    ///
    /// `success` is false for a *degraded* turn — one that hit an iteration cap
    /// or a stuck-loop circuit breaker. Those still return text to the user,
    /// but they must not be labelled a success: `find_successful_episodes` and
    /// the background review both select on that flag, and a stuck loop is
    /// exactly the shape ("lots of tool calls") the review looks for. Learning
    /// a procedure from a failure mode is worse than learning nothing.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn chat_turn_end(
        &self,
        agent_id: AgentID,
        task_id: TaskID,
        trace_id: TraceID,
        user_message: &str,
        answer: &str,
        success: bool,
        tool_calls: usize,
        iterations: u32,
        duration_ms: u64,
    ) {
        self.record_chat_episode(EpisodeRecordInput {
            task_id: &task_id,
            agent_id: &agent_id,
            entry_type: EpisodeType::LLMResponse,
            content: answer,
            summary: Some("Chat answer"),
            metadata: Some(serde_json::json!({ "origin": "chat" })),
            trace_id: &trace_id,
        })
        .await;
        let outcome = if success { "success" } else { "degraded" };
        let summary = format!(
            "task:{}\nresult:{}|tools:{}|iters:{}|{}ms\nanswer:{}",
            truncate_chars(user_message, 200),
            outcome,
            tool_calls,
            iterations,
            duration_ms,
            truncate_chars(answer, 500)
        );
        self.record_chat_episode(EpisodeRecordInput {
            task_id: &task_id,
            agent_id: &agent_id,
            entry_type: EpisodeType::SystemEvent,
            content: &summary,
            summary: Some(if success {
                "Chat turn completed successfully"
            } else {
                "Chat turn completed but was degraded (loop guard tripped)"
            }),
            metadata: Some(serde_json::json!({
                "outcome": outcome,
                "duration_ms": duration_ms,
                "tool_calls": tool_calls,
                "iterations": iterations,
                "origin": "chat",
            })),
            trace_id: &trace_id,
        })
        .await;
        self.hook_registry
            .fire(&HookEvent::TaskEnd {
                task_id,
                agent_id,
                success,
            })
            .await;
        // Detached: on the Nth completion this runs a full consolidation cycle
        // (up to `max_episodes_per_cycle` episodes plus embedding searches).
        // Awaiting it here would stall the caller that persists the reply.
        let engine = std::sync::Arc::clone(&self.consolidation_engine);
        tokio::spawn(async move {
            engine.on_task_completed().await;
        });
    }

    /// A chat turn that ended in an adapter/stream error. Records the failure
    /// and fires `TaskEnd { success: false }` so every `TaskStart` this module
    /// fires has a matching end — an unpaired pair would leave the audit log
    /// (and any future stateful hook) with a turn that never closed.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn chat_turn_failed(
        &self,
        agent_id: AgentID,
        task_id: TaskID,
        trace_id: TraceID,
        user_message: &str,
        error: &str,
        tool_calls: usize,
        iterations: u32,
        duration_ms: u64,
    ) {
        let summary = format!(
            "task:{}\nresult:failure|tools:{}|iters:{}|{}ms\nerror:{}",
            truncate_chars(user_message, 200),
            tool_calls,
            iterations,
            duration_ms,
            truncate_chars(error, 500)
        );
        self.record_chat_episode(EpisodeRecordInput {
            task_id: &task_id,
            agent_id: &agent_id,
            entry_type: EpisodeType::SystemEvent,
            content: &summary,
            summary: Some("Chat turn failed"),
            metadata: Some(serde_json::json!({
                "outcome": "failure",
                "error": truncate_chars(error, 300),
                "duration_ms": duration_ms,
                "tool_calls": tool_calls,
                "iterations": iterations,
                "origin": "chat",
            })),
            trace_id: &trace_id,
        })
        .await;
        self.hook_registry
            .fire(&HookEvent::TaskEnd {
                task_id,
                agent_id,
                success: false,
            })
            .await;
    }

    async fn record_chat_episode(&self, input: EpisodeRecordInput<'_>) {
        let task_id = *input.task_id;
        if let Err(e) = self.episodic_memory.record(input).await {
            tracing::warn!(task_id = %task_id, error = %e, "Failed to record chat episodic memory");
        }
    }
}

/// Truncate to at most `max` chars on a char boundary, appending `…` when cut.
pub(crate) fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// True when the `n`-th user turn of a session (1-based) should carry the
/// memory nudge. `every == 0` disables nudging.
pub(crate) fn should_nudge(user_turns: usize, every: u32) -> bool {
    every > 0 && user_turns > 0 && user_turns.is_multiple_of(every as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_keeps_short_strings() {
        assert_eq!(truncate_chars("abc", 5), "abc");
        assert_eq!(truncate_chars("abcdef", 3), "abc…");
    }

    #[test]
    fn nudge_cadence() {
        assert!(should_nudge(10, 10));
        assert!(should_nudge(20, 10));
        assert!(!should_nudge(5, 10));
        assert!(!should_nudge(10, 0));
        assert!(!should_nudge(0, 10));
    }
}
