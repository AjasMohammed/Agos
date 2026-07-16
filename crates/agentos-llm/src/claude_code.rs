//! Claude Code (subprocess) LLM adapter.
//!
//! Runs the locally-installed `claude` CLI in headless print mode
//! (`claude -p ... --output-format json`) as a pure text reasoning core, so an
//! AgentOS agent can run on a Claude Code / Claude.ai subscription with **no
//! Anthropic API key**.
//!
//! Built-in Claude Code tools are denied via `--disallowed-tools` (an empty
//! `--allowed-tools` does NOT restrict in the default permission mode), so the
//! model cannot touch the host outside AgentOS.
//!
//! Tool calling has two modes:
//! - **MCP mode** (when [`Self::with_mcp_config`] is set): the subprocess gets a
//!   `--mcp-config` pointing at a kernel-hosted MCP server exposing the 4 AgentOS
//!   meta-tools (`mcp__agentos__{search,describe,list,invoke}_tool`). Claude calls
//!   them as native `tool_use` *inside* the subprocess; the kernel gateway runs
//!   each through `ToolRunner` (full capability/audit/sandbox) and returns the
//!   result, so the subprocess loops internally and returns a final answer.
//!   `supports_native_tool_calling()` returns `true`, so the kernel omits the
//!   `## Tools` JSON-envelope block and sees one self-contained turn (no
//!   per-iteration `InferenceResult.tool_calls`).
//! - **Fallback / envelope mode** (no MCP config): `supports_native_tool_calling()`
//!   returns `false`; the kernel injects the `## Tools` JSON instructions and
//!   recovers tool calls from `InferenceResult.text` (same path as Ollama).
//!
//! Streaming is supported via `--output-format stream-json` (token-level
//! `InferenceEvent`s). Image input is supported by writing images to a temp dir
//! and enabling the `Read` tool scoped to that dir — the only way the CLI
//! accepts images (inline base64 is ignored). When images are present, `Read`
//! is therefore enabled for that one call (scoped via `--add-dir` + cwd).
//!
//! # Cost
//! Inference consumes the user's **Claude Code subscription quota / rate
//! limits**, not metered API credits. A subprocess is spawned per inference
//! step, so latency is higher than a direct HTTP API call.

use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine;
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::media::{ImageResolver, NoopImageResolver};
use crate::session::SessionState;
use crate::traits::LLMCore;
use crate::types::{
    HealthStatus, InferenceCost, InferenceEvent, InferenceResult, ModelCapabilities, StopReason,
    TokenUsage,
};
use agentos_types::{
    AgentOSError, ContentPart, ContextEntry, ContextRole, ContextWindow, ToolManifest,
};

/// Default model when none (or "default") is specified. Matches the `claude`
/// CLI's own default tier.
const DEFAULT_MODEL: &str = "claude-sonnet-4-6";
/// Per-inference subprocess timeout. Generous because the CLI does its own
/// startup + context-cache work before responding.
const DEFAULT_TIMEOUT_SECS: u64 = 300;
/// Effective context window we advertise. Claude's real window is 200k, but
/// AgentOS sizes its per-task context budget (and thus how much memory/retrieved
/// context it injects) to this number — and the *entire* prompt is re-sent to a
/// fresh subprocess on every turn. Reporting 200k makes AgentOS flood ~160k
/// tokens per call (40–80s latency, channel timeouts). We cap the effective
/// window to keep prompts lean; override with `with_context_window` for tasks
/// that genuinely need more. Compaction/overflow track this value too.
const DEFAULT_CONTEXT_WINDOW: u64 = 64_000;
const DEFAULT_MAX_OUTPUT_TOKENS: u64 = 32_000;

/// Every built-in Claude Code tool. We deny all of these so the subprocess
/// cannot act outside AgentOS (no host file/shell/network access via Claude's
/// own tools). NOTE: `--allowed-tools` is *additive* and does not restrict in
/// the default permission mode — `--disallowed-tools` is the only flag that
/// actually blocks. Keep this list current with `claude`'s built-in toolset.
const CLAUDE_BUILTIN_TOOLS: &[&str] = &[
    "Task",
    "AskUserQuestion",
    "Bash",
    "CronCreate",
    "CronDelete",
    "CronList",
    "Edit",
    "EnterPlanMode",
    "EnterWorktree",
    "ExitPlanMode",
    "ExitWorktree",
    "Monitor",
    "NotebookEdit",
    "PushNotification",
    "Read",
    "RemoteTrigger",
    "ScheduleWakeup",
    "Skill",
    "TaskOutput",
    "TaskStop",
    "TodoWrite",
    "ToolSearch",
    "WebFetch",
    "WebSearch",
    "Workflow",
    "Write",
];

/// LLM adapter that delegates inference to the local `claude` CLI.
pub struct ClaudeCodeCore {
    /// Binary to invoke (default `"claude"`; overridable for testing / custom paths).
    binary: String,
    /// Model passed via `--model`.
    model: String,
    /// Per-call subprocess timeout.
    timeout: Duration,
    capabilities: ModelCapabilities,
    /// Resolves `ImageSource` (incl. web-upload `FileRef`) to base64 for vision.
    image_resolver: Arc<dyn ImageResolver>,
    /// When set, attached to the subprocess via `--mcp-config` so it exposes the
    /// 4 AgentOS meta-tools as native MCP tools (see [`Self::with_mcp_config`]).
    mcp_config_path: Option<std::path::PathBuf>,
    /// Opt-in session resume. When set, the adapter keys a CLI session by the
    /// `ContextWindow` id and uses `--resume` to send only the delta turn instead
    /// of replaying the full context. `None` (default) ⇒ stateless, byte-identical
    /// to the non-resume path. The session is a cache only (see [`crate::session`]).
    resume_store: Option<Arc<dyn crate::session::ClaudeSessionLookup>>,
}

/// Everything needed to spawn one CLI turn, resolved from the context + any
/// stored resume session.
struct Invocation {
    system: String,
    prompt: String,
    image_dir: Option<tempfile::TempDir>,
    /// Number of active entries the CLI session will hold after this turn —
    /// recorded as the next turn's high-water mark.
    entry_count: usize,
    /// Fingerprint of `(system + full current prefix)`, recorded after the turn.
    fingerprint: u64,
    /// `Some(session_id)` ⇒ resume that session and send only the delta turn;
    /// `None` ⇒ fresh full send (the CLI returns a new session id to record).
    resume_session_id: Option<String>,
}

impl ClaudeCodeCore {
    /// Build an adapter for the given model. Empty or `"default"` selects
    /// [`DEFAULT_MODEL`].
    pub fn new(model: impl Into<String>) -> Self {
        let model = model.into();
        let model = if model.is_empty() || model == "default" {
            DEFAULT_MODEL.to_string()
        } else {
            model
        };
        Self {
            binary: "claude".to_string(),
            model,
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            image_resolver: Arc::new(NoopImageResolver),
            mcp_config_path: None,
            resume_store: None,
            capabilities: ModelCapabilities {
                context_window_tokens: DEFAULT_CONTEXT_WINDOW,
                // Images are passed to the CLI as temp files + the Read tool
                // (inline base64 isn't honored by the CLI). See prepare_invocation.
                supports_images: true,
                // We deliberately use AgentOS's JSON-in-markdown tool path, not
                // a native tool API — see module docs.
                supports_tool_calling: false,
                supports_json_mode: false,
                max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
                supports_streaming: true,
                supports_parallel_tools: false,
                supports_prompt_caching: false,
                supports_thinking: false,
                supports_structured_output: false,
            },
        }
    }

    /// Override the binary path/name (e.g. an absolute path to `claude`).
    pub fn with_binary(mut self, binary: impl Into<String>) -> Self {
        self.binary = binary.into();
        self
    }

    /// Override the per-inference subprocess timeout.
    pub fn with_timeout_secs(mut self, secs: u64) -> Self {
        self.timeout = Duration::from_secs(secs);
        self
    }

    /// Inject the image resolver used to turn `ImageSource` into base64 for vision.
    pub fn with_image_resolver(mut self, resolver: Arc<dyn ImageResolver>) -> Self {
        self.image_resolver = resolver;
        self
    }

    /// Enable opt-in session resume (see [`crate::session::ClaudeSessionLookup`]).
    /// When set, the adapter sends only the delta turn with `--resume`; when
    /// unset (default) the adapter is fully stateless.
    pub fn with_resume_store(
        mut self,
        store: Arc<dyn crate::session::ClaudeSessionLookup>,
    ) -> Self {
        self.resume_store = Some(store);
        self
    }

    /// Attach an MCP config file so the subprocess exposes the 4 AgentOS
    /// meta-tools (search/describe/list/invoke) as native tools. When set,
    /// `supports_native_tool_calling()` returns true and the built-in tools stay denied.
    pub fn with_mcp_config(mut self, path: std::path::PathBuf) -> Self {
        self.mcp_config_path = Some(path);
        self
    }

    /// Override the effective context window (see [`DEFAULT_CONTEXT_WINDOW`]).
    /// Larger values let AgentOS inject more context per call but make every
    /// re-sent subprocess prompt heavier.
    pub fn with_context_window(mut self, tokens: u64) -> Self {
        self.capabilities.context_window_tokens = tokens;
        self
    }

    fn err(&self, reason: impl Into<String>) -> AgentOSError {
        AgentOSError::LLMError {
            provider: "claude-code".to_string(),
            reason: reason.into(),
        }
    }

    /// Split a [`ContextWindow`] into `(system_prompt, conversation_text)`.
    ///
    /// System entries are joined into the `--system-prompt` value (this carries
    /// AgentOS's `## Tools` JSON instructions). All other roles are flattened
    /// into the prompt argument with role markers, mirroring
    /// `ollama.rs::context_to_messages`.
    fn flatten_context(context: &ContextWindow) -> (String, String) {
        let mut system = String::new();
        let mut convo = String::new();
        for entry in context.active_entries() {
            let text = entry.text();
            match entry.role {
                ContextRole::System => {
                    if !system.is_empty() {
                        system.push_str("\n\n");
                    }
                    system.push_str(&text);
                }
                ContextRole::User => {
                    convo.push_str("\n\n## User\n");
                    convo.push_str(&text);
                }
                ContextRole::Assistant => {
                    convo.push_str("\n\n## Assistant\n");
                    convo.push_str(&text);
                }
                ContextRole::ToolResult => {
                    convo.push_str("\n\n## Tool Result\n");
                    convo.push_str(&text);
                }
            }
        }
        (system.trim().to_string(), convo.trim().to_string())
    }

    /// Flatten a slice of context entries into a single role-marked prompt
    /// string. Used for the resume **delta** turn: on `--resume` the CLI session
    /// already holds the prior conversation + system prompt, so we send only the
    /// new entries (no separate `--system-prompt`).
    fn flatten_delta(entries: &[&ContextEntry]) -> String {
        let mut convo = String::new();
        for entry in entries {
            let marker = match entry.role {
                ContextRole::System => "## System",
                ContextRole::User => "## User",
                ContextRole::Assistant => "## Assistant",
                ContextRole::ToolResult => "## Tool Result",
            };
            convo.push_str("\n\n");
            convo.push_str(marker);
            convo.push('\n');
            convo.push_str(&entry.text());
        }
        convo.trim().to_string()
    }

    /// Look up an existing resume session for this context window, if resume is
    /// enabled. `None` ⇒ stateless full-context send.
    async fn lookup_resume(&self, context: &ContextWindow) -> Option<SessionState> {
        let store = self.resume_store.as_ref()?;
        // Key on the STABLE resume_key (set by the kernel to the TaskID), not the
        // ephemeral compiled `context.id` which is regenerated every turn.
        let key = context.resume_key.as_deref()?;
        store.lookup(key).await
    }

    /// Stable hash of the system prompt + the given prefix entries (role + text).
    /// Detects when the conversation prefix the CLI session holds has diverged
    /// from the current context (recompilation / compaction / reorder / eviction
    /// / system-prompt change), in which case a delta resume would be unsafe.
    fn prefix_fingerprint(system: &str, entries: &[&ContextEntry]) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        system.hash(&mut h);
        for e in entries {
            std::mem::discriminant(&e.role).hash(&mut h);
            e.text().hash(&mut h);
        }
        h.finish()
    }

    /// Record the (possibly new) CLI session id + high-water mark after a turn,
    /// so the next turn can resume and send only the delta. Best-effort.
    async fn record_resume(
        &self,
        context: &ContextWindow,
        session_id: &str,
        entry_count: usize,
        fingerprint: u64,
    ) {
        if let (Some(store), Some(key)) =
            (self.resume_store.as_ref(), context.resume_key.as_deref())
        {
            store
                .record(key, session_id, entry_count, fingerprint)
                .await;
        }
    }

    /// Parse the `--output-format json` payload from the CLI into an
    /// [`InferenceResult`] plus the CLI `session_id` (for resume). Pure (no I/O)
    /// so it is unit-testable.
    fn parse_cli_json(
        &self,
        stdout: &str,
        fallback_duration_ms: u64,
    ) -> Result<(InferenceResult, Option<String>), AgentOSError> {
        let parsed: ClaudeCliResult = serde_json::from_str(stdout.trim())
            .map_err(|e| self.err(format!("invalid JSON from claude CLI: {e}")))?;

        if parsed.is_error {
            let msg = parsed
                .result
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "claude CLI reported is_error=true".to_string());
            return Err(self.err(format!("claude CLI error: {msg}")));
        }

        let text = parsed
            .result
            .ok_or_else(|| self.err("claude CLI response missing 'result' field"))?;

        let usage = parsed.usage.unwrap_or_default();
        // Count cached + freshly-created prompt tokens as prompt tokens so cost
        // attribution and overflow heuristics see the true input size.
        let prompt_tokens = usage
            .input_tokens
            .saturating_add(usage.cache_read_input_tokens)
            .saturating_add(usage.cache_creation_input_tokens);
        let completion_tokens = usage.output_tokens;

        let stop_reason = match parsed.stop_reason.as_deref() {
            None | Some("end_turn") => StopReason::EndTurn,
            Some("max_tokens") => StopReason::MaxTokens,
            Some("stop_sequence") => StopReason::StopSequence,
            Some("tool_use") => StopReason::ToolUse,
            Some(other) => StopReason::Other(other.to_string()),
        };

        let result = InferenceResult {
            text,
            tokens_used: TokenUsage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens.saturating_add(completion_tokens),
            },
            model: self.model.clone(),
            duration_ms: parsed.duration_ms.unwrap_or(fallback_duration_ms),
            tool_calls: Vec::new(),
            uncertainty: None,
            stop_reason,
            // The CLI reports `total_cost_usd` (the API-EQUIVALENT cost; the
            // agent actually runs on the subscription). We surface it as the
            // inference cost so AgentOS's budget governance (warning / pause /
            // hard-limit in cost_tracker) accounts for and can cap claude-code
            // spend — otherwise its cost is invisible and runaway loops go
            // unbudgeted. Per-direction split isn't provided, so input/output
            // are 0 and the total carries the value.
            cost: parsed.total_cost_usd.map(|total| InferenceCost {
                input_cost_usd: 0.0,
                output_cost_usd: 0.0,
                total_cost_usd: total,
            }),
            cached_tokens: usage.cache_read_input_tokens,
        };
        Ok((result, parsed.session_id))
    }

    /// Build the shared `claude -p` invocation (binary, prompt, model, system
    /// prompt, piped I/O). Callers append the `--output-format` they want.
    ///
    /// With no images, all built-in tools are disabled so Claude is a pure
    /// reasoning core. When `image_dir` is set, the `Read` tool is enabled and
    /// scoped to that dir (via `--add-dir` + working directory) so Claude can
    /// load the attached image files — the only way the CLI accepts images.
    fn base_command(
        &self,
        prompt: &str,
        system: &str,
        image_dir: Option<&std::path::Path>,
        resume_session_id: Option<&str>,
    ) -> Command {
        let mut cmd = Command::new(&self.binary);
        cmd.arg("-p").arg(prompt).arg("--model").arg(&self.model);
        // Opt-in resume: continue the prior CLI session and send only the delta
        // turn (the caller passes the delta as `prompt` and an empty `system`).
        if let Some(sid) = resume_session_id {
            cmd.arg("--resume").arg(sid);
        }
        // Deny every built-in tool so Claude is a pure reasoning core that can
        // only emit AgentOS tool-call JSON — never touching the host outside
        // AgentOS's capability/audit/sandbox layer. `--allowed-tools` is additive
        // and does NOT restrict in the default permission mode, so the denylist
        // is the only thing that actually blocks. When images are present we keep
        // `Read` available (scoped via --add-dir + cwd) so Claude can load the
        // image files — the only way the CLI accepts images.
        cmd.arg("--disallowed-tools");
        for tool in CLAUDE_BUILTIN_TOOLS {
            if image_dir.is_some() && *tool == "Read" {
                continue;
            }
            cmd.arg(tool);
        }
        if let Some(dir) = image_dir {
            cmd.arg("--add-dir").arg(dir).current_dir(dir);
        }
        // Isolation: `--strict-mcp-config` makes the CLI use ONLY the MCP servers
        // we pass via `--mcp-config` and IGNORE the host user's `~/.claude.json`
        // servers. Passed unconditionally so that even when no gateway is attached
        // (plain-adapter fallback), the agent can never reach the operator's
        // personal MCP servers (gmail, browser, etc.) — those live entirely
        // outside AgentOS's capability/audit/sandbox layer. Without this the
        // `claude` CLI silently merges the user's servers into the agent's toolset.
        cmd.arg("--strict-mcp-config");
        // When an MCP config is attached, expose the 4 AgentOS meta-tools as
        // native MCP tools. The built-in denylist above still applies, so only
        // these `mcp__agentos__*` tools are allowed alongside the denied built-ins.
        if let Some(path) = &self.mcp_config_path {
            cmd.arg("--mcp-config").arg(path);
            cmd.arg("--allowed-tools")
                .arg("mcp__agentos__search_tools")
                .arg("mcp__agentos__describe_tool")
                .arg("mcp__agentos__list_tools")
                .arg("mcp__agentos__invoke_tool");
        }
        if !system.is_empty() {
            cmd.arg("--system-prompt").arg(system);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    /// Flatten the context to a `(system, prompt)` pair and, if the context
    /// carries images, write them to a fresh temp dir and append file
    /// references to the prompt. The returned `TempDir` must be kept alive
    /// until the subprocess exits (it is cleaned up on drop).
    fn prepare_invocation(
        &self,
        context: &ContextWindow,
        resume: Option<&SessionState>,
    ) -> Result<Invocation, AgentOSError> {
        let all = context.active_entries();
        let entry_count = all.len();
        // The system prompt is ALWAYS sent: it is rebuilt every turn with fresh
        // world-state reminders, the standing injection-safety rule, and the
        // current tool list — never dropped on resume.
        let (system, full_convo) = Self::flatten_context(context);
        // Fingerprint the full current prefix; recorded after the turn so the
        // next turn can verify the CLI session still matches before a delta.
        let fingerprint = Self::prefix_fingerprint(&system, &all);

        // Send a DELTA only when a stored session's recorded prefix still matches
        // the current one (same boundary + same fingerprint over `[..hwm]`). Any
        // divergence — compaction, reorder, eviction, or a changed system prompt
        // (the fingerprint folds in `system`) — falls back to a full send + fresh
        // session, so resume is opportunistic but always safe.
        let matched = resume.filter(|s| {
            s.last_sent_entry_count <= entry_count
                && Self::prefix_fingerprint(&system, &all[..s.last_sent_entry_count])
                    == s.fingerprint
        });
        let (convo, payload_entries, resume_session_id): (
            String,
            Vec<&ContextEntry>,
            Option<String>,
        ) = match matched {
            Some(s) => {
                // System entries are ALWAYS carried via `--system-prompt` (rebuilt
                // every turn by `flatten_context`), so drop them from the delta body
                // — otherwise a tail world-state reminder would be sent twice (once
                // in `--system-prompt`, once inlined as `## System`). The fingerprint
                // / high-water mark still count System entries (we slice `all`), so
                // only the rendered payload changes, not the guard boundary.
                let delta: Vec<&ContextEntry> = all[s.last_sent_entry_count..]
                    .iter()
                    .copied()
                    .filter(|e| e.role != ContextRole::System)
                    .collect();
                (
                    Self::flatten_delta(&delta),
                    delta,
                    Some(s.session_id.clone()),
                )
            }
            None => (full_convo, all.clone(), None),
        };
        let mut prompt = if convo.is_empty() {
            "Continue.".to_string()
        } else {
            convo
        };

        // Resolve images from the entries we're actually sending this turn.
        let mut images: Vec<(String, String)> = Vec::new();
        for entry in &payload_entries {
            for part in &entry.parts {
                if let ContentPart::Image { mime, source } = part {
                    match crate::media::resolve_image_to_base64(mime, source, &self.image_resolver)
                    {
                        Ok((m, b64)) => images.push((m, b64)),
                        Err(e) => prompt.push_str(&format!("\n\n[image could not be loaded: {e}]")),
                    }
                }
            }
        }
        if images.is_empty() {
            return Ok(Invocation {
                system,
                prompt,
                image_dir: None,
                entry_count,
                fingerprint,
                resume_session_id,
            });
        }

        let dir = tempfile::tempdir()
            .map_err(|e| self.err(format!("failed to create temp dir for images: {e}")))?;
        let mut names = Vec::new();
        for (i, (mime, b64)) in images.iter().enumerate() {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| self.err(format!("invalid image base64: {e}")))?;
            let ext = match mime.rsplit('/').next().unwrap_or("png") {
                "jpeg" => "jpg",
                other => other,
            };
            let name = format!("image_{}.{}", i + 1, ext);
            std::fs::write(dir.path().join(&name), &bytes)
                .map_err(|e| self.err(format!("failed to write image temp file: {e}")))?;
            names.push(name);
        }
        prompt.push_str(&format!(
            "\n\n[{} image file(s) attached in the current directory: {}. \
             Use the Read tool to view them.]",
            names.len(),
            names.join(", ")
        ));
        Ok(Invocation {
            system,
            prompt,
            image_dir: Some(dir),
            entry_count,
            fingerprint,
            resume_session_id,
        })
    }

    /// Stream a response by parsing `--output-format stream-json`: forward each
    /// text delta as an [`InferenceEvent::Token`], then the final result object
    /// as [`InferenceEvent::Done`]. On failure, emits [`InferenceEvent::Error`].
    /// Run one `claude -p --output-format json` subprocess for a prepared
    /// invocation, parse it, and record the resume session on success. Shared by
    /// the primary call and the resume-failure retry in [`LLMCore::infer`].
    async fn run_oneshot(
        &self,
        context: &ContextWindow,
        inv: Invocation,
    ) -> Result<InferenceResult, AgentOSError> {
        let mut cmd = self.base_command(
            &inv.prompt,
            &inv.system,
            inv.image_dir.as_ref().map(|d| d.path()),
            inv.resume_session_id.as_deref(),
        );
        cmd.arg("--output-format").arg("json");

        let start = Instant::now();
        let output = tokio::time::timeout(self.timeout, cmd.output())
            .await
            .map_err(|_| {
                self.err(format!(
                    "claude CLI timed out after {}s",
                    self.timeout.as_secs()
                ))
            })?
            .map_err(|e| self.err(format!("failed to spawn '{}': {e}", self.binary)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(self.err(format!(
                "claude CLI exited with {}: {}",
                output.status,
                stderr.trim()
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let (result, session_id) =
            self.parse_cli_json(&stdout, start.elapsed().as_millis() as u64)?;
        if let Some(sid) = session_id {
            self.record_resume(context, &sid, inv.entry_count, inv.fingerprint)
                .await;
        }
        Ok(result)
    }

    /// Invalidate (delete) the cached resume session for this context. Best-effort.
    async fn invalidate_resume(&self, context: &ContextWindow) {
        if let (Some(store), Some(key)) =
            (self.resume_store.as_ref(), context.resume_key.as_deref())
        {
            store.invalidate(key).await;
        }
    }

    /// Run one streaming subprocess for a prepared invocation, forwarding text
    /// deltas as [`InferenceEvent::Token`]. Does NOT emit the terminal
    /// `Done`/`Error` event — the caller decides that so it can retry on failure.
    /// On error, returns whether any token was already streamed (the caller must
    /// not retry once output has been emitted, or it would duplicate it).
    async fn stream_attempt(
        &self,
        inv: &Invocation,
        tx: &mpsc::Sender<InferenceEvent>,
    ) -> Result<(InferenceResult, Option<String>), (AgentOSError, bool)> {
        let mut cmd = self.base_command(
            &inv.prompt,
            &inv.system,
            inv.image_dir.as_ref().map(|d| d.path()),
            inv.resume_session_id.as_deref(),
        );
        // stream-json in print mode requires --verbose; --include-partial-messages
        // surfaces token-level content_block_delta events.
        cmd.arg("--output-format")
            .arg("stream-json")
            .arg("--verbose")
            .arg("--include-partial-messages");

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return Err((
                    self.err(format!("failed to spawn '{}': {e}", self.binary)),
                    false,
                ))
            }
        };
        let stdout = match child.stdout.take() {
            Some(s) => s,
            None => {
                let _ = child.start_kill();
                return Err((self.err("failed to capture claude CLI stdout"), false));
            }
        };
        let mut reader = BufReader::new(stdout).lines();
        let start = Instant::now();
        // Tracks whether we've forwarded any token to `tx`; gates the caller's retry.
        let mut emitted = false;

        let outcome = tokio::time::timeout(self.timeout, async {
            let mut final_result: Option<InferenceResult> = None;
            let mut final_session_id: Option<String> = None;
            while let Some(line) = reader
                .next_line()
                .await
                .map_err(|e| self.err(format!("error reading claude CLI stream: {e}")))?
            {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let v: serde_json::Value = match serde_json::from_str(trimmed) {
                    Ok(v) => v,
                    Err(_) => continue, // ignore any non-JSON noise
                };
                match v.get("type").and_then(|t| t.as_str()) {
                    Some("stream_event") => {
                        if let Some(text) = v
                            .get("event")
                            .filter(|e| {
                                e.get("type").and_then(|t| t.as_str())
                                    == Some("content_block_delta")
                            })
                            .and_then(|e| e.get("delta"))
                            .filter(|d| {
                                d.get("type").and_then(|t| t.as_str()) == Some("text_delta")
                            })
                            .and_then(|d| d.get("text"))
                            .and_then(|t| t.as_str())
                        {
                            if !text.is_empty() {
                                emitted = true;
                                let _ = tx.send(InferenceEvent::Token(text.to_string())).await;
                            }
                        }
                    }
                    Some("result") => {
                        // The result object matches the non-streaming shape.
                        let (res, sid) =
                            self.parse_cli_json(trimmed, start.elapsed().as_millis() as u64)?;
                        final_session_id = sid;
                        final_result = Some(res);
                    }
                    _ => {}
                }
            }
            let res = final_result
                .ok_or_else(|| self.err("claude CLI stream ended without a result event"))?;
            Ok::<_, AgentOSError>((res, final_session_id))
        })
        .await;

        match outcome {
            Ok(Ok((result, session_id))) => {
                let _ = child.wait().await;
                Ok((result, session_id))
            }
            Ok(Err(e)) => {
                let _ = child.start_kill();
                Err((e, emitted))
            }
            Err(_) => {
                let _ = child.start_kill();
                Err((
                    self.err(format!(
                        "claude CLI timed out after {}s",
                        self.timeout.as_secs()
                    )),
                    emitted,
                ))
            }
        }
    }

    async fn run_streaming(
        &self,
        context: &ContextWindow,
        tx: mpsc::Sender<InferenceEvent>,
    ) -> Result<(), AgentOSError> {
        let resume = self.lookup_resume(context).await;
        // `inv` (holding the temp image dir) is kept alive until the subprocess exits.
        let inv = self.prepare_invocation(context, resume.as_ref())?;
        let resumed = inv.resume_session_id.is_some();

        let outcome = match self.stream_attempt(&inv, &tx).await {
            Ok(ok) => Ok(ok),
            // Resume failed before streaming any output: drop the stale session and
            // retry once as a full, fresh send (mirrors the non-streaming path).
            // If output was already emitted we must NOT retry (would duplicate it).
            Err((e, emitted)) if resumed && !emitted => {
                tracing::warn!(error = %e, "claude --resume stream failed; retrying as full send");
                self.invalidate_resume(context).await;
                let inv = self.prepare_invocation(context, None)?;
                self.stream_attempt(&inv, &tx).await.map_err(|(e, _)| e)
            }
            Err((e, _)) => Err(e),
        };

        match outcome {
            Ok((result, session_id)) => {
                if let Some(sid) = session_id {
                    self.record_resume(context, &sid, inv.entry_count, inv.fingerprint)
                        .await;
                }
                let _ = tx.send(InferenceEvent::Done(result)).await;
                Ok(())
            }
            Err(e) => {
                let _ = tx.send(InferenceEvent::Error(e.to_string())).await;
                Err(e)
            }
        }
    }
}

#[async_trait]
impl LLMCore for ClaudeCodeCore {
    /// Native (MCP) tool-calling is active only when an MCP config is attached
    /// via [`ClaudeCodeCore::with_mcp_config`] — the subprocess then exposes the
    /// 4 AgentOS meta-tools as native `mcp__agentos__*` tools. Otherwise this is
    /// `false` and the kernel falls back to the JSON-in-markdown envelope path
    /// (tool instructions injected into the system prompt, tool calls recovered
    /// from `InferenceResult.text`).
    fn supports_native_tool_calling(&self) -> bool {
        self.mcp_config_path.is_some()
    }

    fn inference_watchdog_secs(&self) -> u64 {
        // One claude-code inference spawns a subprocess and, in MCP mode, runs
        // the agent's entire tool loop (discover → invoke → reason → repeat)
        // before returning — legitimately minutes. Use a generous watchdog so
        // the kernel doesn't abort real work at the 120s default.
        300
    }

    async fn infer(&self, context: &ContextWindow) -> Result<InferenceResult, AgentOSError> {
        let resume = self.lookup_resume(context).await;
        // `inv` (holding the temp image dir) is kept alive until the call returns.
        let inv = self.prepare_invocation(context, resume.as_ref())?;
        let resumed = inv.resume_session_id.is_some();

        match self.run_oneshot(context, inv).await {
            Ok(result) => Ok(result),
            Err(e) if resumed => {
                // The `--resume` session may have expired or been pruned CLI-side;
                // the fingerprint guard can't detect that. On any failure of a
                // resumed call, drop the cached session and retry once as a full,
                // fresh send. A genuine (non-resume) error simply recurs on the
                // retry and surfaces normally.
                tracing::warn!(error = %e, "claude --resume failed; retrying as full send");
                self.invalidate_resume(context).await;
                let inv = self.prepare_invocation(context, None)?;
                self.run_oneshot(context, inv).await
            }
            Err(e) => Err(e),
        }
    }

    async fn infer_stream(
        &self,
        context: &ContextWindow,
        tx: mpsc::Sender<InferenceEvent>,
    ) -> Result<(), AgentOSError> {
        self.run_streaming(context, tx).await
    }

    async fn infer_stream_with_tools(
        &self,
        context: &ContextWindow,
        _tools: &[ToolManifest],
        tx: mpsc::Sender<InferenceEvent>,
    ) -> Result<(), AgentOSError> {
        // Tools are described in the system prompt (non-native path), so the
        // structured list is unused — stream exactly as `infer_stream`.
        self.run_streaming(context, tx).await
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    async fn health_check(&self) -> HealthStatus {
        // `claude --version` is fast and free (no quota); a clean exit means the
        // CLI is installed and runnable. We do not probe auth here to avoid
        // burning subscription quota on every connect.
        let mut cmd = Command::new(&self.binary);
        cmd.arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        match tokio::time::timeout(Duration::from_secs(10), cmd.output()).await {
            Ok(Ok(out)) if out.status.success() => HealthStatus::Healthy,
            Ok(Ok(out)) => HealthStatus::Unhealthy {
                reason: format!("`{} --version` exited with {}", self.binary, out.status),
            },
            Ok(Err(e)) => HealthStatus::Unhealthy {
                reason: format!(
                    "cannot run '{}': {e}. Install Claude Code and run `claude` once to log in.",
                    self.binary
                ),
            },
            Err(_) => HealthStatus::Unhealthy {
                reason: format!("`{} --version` timed out", self.binary),
            },
        }
    }

    fn provider_name(&self) -> &str {
        "claude-code"
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}

/// Subset of the `claude --output-format json` result object we consume.
#[derive(Debug, Deserialize)]
struct ClaudeCliResult {
    #[serde(default)]
    is_error: bool,
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    duration_ms: Option<u64>,
    #[serde(default)]
    usage: Option<ClaudeCliUsage>,
    /// API-equivalent cost the CLI computed for this turn (USD). Used to feed
    /// AgentOS budget governance; not literal subscription billing.
    #[serde(default)]
    total_cost_usd: Option<f64>,
    /// CLI session id, used for opt-in `--resume` (see [`crate::session`]).
    #[serde(default)]
    session_id: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ClaudeCliUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::{ContextEntry, ContextWindow};

    fn entry(role: ContextRole, text: &str) -> ContextEntry {
        ContextEntry::from_text(role, text.to_string())
    }

    #[test]
    fn parse_success_sums_cached_tokens_into_prompt() {
        let core = ClaudeCodeCore::new("claude-sonnet-4-6");
        let json = r#"{
            "type":"result","subtype":"success","is_error":false,
            "result":"hello world","stop_reason":"end_turn","duration_ms":1892,
            "usage":{"input_tokens":3,"output_tokens":4,
                     "cache_read_input_tokens":100,"cache_creation_input_tokens":20}
        }"#;
        let (r, _sid) = core.parse_cli_json(json, 0).expect("parse ok");
        assert_eq!(r.text, "hello world");
        assert_eq!(r.stop_reason, StopReason::EndTurn);
        assert_eq!(r.tokens_used.prompt_tokens, 123); // 3 + 100 + 20
        assert_eq!(r.tokens_used.completion_tokens, 4);
        assert_eq!(r.tokens_used.total_tokens, 127);
        assert_eq!(r.cached_tokens, 100);
        assert_eq!(r.duration_ms, 1892);
        assert!(r.tool_calls.is_empty());
    }

    #[test]
    fn parse_is_error_returns_err() {
        let core = ClaudeCodeCore::new("default");
        let json = r#"{"type":"result","is_error":true,"result":"rate limited"}"#;
        let err = core.parse_cli_json(json, 0).unwrap_err();
        assert!(format!("{err}").contains("rate limited"));
    }

    #[test]
    fn parse_malformed_returns_err() {
        let core = ClaudeCodeCore::new("default");
        assert!(core.parse_cli_json("not json", 0).is_err());
    }

    #[test]
    fn parse_maps_stop_reasons() {
        let core = ClaudeCodeCore::new("default");
        let mk = |sr: &str| format!(r#"{{"is_error":false,"result":"x","stop_reason":"{sr}"}}"#);
        assert_eq!(
            core.parse_cli_json(&mk("max_tokens"), 0)
                .unwrap()
                .0
                .stop_reason,
            StopReason::MaxTokens
        );
        assert_eq!(
            core.parse_cli_json(&mk("weird"), 0).unwrap().0.stop_reason,
            StopReason::Other("weird".to_string())
        );
    }

    #[test]
    fn parse_missing_duration_uses_fallback() {
        let core = ClaudeCodeCore::new("default");
        let json = r#"{"is_error":false,"result":"x"}"#;
        assert_eq!(core.parse_cli_json(json, 555).unwrap().0.duration_ms, 555);
    }

    #[test]
    fn parse_populates_cost_for_budget_governance() {
        let core = ClaudeCodeCore::new("default");
        // With total_cost_usd → cost is surfaced so the budget tracker sees it.
        let (r, _sid) = core
            .parse_cli_json(
                r#"{"is_error":false,"result":"x","total_cost_usd":0.0425}"#,
                0,
            )
            .unwrap();
        let cost = r.cost.expect("cost populated from total_cost_usd");
        assert!((cost.total_cost_usd - 0.0425).abs() < 1e-9);
        // Without it → None (no fabricated cost).
        assert!(core
            .parse_cli_json(r#"{"is_error":false,"result":"x"}"#, 0)
            .unwrap()
            .0
            .cost
            .is_none());
    }

    #[test]
    fn parse_captures_session_id() {
        let core = ClaudeCodeCore::new("default");
        let (_r, sid) = core
            .parse_cli_json(
                r#"{"is_error":false,"result":"x","session_id":"abc123"}"#,
                0,
            )
            .unwrap();
        assert_eq!(sid.as_deref(), Some("abc123"));
        let (_r2, sid2) = core
            .parse_cli_json(r#"{"is_error":false,"result":"x"}"#, 0)
            .unwrap();
        assert!(sid2.is_none());
    }

    #[test]
    fn base_command_adds_resume_when_session_present() {
        let core = ClaudeCodeCore::new("default");
        let args = cmd_args(&core.base_command("delta", "", None, Some("sess-1")));
        assert!(args.iter().any(|a| a == "--resume"));
        assert!(args.iter().any(|a| a == "sess-1"));
    }

    #[test]
    fn base_command_omits_resume_when_none() {
        let core = ClaudeCodeCore::new("default");
        let args = cmd_args(&core.base_command("full", "sys", None, None));
        assert!(!args.iter().any(|a| a == "--resume"));
    }

    #[test]
    fn prepare_invocation_resume_sends_only_delta_when_fingerprint_matches() {
        let core = ClaudeCodeCore::new("default");
        let mut ctx = ContextWindow::new(10_000);
        ctx.push(entry(ContextRole::System, "SYS"));
        ctx.push(entry(ContextRole::User, "first"));
        ctx.push(entry(ContextRole::Assistant, "reply"));
        ctx.push(entry(ContextRole::User, "second"));
        // 4 active entries; hwm = 3. For the delta to fire, the stored
        // fingerprint must match the current prefix [..3].
        let all = ctx.active_entries();
        let (system, _) = ClaudeCodeCore::flatten_context(&ctx);
        let fp = ClaudeCodeCore::prefix_fingerprint(&system, &all[..3]);
        let resume = SessionState {
            session_id: "s".into(),
            last_sent_entry_count: 3,
            fingerprint: fp,
        };
        let inv = core.prepare_invocation(&ctx, Some(&resume)).unwrap();
        assert_eq!(inv.entry_count, 4);
        assert_eq!(inv.resume_session_id.as_deref(), Some("s"));
        // System is ALWAYS re-sent (H1 fix), but the conversation is delta-only.
        assert!(!inv.system.is_empty(), "system prompt is always re-sent");
        assert!(inv.prompt.contains("second"));
        assert!(!inv.prompt.contains("first"));
    }

    #[test]
    fn prepare_invocation_full_send_when_fingerprint_mismatches() {
        let core = ClaudeCodeCore::new("default");
        let mut ctx = ContextWindow::new(10_000);
        ctx.push(entry(ContextRole::User, "first"));
        ctx.push(entry(ContextRole::User, "second"));
        // Stale/wrong fingerprint ⇒ no resume; full context + no --resume.
        let resume = SessionState {
            session_id: "s".into(),
            last_sent_entry_count: 1,
            fingerprint: 0xDEAD_BEEF,
        };
        let inv = core.prepare_invocation(&ctx, Some(&resume)).unwrap();
        assert!(
            inv.resume_session_id.is_none(),
            "fingerprint mismatch must fall back to a fresh full send"
        );
        assert!(inv.prompt.contains("first") && inv.prompt.contains("second"));
    }

    #[test]
    fn default_model_applied() {
        assert_eq!(ClaudeCodeCore::new("").model_name(), DEFAULT_MODEL);
        assert_eq!(ClaudeCodeCore::new("default").model_name(), DEFAULT_MODEL);
        assert_eq!(
            ClaudeCodeCore::new("claude-opus-4-8").model_name(),
            "claude-opus-4-8"
        );
    }

    #[test]
    fn flatten_splits_system_and_marks_roles() {
        let mut ctx = ContextWindow::new(10_000);
        ctx.push(entry(ContextRole::System, "SYS-A"));
        ctx.push(entry(ContextRole::System, "SYS-B"));
        ctx.push(entry(ContextRole::User, "hi there"));
        ctx.push(entry(ContextRole::Assistant, "hello"));
        ctx.push(entry(ContextRole::ToolResult, "tool out"));
        let (system, convo) = ClaudeCodeCore::flatten_context(&ctx);
        assert!(system.contains("SYS-A") && system.contains("SYS-B"));
        assert!(!convo.contains("SYS-A"));
        assert!(convo.contains("## User") && convo.contains("hi there"));
        assert!(convo.contains("## Assistant") && convo.contains("hello"));
        assert!(convo.contains("## Tool Result") && convo.contains("tool out"));
    }

    #[test]
    fn provider_name_is_claude_code() {
        assert_eq!(
            ClaudeCodeCore::new("default").provider_name(),
            "claude-code"
        );
    }

    #[test]
    fn supports_images_and_streaming() {
        let caps = ClaudeCodeCore::new("default").capabilities().clone();
        assert!(caps.supports_images);
        assert!(caps.supports_streaming);
    }

    #[test]
    fn prepare_invocation_no_images_returns_none() {
        let core = ClaudeCodeCore::new("default");
        let mut ctx = ContextWindow::new(10_000);
        ctx.push(entry(ContextRole::User, "hello"));
        let inv = core.prepare_invocation(&ctx, None).unwrap();
        assert!(inv.image_dir.is_none());
        assert!(inv.prompt.contains("hello"));
    }

    #[test]
    fn prepare_invocation_writes_image_and_references_it() {
        let core = ClaudeCodeCore::new("default");
        let mut ctx = ContextWindow::new(10_000);
        // 1x1 PNG.
        let b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNgAAIAAAUAAen63NgAAAAASUVORK5CYII=";
        let mut e = entry(ContextRole::User, "what is in this image?");
        e.parts.push(ContentPart::Image {
            mime: "image/png".to_string(),
            source: agentos_types::ImageSource::Base64 {
                data: b64.to_string(),
            },
        });
        ctx.push(e);
        let inv = core.prepare_invocation(&ctx, None).expect("prepare ok");
        let dir = inv.image_dir.expect("temp image dir created");
        assert!(dir.path().join("image_1.png").exists());
        assert!(inv.prompt.contains("image_1.png"));
        assert!(inv.prompt.contains("Read tool"));
    }

    fn cmd_args(cmd: &Command) -> Vec<String> {
        cmd.as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn base_command_denies_all_builtins_without_images() {
        let core = ClaudeCodeCore::new("default");
        let args = cmd_args(&core.base_command("hi", "", None, None));
        assert!(args.iter().any(|a| a == "--disallowed-tools"));
        // No additive allowlist (it doesn't restrict in the default mode).
        assert!(!args.iter().any(|a| a == "--allowed-tools"));
        // Host-touching tools must be denied, including Read when no images.
        for t in ["Write", "Bash", "Read", "Edit", "WebFetch"] {
            assert!(args.iter().any(|a| a == t), "expected {t} in denylist");
        }
    }

    #[test]
    fn base_command_keeps_read_only_for_images() {
        let core = ClaudeCodeCore::new("default");
        let args = cmd_args(&core.base_command("hi", "", Some(std::path::Path::new("/tmp")), None));
        assert!(args.iter().any(|a| a == "--disallowed-tools"));
        assert!(args.iter().any(|a| a == "--add-dir"));
        // Read stays available for image loading; everything else stays denied.
        assert!(!args.iter().any(|a| a == "Read"));
        assert!(args.iter().any(|a| a == "Write"));
        assert!(args.iter().any(|a| a == "Bash"));
    }

    #[test]
    fn base_command_with_mcp_config_adds_flags() {
        use std::path::PathBuf;
        let core = ClaudeCodeCore::new("default").with_mcp_config(PathBuf::from("/tmp/x.json"));
        let args = cmd_args(&core.base_command("hi", "", None, None));
        // MCP config + the 4 allowed meta-tools are present.
        assert!(args.iter().any(|a| a == "--mcp-config"));
        assert!(args.iter().any(|a| a == "/tmp/x.json"));
        assert!(args.iter().any(|a| a == "mcp__agentos__invoke_tool"));
        // Built-ins remain denied.
        assert!(args.iter().any(|a| a == "--disallowed-tools"));
        assert!(args.iter().any(|a| a == "Write"));
    }

    #[test]
    fn supports_native_reflects_mcp_config() {
        use std::path::PathBuf;
        assert!(!ClaudeCodeCore::new("default").supports_native_tool_calling());
        assert!(ClaudeCodeCore::new("default")
            .with_mcp_config(PathBuf::from("/tmp/x.json"))
            .supports_native_tool_calling());
    }
}
