use crate::kernel::Kernel;
use agentos_audit::{AuditEntry, AuditEventType, AuditSeverity};
use agentos_types::*;
use chrono::Utc;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Cap for `channel-send file_path` uploads. Telegram's bot limit is 50 MB;
/// the bytes ride through the notification router base64-encoded, so keep
/// well under it.
const CHANNEL_SEND_MAX_FILE_BYTES: u64 = 20 * 1024 * 1024;
/// Telegram `sendPhoto` limit; larger images are sent as documents.
const TELEGRAM_PHOTO_MAX_BYTES: u64 = 10 * 1024 * 1024;

enum AgentFileError {
    /// The open handle resolves outside the agent home (payload = real path).
    Outside(String),
    Other(String),
}

/// Read one of an agent's own files for upload, race-free and bounded.
///
/// Opens first, then asks where the *open handle* points (`/proc/self/fd`),
/// so a symlink swapped in after any earlier path check cannot redirect the
/// read. Size comes from fstat on that handle, and the read itself is capped
/// at `max + 1` bytes, so a file still growing cannot blow past the limit.
/// `O_NONBLOCK` keeps a FIFO planted in the home from hanging the thread in
/// `open`.
fn read_agent_file_blocking(
    home: &std::path::Path,
    path: &std::path::Path,
    max: u64,
) -> Result<Vec<u8>, AgentFileError> {
    use std::io::Read;
    let home = home
        .canonicalize()
        .map_err(|e| AgentFileError::Other(format!("agent files directory unavailable: {e}")))?;
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NONBLOCK);
    }
    let file = opts
        .open(path)
        .map_err(|e| AgentFileError::Other(format!("could not open: {e}")))?;
    #[cfg(target_os = "linux")]
    let real = {
        use std::os::fd::AsRawFd;
        std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
    };
    // ponytail: off Linux the path is re-resolved after open, leaving a narrow
    // swap window; Linux is the supported target.
    #[cfg(not(target_os = "linux"))]
    let real = path.canonicalize();
    let real = real.map_err(|e| AgentFileError::Other(format!("could not resolve: {e}")))?;
    if !real.starts_with(&home) {
        return Err(AgentFileError::Outside(real.to_string_lossy().into_owned()));
    }
    let meta = file
        .metadata()
        .map_err(|e| AgentFileError::Other(format!("could not stat: {e}")))?;
    if !meta.is_file() {
        return Err(AgentFileError::Other("not a regular file".into()));
    }
    let too_big =
        || AgentFileError::Other(format!("larger than the {max}-byte channel upload cap"));
    if meta.len() > max {
        return Err(too_big());
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| AgentFileError::Other(format!("could not read: {e}")))?;
    if bytes.len() as u64 > max {
        return Err(too_big());
    }
    Ok(bytes)
}

/// Built-in delivery adapter kinds accepted as `notify-user` channel selectors.
///
/// These names are matched against `DeliveryAdapter::channel_id().as_str()` when
/// no registered channel row exists for the selector. Must stay in lockstep with
/// `DeliveryChannel::{CLI,WEB,WEBHOOK,DESKTOP,SLACK}` constants.
const BUILTIN_DELIVERY_KINDS: &[&str] = &["cli", "web", "webhook", "desktop", "slack"];

/// Actions that tools can request the kernel to perform on their behalf.
///
/// Tools return a `_kernel_action` field in their result JSON to signal
/// that the kernel should intercept the result and perform a privileged
/// operation (e.g., delegating a task, sending an inter-agent message).
/// This keeps tools stateless while the kernel retains full control over
/// scheduling, permissions, and audit.
#[derive(Debug)]
pub(crate) enum KernelAction {
    DelegateTask {
        target_agent: String,
        prompt: String,
        priority: u8,
    },
    SendAgentMessage {
        to: String,
        content: String,
    },
    EscalateToHuman {
        reason: EscalationReason,
        context_summary: String,
        decision_point: String,
        options: Vec<String>,
        urgency: String,
        blocking: bool,
    },
    SwitchPartition {
        partition: String, // "active" or "scratchpad"
    },
    /// Run a stored executable procedure as the calling agent.
    ///
    /// Every step is gated individually against that agent's own
    /// `PermissionSet`, so this grants no authority the agent did not have.
    RunProcedure {
        procedure: String,
        inputs: serde_json::Value,
        detach: bool,
    },
    MemoryBlockWrite {
        label: String,
        content: String,
    },
    MemoryBlockRead {
        label: String,
    },
    MemoryBlockList,
    MemoryBlockDelete {
        label: String,
    },
    /// Fire-and-forget notification to the user inbox.
    ///
    /// `channels`: empty = fan out to all registered delivery adapters.
    /// Non-empty = restrict to selected channels (matched by registered channel
    /// display name, `ChannelInstanceID`, or channel kind/id like "telegram").
    NotifyUser {
        subject: String,
        body: String,
        priority: String,
        channels: Vec<String>,
    },
    /// Blocking question to the user — task pauses until user responds.
    AskUser {
        question: String,
        options: Option<Vec<String>>,
        timeout_secs: u64,
        priority: String,
        auto_action: String,
    },
    /// Blocking request for access to a host folder. Raises an escalation whose
    /// approval WRITES the workspace grant before the caller is woken — an
    /// approval that changes no state is what made 2026-09-21 unrecoverable.
    WorkspaceRequest {
        path: String,
        mode: String,
        reason: String,
        timeout_secs: u64,
    },
    /// Synchronous agent-to-agent RPC call — blocks until the target agent
    /// completes the child task and returns its output.
    AgentRpcCall {
        target_agent: String,
        prompt: String,
        timeout_secs: u64,
    },
    /// Update the agent's self-curated context memory document.
    ContextMemoryUpdate {
        content: String,
        reason: Option<String>,
    },
    /// Read the agent's current context memory document.
    ContextMemoryRead,
    /// Full-text search over the agent's own past chat sessions.
    ChatSearch {
        query: String,
        limit: usize,
    },
    /// Spawn a sub-agent task scoped to the current task's capabilities.
    SpawnAgent {
        agent: String,
        prompt: String,
        permissions: Vec<String>,
        context_messages: u64,
    },
    /// Wait for spawned sub-agent tasks and collect their results.
    AwaitAgents {
        task_ids: Vec<String>,
    },
    /// Non-blocking poll of spawned sub-agent status and progress.
    PollAgents {
        task_ids: Vec<String>,
        include_progress: bool,
    },
    /// Cancel a spawned sub-agent (cascades to grandchildren).
    CancelAgent {
        task_id: String,
        reason: String,
    },
    /// Fire-and-forget async spawn. Creates a child task but does NOT add a
    /// scheduler dependency, so the parent continues immediately. On completion
    /// the child's result is injected into the spawner's context window.
    SpawnAsync {
        target_agent: String,
        prompt: String,
        priority: u8,
    },
    /// Delegate a task to an external A2A-compliant agent via HTTP.
    A2ADelegate {
        agent_url: String,
        capability: String,
        input: serde_json::Value,
        token: Option<String>,
        wait_for_result: bool,
    },
    /// Subscribe the calling agent to events matching a filter.
    /// Permission-gated per `EventCategory` via `event_permissions`.
    EventSubscribeAction {
        event_filter: String,
        payload_filter: Option<String>,
        throttle: Option<String>,
        priority: Option<String>,
    },
    /// Cancel one of the calling agent's own subscriptions by ID.
    EventUnsubscribeAction {
        subscription_id: String,
    },
    /// Return all subscriptions belonging to the calling agent.
    EventListSubscriptionsAction,
    /// Enumerate all event categories and types, marking which ones the
    /// calling agent currently has permission to subscribe to.
    EventListAvailableAction,
    /// Create an in-memory one-shot timer that fires after `delay_secs`.
    SetTimer {
        name: String,
        delay_secs: u64,
        agent_name: String,
        action: TimerAction,
    },
    /// Cancel a pending in-memory timer by name.
    CancelTimer {
        name: String,
    },
    /// List all pending in-memory timers.
    ListTimers,
    /// Schedule a one-shot action at an absolute datetime (or a relative delay).
    ScheduleOnce {
        name: String,
        action: agentos_types::schedule::OnceJobAction,
        agent_name: String,
        fire_at: chrono::DateTime<chrono::Utc>,
    },
    /// Cancel a pending once-job by name.
    CancelOnceJob {
        name: String,
    },
    /// List all pending once-jobs.
    ListOnceJobs,
    /// Query per-fire run history for a given schedule.
    GetScheduleRuns {
        schedule_id: String,
        limit: u32,
        state_filter: Option<String>,
    },
    /// List the calling agent's own schedules (cron / once / timer).
    ListMySchedules {
        kinds: Vec<String>,
        include_inactive: bool,
    },
    /// Fetch the recorded output + audit logs for one scheduled run.
    GetTaskLogs {
        run_id: String,
    },
    /// Create a recurring cron schedule.
    CreateSchedule {
        name: String,
        cron: String,
        agent_name: String,
        mode: String,
        task_prompt: Option<String>,
        notify_subject: Option<String>,
        notify_body: Option<String>,
        notify_priority: Option<String>,
        tool: Option<String>,
        tool_args: Option<serde_json::Value>,
    },
    /// Pause/resume/delete a schedule by name.
    ControlSchedule {
        action: String,
        name: String,
    },
    /// Send a message to a single connected channel by name or ID.
    /// Distinct from NotifyUser, which fans out to every registered delivery
    /// adapter. ChannelSend is targeted: agent picks one channel.
    ChannelSend {
        /// Display name (e.g. "telegram-main") or `ChannelInstanceID` UUID.
        channel: String,
        /// Message body. Markdown is rendered per-platform when supported.
        /// May be empty when an `attachment` is present.
        text: String,
        /// Optional thread/reply target (platform-specific).
        thread_id: Option<String>,
        /// Optional media attachment (image/document by URL).
        attachment: Option<MessageAttachment>,
        /// Stored file id to resolve to bytes and upload directly (Telegram
        /// multipart). Mutually exclusive with a URL `attachment`.
        file_id: Option<String>,
        /// Absolute path inside the agent home, already contained by the
        /// tool wrapper; read and uploaded like a `file_id`.
        file_path: Option<String>,
        /// Caption/filename applied to the resolved `file_id` attachment.
        caption: Option<String>,
        filename: Option<String>,
    },
    AgentInboxList {
        limit: u32,
        unread_only: bool,
    },
    AgentInboxRead {
        id: String,
    },
    AgentInboxDismiss {
        id: String,
    },
    AgentMessagesList {
        limit: u32,
        unread_only: bool,
    },
    AgentMessagesRead {
        id: String,
    },
    AgentMessagesDismiss {
        id: String,
    },
}

/// Why an agent is requesting human escalation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum EscalationReason {
    /// Agent is uncertain about the correct approach.
    Uncertainty,
    /// Agent detected a potential safety or security concern.
    SafetyConcern,
    /// Agent needs authorization for a high-impact action.
    AuthorizationRequired,
    /// Agent encountered an ambiguous or contradictory instruction.
    AmbiguousInstruction,
    /// Custom reason not covered above.
    Other(String),
}

/// Outcome of executing a kernel action, used to build the tool result
/// that gets pushed into the agent's context.
pub(crate) struct KernelActionResult {
    pub success: bool,
    pub result: serde_json::Value,
}

impl KernelAction {
    /// Stable snake_case name of the action (audit + error messages).
    pub(crate) fn name(&self) -> &'static str {
        match self {
            KernelAction::DelegateTask { .. } => "delegate_task",
            KernelAction::SendAgentMessage { .. } => "send_agent_message",
            KernelAction::EscalateToHuman { .. } => "escalate",
            KernelAction::SwitchPartition { .. } => "switch_partition",
            KernelAction::RunProcedure { .. } => "run_procedure",
            KernelAction::MemoryBlockWrite { .. } => "memory_block_write",
            KernelAction::MemoryBlockRead { .. } => "memory_block_read",
            KernelAction::MemoryBlockList => "memory_block_list",
            KernelAction::MemoryBlockDelete { .. } => "memory_block_delete",
            KernelAction::NotifyUser { .. } => "notify_user",
            KernelAction::AskUser { .. } => "ask_user",
            KernelAction::WorkspaceRequest { .. } => "workspace_request",
            KernelAction::AgentRpcCall { .. } => "agent_rpc_call",
            KernelAction::ContextMemoryUpdate { .. } => "context_memory_update",
            KernelAction::ContextMemoryRead => "context_memory_read",
            KernelAction::ChatSearch { .. } => "chat_search",
            KernelAction::SpawnAgent { .. } => "spawn_agent",
            KernelAction::AwaitAgents { .. } => "await_agents",
            KernelAction::PollAgents { .. } => "poll_agents",
            KernelAction::CancelAgent { .. } => "cancel_agent",
            KernelAction::SpawnAsync { .. } => "spawn_async",
            KernelAction::A2ADelegate { .. } => "a2a_delegate",
            KernelAction::EventSubscribeAction { .. } => "event_subscribe",
            KernelAction::EventUnsubscribeAction { .. } => "event_unsubscribe",
            KernelAction::EventListSubscriptionsAction => "event_list_subscriptions",
            KernelAction::EventListAvailableAction => "event_list_available",
            KernelAction::SetTimer { .. } => "set_timer",
            KernelAction::CancelTimer { .. } => "cancel_timer",
            KernelAction::ListTimers => "list_timers",
            KernelAction::ScheduleOnce { action, .. } => match action {
                agentos_types::schedule::OnceJobAction::RunTask { .. } => "schedule_once:task",
                agentos_types::schedule::OnceJobAction::NotifyUser { .. } => "schedule_once:notify",
                agentos_types::schedule::OnceJobAction::RunTool { .. } => "schedule_once:tool",
            },
            KernelAction::CancelOnceJob { .. } => "cancel_once_job",
            KernelAction::ListOnceJobs => "list_once_jobs",
            KernelAction::GetScheduleRuns { .. } => "get_schedule_runs",
            KernelAction::ListMySchedules { .. } => "list_my_schedules",
            KernelAction::GetTaskLogs { .. } => "get_task_logs",
            KernelAction::CreateSchedule { .. } => "create_schedule",
            KernelAction::ControlSchedule { .. } => "control_schedule",
            KernelAction::ChannelSend { .. } => "channel_send",
            KernelAction::AgentInboxList { .. } => "agent_inbox_list",
            KernelAction::AgentInboxRead { .. } => "agent_inbox_read",
            KernelAction::AgentInboxDismiss { .. } => "agent_inbox_dismiss",
            KernelAction::AgentMessagesList { .. } => "agent_messages_list",
            KernelAction::AgentMessagesRead { .. } => "agent_messages_read",
            KernelAction::AgentMessagesDismiss { .. } => "agent_messages_dismiss",
        }
    }
    /// Try to parse a kernel action from a tool result.
    /// Returns `None` if the result does not contain a `_kernel_action` field.
    pub fn from_tool_result(value: &serde_json::Value) -> Option<Self> {
        let action = value.get("_kernel_action")?.as_str()?;
        match action {
            "delegate_task" => {
                let target_agent = value.get("target_agent")?.as_str()?.to_string();
                let prompt = value.get("task")?.as_str()?.to_string();
                let priority = value.get("priority").and_then(|v| v.as_u64()).unwrap_or(5) as u8;
                Some(Self::DelegateTask {
                    target_agent,
                    prompt,
                    priority,
                })
            }
            "run_procedure" => {
                let procedure = value.get("procedure")?.as_str()?.to_string();
                Some(Self::RunProcedure {
                    procedure,
                    inputs: value
                        .get("inputs")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                    detach: value
                        .get("detach")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                })
            }
            "send_agent_message" => {
                let to = value.get("to")?.as_str()?.to_string();
                let content = value.get("content")?.as_str()?.to_string();
                Some(Self::SendAgentMessage { to, content })
            }
            "agent_inbox_list" => {
                let limit = value.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as u32;
                let unread_only = value
                    .get("unread_only")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                Some(Self::AgentInboxList { limit, unread_only })
            }
            "agent_inbox_read" => {
                let id = value.get("id")?.as_str()?.to_string();
                Some(Self::AgentInboxRead { id })
            }
            "agent_inbox_dismiss" => {
                let id = value.get("id")?.as_str()?.to_string();
                Some(Self::AgentInboxDismiss { id })
            }
            "agent_messages_list" => {
                let limit = value.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as u32;
                let unread_only = value
                    .get("unread_only")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                Some(Self::AgentMessagesList { limit, unread_only })
            }
            "agent_messages_read" => {
                let id = value.get("id")?.as_str()?.to_string();
                Some(Self::AgentMessagesRead { id })
            }
            "agent_messages_dismiss" => {
                let id = value.get("id")?.as_str()?.to_string();
                Some(Self::AgentMessagesDismiss { id })
            }
            "escalate" => {
                let reason_str = value
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("other");
                let reason = match reason_str {
                    "uncertainty" => EscalationReason::Uncertainty,
                    "safety_concern" => EscalationReason::SafetyConcern,
                    "authorization_required" => EscalationReason::AuthorizationRequired,
                    "ambiguous_instruction" => EscalationReason::AmbiguousInstruction,
                    other => EscalationReason::Other(other.to_string()),
                };
                let context_summary = value
                    .get("context_summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let decision_point = value
                    .get("decision_point")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let options = value
                    .get("options")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                let urgency = value
                    .get("urgency")
                    .and_then(|v| v.as_str())
                    .unwrap_or("normal")
                    .to_string();
                let blocking = value
                    .get("blocking")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                Some(Self::EscalateToHuman {
                    reason,
                    context_summary,
                    decision_point,
                    options,
                    urgency,
                    blocking,
                })
            }
            "switch_partition" => {
                let partition = value
                    .get("partition")
                    .and_then(|v| v.as_str())
                    .unwrap_or("active")
                    .to_string();
                Some(Self::SwitchPartition { partition })
            }
            "memory_block_write" => {
                let label = value.get("label")?.as_str()?.to_string();
                let content = value.get("content")?.as_str()?.to_string();
                Some(Self::MemoryBlockWrite { label, content })
            }
            "memory_block_read" => {
                let label = value.get("label")?.as_str()?.to_string();
                Some(Self::MemoryBlockRead { label })
            }
            "memory_block_list" => Some(Self::MemoryBlockList),
            "memory_block_delete" => {
                let label = value.get("label")?.as_str()?.to_string();
                Some(Self::MemoryBlockDelete { label })
            }
            "notify_user" => {
                let subject = value.get("subject")?.as_str()?.to_string();
                let body = value.get("body")?.as_str()?.to_string();
                let priority = value
                    .get("priority")
                    .and_then(|v| v.as_str())
                    .unwrap_or("info")
                    .to_string();
                let channels: Vec<String> = match value.get("channels") {
                    Some(serde_json::Value::Array(arr)) => arr
                        .iter()
                        .filter_map(|v| v.as_str())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect(),
                    Some(serde_json::Value::String(s)) => {
                        let t = s.trim();
                        if t.is_empty() {
                            Vec::new()
                        } else {
                            vec![t.to_string()]
                        }
                    }
                    _ => Vec::new(),
                };
                Some(Self::NotifyUser {
                    subject,
                    body,
                    priority,
                    channels,
                })
            }
            "channel_send" => {
                let channel = value.get("channel")?.as_str()?;
                let text = value
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let str_field = |key: &str| {
                    value
                        .get(key)
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                };
                let image_url = str_field("image_url");
                let document_url = str_field("document_url");
                let file_id = str_field("file_id");
                let file_path = str_field("file_path");
                let caption = str_field("caption");
                let filename = str_field("filename");
                // Album of image URLs (Telegram sendMediaGroup): 2–10 items.
                let image_urls: Vec<String> = value["image_urls"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str())
                            .filter(|s| !s.is_empty())
                            .map(|s| s.to_string())
                            .collect()
                    })
                    .unwrap_or_default();

                // Build the URL attachment. Priority: image album → image → document.
                // A `file_id` is resolved to bytes later in execute_channel_send.
                let attachment = if image_urls.len() > 1 {
                    let mut it = image_urls.into_iter();
                    let first = it.next().unwrap();
                    Some(MessageAttachment {
                        url: first,
                        kind: AttachmentKind::Image,
                        filename: filename.clone(),
                        caption: caption.clone(),
                        inline: None,
                        group_urls: it.collect(),
                    })
                } else {
                    match (
                        image_url.or_else(|| image_urls.into_iter().next()),
                        document_url,
                    ) {
                        (Some(url), _) => Some(MessageAttachment {
                            url,
                            kind: AttachmentKind::Image,
                            filename: filename.clone(),
                            caption: caption.clone(),
                            inline: None,
                            group_urls: Vec::new(),
                        }),
                        (None, Some(url)) => Some(MessageAttachment {
                            url,
                            kind: AttachmentKind::Document,
                            filename: filename.clone(),
                            caption: caption.clone(),
                            inline: None,
                            group_urls: Vec::new(),
                        }),
                        (None, None) => None,
                    }
                };

                if channel.trim().is_empty()
                    || (text.is_empty()
                        && attachment.is_none()
                        && file_id.is_none()
                        && file_path.is_none())
                {
                    tracing::warn!(
                        "Dropping channel_send: channel must be non-empty and a message needs text, an attachment, a file_id, or a file_path"
                    );
                    return None;
                }
                let thread_id = value
                    .get("thread_id")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string());
                Some(Self::ChannelSend {
                    channel: channel.to_string(),
                    text: text.to_string(),
                    thread_id,
                    attachment,
                    file_id,
                    file_path,
                    caption,
                    filename,
                })
            }
            "ask_user" => {
                let question = value.get("question")?.as_str()?.to_string();
                let options = value.get("options").and_then(|v| v.as_array()).map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                });
                let timeout_secs = value
                    .get("timeout_secs")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(300);
                let priority = value
                    .get("priority")
                    .and_then(|v| v.as_str())
                    .unwrap_or("info")
                    .to_string();
                let auto_action = value
                    .get("auto_action")
                    .and_then(|v| v.as_str())
                    .unwrap_or("auto_denied")
                    .to_string();
                Some(Self::AskUser {
                    question,
                    options,
                    timeout_secs,
                    priority,
                    auto_action,
                })
            }
            "workspace_request" => {
                let path = value.get("path")?.as_str()?.to_string();
                let reason = value.get("reason")?.as_str()?.to_string();
                let mode = value
                    .get("mode")
                    .and_then(|v| v.as_str())
                    .unwrap_or("r")
                    .to_string();
                let timeout_secs = value
                    .get("timeout_secs")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(300);
                Some(Self::WorkspaceRequest {
                    path,
                    mode,
                    reason,
                    timeout_secs,
                })
            }
            "agent_rpc_call" => {
                let target_agent = value.get("target_agent")?.as_str()?.to_string();
                let prompt = value.get("prompt")?.as_str()?.to_string();
                let timeout_secs = value
                    .get("timeout_secs")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(300);
                Some(Self::AgentRpcCall {
                    target_agent,
                    prompt,
                    timeout_secs,
                })
            }
            "context_memory_update" => {
                // No `agent_id`: these act on the calling agent, resolved from
                // `task.agent_id` at dispatch. Parsing one from the envelope
                // would make it look authoritative when it is not.
                let content = value.get("content")?.as_str()?.to_string();
                let reason = value
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                Some(Self::ContextMemoryUpdate { content, reason })
            }
            "context_memory_read" => Some(Self::ContextMemoryRead),
            "chat_search" => {
                let query = value.get("query")?.as_str()?.to_string();
                let limit = value
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(10)
                    .clamp(1, 50) as usize;
                Some(Self::ChatSearch { query, limit })
            }
            "spawn_agent" => {
                let agent = value.get("agent")?.as_str()?.to_string();
                let prompt = value.get("prompt")?.as_str()?.to_string();
                let permissions = value
                    .get("permissions")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                let context_messages = value
                    .get("context_messages")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(10);
                Some(Self::SpawnAgent {
                    agent,
                    prompt,
                    permissions,
                    context_messages,
                })
            }
            "await_agents" => {
                let task_ids = value
                    .get("task_ids")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                Some(Self::AwaitAgents { task_ids })
            }
            "poll_agents" => {
                let task_ids = value
                    .get("task_ids")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                let include_progress = value
                    .get("include_progress")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                Some(Self::PollAgents {
                    task_ids,
                    include_progress,
                })
            }
            "cancel_agent" => {
                let task_id = value.get("task_id")?.as_str()?.to_string();
                let reason = value
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Cancelled by parent agent")
                    .to_string();
                Some(Self::CancelAgent { task_id, reason })
            }
            "spawn_async" => {
                let target_agent = value.get("target_agent")?.as_str()?.to_string();
                let prompt = value.get("task")?.as_str()?.to_string();
                let priority = value.get("priority").and_then(|v| v.as_u64()).unwrap_or(5) as u8;
                Some(Self::SpawnAsync {
                    target_agent,
                    prompt,
                    priority,
                })
            }
            "a2a_delegate" => {
                let agent_url = value.get("agent_url")?.as_str()?.to_string();
                let capability = value.get("capability")?.as_str()?.to_string();
                let input = value.get("input").cloned().unwrap_or(serde_json::json!({}));
                let token = value
                    .get("token")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let wait_for_result = value
                    .get("wait_for_result")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                Some(Self::A2ADelegate {
                    agent_url,
                    capability,
                    input,
                    token,
                    wait_for_result,
                })
            }
            "event_subscribe" => {
                let event_filter = value.get("event_filter")?.as_str()?.to_string();
                let payload_filter = value
                    .get("payload_filter")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let throttle = value
                    .get("throttle")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let priority = value
                    .get("priority")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                Some(Self::EventSubscribeAction {
                    event_filter,
                    payload_filter,
                    throttle,
                    priority,
                })
            }
            "event_unsubscribe" => {
                let subscription_id = value.get("subscription_id")?.as_str()?.to_string();
                Some(Self::EventUnsubscribeAction { subscription_id })
            }
            "event_list_subscriptions" => Some(Self::EventListSubscriptionsAction),
            "event_list_available" => Some(Self::EventListAvailableAction),
            "set_timer" => {
                let name = value.get("name")?.as_str()?.to_string();
                let delay_secs = value.get("delay_secs")?.as_u64()?;
                let agent_name = value
                    .get("agent_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let action: TimerAction = match serde_json::from_value(value.get("action")?.clone())
                {
                    Ok(a) => a,
                    Err(e) => {
                        tracing::warn!(error = %e, "set_timer: failed to deserialize TimerAction");
                        return None;
                    }
                };
                Some(Self::SetTimer {
                    name,
                    delay_secs,
                    agent_name,
                    action,
                })
            }
            "cancel_timer" => {
                let name = value.get("name")?.as_str()?.to_string();
                Some(Self::CancelTimer { name })
            }
            "list_timers" => Some(Self::ListTimers),
            "schedule_once" => {
                use agentos_types::schedule::OnceJobAction;
                let name = value.get("name")?.as_str()?.to_string();
                let agent_name = value
                    .get("agent_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let fire_at: chrono::DateTime<chrono::Utc> = match serde_json::from_value(
                    value.get("fire_at")?.clone(),
                ) {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::warn!(error = %e, "schedule_once: failed to deserialize fire_at");
                        return None;
                    }
                };
                let mode = value.get("mode").and_then(|v| v.as_str()).unwrap_or("task");
                let action = match mode {
                    "notify" => {
                        let subject = value.get("notify_subject")?.as_str()?.to_string();
                        let body = value.get("notify_body")?.as_str()?.to_string();
                        let priority = value
                            .get("notify_priority")
                            .and_then(|v| v.as_str())
                            .unwrap_or("info")
                            .to_string();
                        OnceJobAction::NotifyUser {
                            subject,
                            body,
                            priority,
                        }
                    }
                    "tool" => {
                        let tool = value.get("tool")?.as_str()?.to_string();
                        let args = value
                            .get("tool_args")
                            .cloned()
                            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                        OnceJobAction::RunTool { tool, args }
                    }
                    _ => {
                        let prompt = value.get("task_prompt")?.as_str()?.to_string();
                        OnceJobAction::RunTask { prompt }
                    }
                };
                Some(Self::ScheduleOnce {
                    name,
                    action,
                    agent_name,
                    fire_at,
                })
            }
            "cancel_once_job" => {
                let name = value.get("name")?.as_str()?.to_string();
                Some(Self::CancelOnceJob { name })
            }
            "list_once_jobs" => Some(Self::ListOnceJobs),
            "get_schedule_runs" => {
                let schedule_id = value.get("schedule_id")?.as_str()?.to_string();
                let limit = value
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(20)
                    .clamp(1, 100) as u32;
                let state_filter = value
                    .get("state")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                Some(Self::GetScheduleRuns {
                    schedule_id,
                    limit,
                    state_filter,
                })
            }
            "list_my_schedules" => {
                let kinds = value
                    .get("kinds")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let include_inactive = value
                    .get("include_inactive")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                Some(Self::ListMySchedules {
                    kinds,
                    include_inactive,
                })
            }
            "get_task_logs" => {
                let run_id = value.get("run_id")?.as_str()?.to_string();
                Some(Self::GetTaskLogs { run_id })
            }
            "create_schedule" => {
                let name = value.get("name")?.as_str()?.to_string();
                let cron = value.get("cron")?.as_str()?.to_string();
                let agent_name = value
                    .get("agent_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let mode = value
                    .get("mode")
                    .and_then(|v| v.as_str())
                    .unwrap_or("task")
                    .to_string();
                Some(Self::CreateSchedule {
                    name,
                    cron,
                    agent_name,
                    mode,
                    task_prompt: value
                        .get("task_prompt")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    notify_subject: value
                        .get("notify_subject")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    notify_body: value
                        .get("notify_body")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    notify_priority: value
                        .get("notify_priority")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    tool: value
                        .get("tool")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    tool_args: value.get("tool_args").cloned(),
                })
            }
            "control_schedule" => {
                let action = value.get("action")?.as_str()?.to_string();
                let name = value.get("name")?.as_str()?.to_string();
                Some(Self::ControlSchedule { action, name })
            }
            other => {
                tracing::warn!(action = %other, "Unknown _kernel_action, ignoring");
                None
            }
        }
    }
}

impl Kernel {
    /// Execute a kernel action on behalf of a running task.
    ///
    /// This is the central dispatch point for all tool-initiated kernel
    /// operations. It enforces permissions via the existing capability
    /// system and produces full audit trails.
    pub(crate) async fn dispatch_kernel_action(
        &self,
        task: &AgentTask,
        action: KernelAction,
        trace_id: TraceID,
    ) -> KernelActionResult {
        let action_name = action.name();

        self.audit_log(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id,
            event_type: agentos_audit::AuditEventType::ToolExecutionStarted,
            agent_id: Some(task.agent_id),
            task_id: Some(task.id),
            tool_id: None,
            details: serde_json::json!({ "kernel_action": action_name }),
            severity: agentos_audit::AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        });

        let result = match action {
            KernelAction::DelegateTask {
                target_agent,
                prompt,
                priority,
            } => {
                self.execute_delegate_task(task, &target_agent, &prompt, priority)
                    .await
            }
            KernelAction::SendAgentMessage { to, content } => {
                self.execute_send_message(task, &to, &content, trace_id)
                    .await
            }
            KernelAction::AgentInboxList { limit, unread_only } => {
                self.execute_agent_inbox_list(task, limit, unread_only)
                    .await
            }
            KernelAction::AgentInboxRead { id } => self.execute_agent_inbox_read(task, &id).await,
            KernelAction::AgentInboxDismiss { id } => {
                self.execute_agent_inbox_dismiss(task, &id).await
            }
            KernelAction::AgentMessagesList { limit, unread_only } => {
                self.execute_agent_messages_list(task, limit, unread_only)
                    .await
            }
            KernelAction::AgentMessagesRead { id } => {
                self.execute_agent_messages_read(task, &id).await
            }
            KernelAction::AgentMessagesDismiss { id } => {
                self.execute_agent_messages_dismiss(task, &id).await
            }
            KernelAction::EscalateToHuman {
                reason,
                context_summary,
                decision_point,
                options,
                urgency,
                blocking,
            } => {
                self.execute_escalation(
                    task,
                    reason,
                    &context_summary,
                    &decision_point,
                    &options,
                    &urgency,
                    blocking,
                    trace_id,
                )
                .await
            }
            KernelAction::SwitchPartition { partition } => {
                self.execute_switch_partition(task, &partition).await
            }
            KernelAction::RunProcedure {
                procedure,
                inputs,
                detach,
            } => {
                self.execute_run_procedure(task, &procedure, &inputs, detach, trace_id)
                    .await
            }
            KernelAction::MemoryBlockWrite { label, content } => {
                self.execute_memory_block_write(task, &label, &content)
                    .await
            }
            KernelAction::MemoryBlockRead { label } => {
                self.execute_memory_block_read(task, &label).await
            }
            KernelAction::MemoryBlockList => self.execute_memory_block_list(task).await,
            KernelAction::MemoryBlockDelete { label } => {
                self.execute_memory_block_delete(task, &label).await
            }
            KernelAction::NotifyUser {
                subject,
                body,
                priority,
                channels,
            } => {
                self.execute_notify_user(task, subject, body, priority, channels, trace_id)
                    .await
            }
            KernelAction::AskUser {
                question,
                options,
                timeout_secs,
                priority,
                auto_action,
            } => {
                self.execute_ask_user(
                    task,
                    question,
                    options,
                    timeout_secs,
                    priority,
                    auto_action,
                    trace_id,
                )
                .await
            }
            KernelAction::WorkspaceRequest {
                path,
                mode,
                reason,
                timeout_secs,
            } => {
                self.execute_workspace_request(task, path, mode, reason, timeout_secs, trace_id)
                    .await
            }
            KernelAction::AgentRpcCall {
                target_agent,
                prompt,
                timeout_secs,
            } => {
                self.execute_agent_rpc_call(task, &target_agent, &prompt, timeout_secs, trace_id)
                    .await
            }
            KernelAction::ContextMemoryUpdate { content, reason } => {
                // Bound to the kernel's own identity for this call. Taking the
                // target from the tool-result envelope would let any tool that
                // can be induced to echo attacker-shaped JSON rewrite ANOTHER
                // agent's standing prompt.
                let agent_id = task.agent_id.to_string();
                // Injection scanning (spec §9)
                let scan = self.injection_scanner.scan(&content);
                if scan.max_threat == Some(crate::injection_scanner::ThreatLevel::High) {
                    self.audit_log(AuditEntry {
                        timestamp: Utc::now(),
                        trace_id,
                        event_type: AuditEventType::RiskEscalation,
                        agent_id: Some(task.agent_id),
                        task_id: Some(task.id),
                        tool_id: None,
                        details: serde_json::json!({
                            "source": "context_memory_update",
                            "threat": "high",
                            "agent_id": agent_id,
                        }),
                        severity: AuditSeverity::Security,
                        reversible: false,
                        rollback_ref: None,
                    });
                    return KernelActionResult {
                        success: false,
                        result: serde_json::json!({
                            "error": "Content rejected: high-confidence injection pattern detected.",
                        }),
                    };
                }

                match self
                    .context_memory_store
                    .write(&agent_id, &content, reason.as_deref())
                    .await
                {
                    Ok(entry) => {
                        self.audit_log(AuditEntry {
                            timestamp: Utc::now(),
                            trace_id,
                            event_type: AuditEventType::ContextMemoryUpdated,
                            agent_id: Some(task.agent_id),
                            task_id: Some(task.id),
                            tool_id: None,
                            details: serde_json::json!({
                                "agent_id": entry.agent_id,
                                "version": entry.version,
                                "token_count": entry.token_count,
                                "reason": reason,
                            }),
                            severity: AuditSeverity::Info,
                            reversible: true,
                            rollback_ref: Some(format!(
                                "context_memory:{}:{}",
                                entry.agent_id,
                                entry.version.saturating_sub(1)
                            )),
                        });
                        KernelActionResult {
                            success: true,
                            result: serde_json::json!({
                                "updated": true,
                                "version": entry.version,
                                "token_count": entry.token_count,
                                "message": "Context memory updated. Changes take effect on your next task.",
                            }),
                        }
                    }
                    Err(e) => KernelActionResult {
                        success: false,
                        result: serde_json::json!({
                            "error": e.to_string(),
                        }),
                    },
                }
            }
            KernelAction::ContextMemoryRead => {
                // Read own document only — see `ContextMemoryUpdate` above.
                let agent_id = task.agent_id.to_string();
                match self.context_memory_store.read(&agent_id).await {
                    Ok(Some(entry)) => KernelActionResult {
                        success: true,
                        result: serde_json::json!({
                            "content": entry.content,
                            "version": entry.version,
                            "token_count": entry.token_count,
                            "updated_at": entry.updated_at.to_rfc3339(),
                        }),
                    },
                    Ok(None) => KernelActionResult {
                        success: true,
                        result: serde_json::json!({
                            "content": "",
                            "version": 0,
                            "token_count": 0,
                            "message": "No context memory set yet. Use context-memory-update to create one.",
                        }),
                    },
                    Err(e) => KernelActionResult {
                        success: false,
                        result: serde_json::json!({
                            "error": e.to_string(),
                        }),
                    },
                }
            }
            KernelAction::ChatSearch { query, limit } => {
                // Scope to the calling agent's own sessions, resolved from the
                // kernel's own identity for this call — see the note on
                // `ContextMemoryUpdate`.
                let agent_name = {
                    let registry = self.agent_registry.read().await;
                    registry.get_by_id(&task.agent_id).map(|p| p.name.clone())
                };
                let Some(agent_name) = agent_name else {
                    return KernelActionResult {
                        success: false,
                        result: serde_json::json!({
                            "error": "chat-search: calling agent is not registered",
                        }),
                    };
                };
                let store = Arc::clone(&self.chat_store);
                let hits = tokio::task::spawn_blocking(move || {
                    store.search(&query, Some(&agent_name), limit)
                })
                .await;
                match hits {
                    Ok(Ok(hits)) => KernelActionResult {
                        success: true,
                        result: serde_json::json!({
                            "count": hits.len(),
                            "results": hits,
                        }),
                    },
                    Ok(Err(e)) => KernelActionResult {
                        success: false,
                        result: serde_json::json!({ "error": e.to_string() }),
                    },
                    Err(e) => KernelActionResult {
                        success: false,
                        result: serde_json::json!({ "error": format!("chat-search task failed: {e}") }),
                    },
                }
            }
            KernelAction::SpawnAgent {
                agent,
                prompt,
                permissions,
                context_messages,
            } => {
                // Build a context slice from the parent task's current context window.
                let slice = self
                    .context_manager
                    .get_slice(
                        &task.id,
                        context_messages as usize,
                        format!("from-parent-{}", task.id),
                    )
                    .await;

                let response = self
                    .cmd_spawn_sub_agent(
                        task.id,
                        &agent,
                        &prompt,
                        &permissions,
                        slice,
                        // No `handoff_mode` here — caller provided an explicit slice
                        // via `last_n` above, which always wins over the mode arg.
                        None,
                        // Inherit parent's allowlist (no per-call narrowing in this path).
                        task.tool_categories.clone(),
                    )
                    .await;

                match response {
                    agentos_bus::KernelResponse::SubAgentSpawned { child_task_id } => {
                        KernelActionResult {
                            success: true,
                            result: serde_json::json!({
                                "task_id": child_task_id.to_string(),
                                "agent": agent,
                                "status": "spawned",
                                "message": format!(
                                    "Sub-agent '{}' spawned as task {}. Use await-agents to wait for the result.",
                                    agent, child_task_id
                                ),
                            }),
                        }
                    }
                    agentos_bus::KernelResponse::Error { message } => KernelActionResult {
                        success: false,
                        result: serde_json::json!({ "error": message }),
                    },
                    _ => KernelActionResult {
                        success: false,
                        result: serde_json::json!({ "error": "unexpected response from spawn" }),
                    },
                }
            }
            KernelAction::AwaitAgents { task_ids } => {
                // Parse task IDs and query their current state.
                let mut parsed_ids = Vec::with_capacity(task_ids.len());
                for id_str in &task_ids {
                    match id_str.parse::<agentos_types::TaskID>() {
                        Ok(id) => parsed_ids.push(id),
                        Err(_) => {
                            return KernelActionResult {
                                success: false,
                                result: serde_json::json!({
                                    "error": format!("invalid task_id: {}", id_str)
                                }),
                            };
                        }
                    }
                }

                let response = self.cmd_await_sub_agents(task.id, &parsed_ids).await;

                match response {
                    agentos_bus::KernelResponse::SubAgentResults { results } => {
                        let results_json: Vec<serde_json::Value> = results
                            .iter()
                            .map(|(id, summary)| {
                                serde_json::json!({
                                    "task_id": id.to_string(),
                                    "summary": summary,
                                })
                            })
                            .collect();
                        KernelActionResult {
                            success: true,
                            result: serde_json::json!({ "results": results_json }),
                        }
                    }
                    agentos_bus::KernelResponse::Error { message } => KernelActionResult {
                        success: false,
                        result: serde_json::json!({ "error": message }),
                    },
                    _ => KernelActionResult {
                        success: false,
                        result: serde_json::json!({ "error": "unexpected response from await" }),
                    },
                }
            }
            KernelAction::PollAgents {
                task_ids,
                include_progress,
            } => {
                // Cap the number of task IDs to bound per-call scheduler work.
                if task_ids.len() > 50 {
                    return KernelActionResult {
                        success: false,
                        result: serde_json::json!({
                            "error": format!(
                                "poll_agents supports at most 50 task_ids per call (got {})",
                                task_ids.len()
                            )
                        }),
                    };
                }
                let mut results = Vec::with_capacity(task_ids.len());
                for id_str in &task_ids {
                    match id_str.parse::<agentos_types::TaskID>() {
                        Ok(tid) => {
                            if let Some(child_task) = self.scheduler.get_task(&tid).await {
                                // Verify the caller is the parent
                                if child_task.parent_task_id != Some(task.id) {
                                    results.push(serde_json::json!({
                                        "task_id": id_str,
                                        "error": "not parent of this task"
                                    }));
                                    continue;
                                }
                                let state_label = format!("{:?}", child_task.state);
                                let mut entry = serde_json::json!({
                                    "task_id": id_str,
                                    "state": state_label,
                                    "spawn_depth": child_task.spawn_depth,
                                });
                                if include_progress {
                                    // Include last few history messages as progress
                                    let history_len = child_task.history.len();
                                    let recent: Vec<String> = child_task
                                        .history
                                        .iter()
                                        .rev()
                                        .take(3)
                                        .filter_map(|m| {
                                            m.payload
                                                .data
                                                .get("content")
                                                .and_then(|v| v.as_str())
                                                .map(|content: &str| {
                                                    let truncated: String =
                                                        content.chars().take(200).collect();
                                                    if truncated.len() < content.chars().count() {
                                                        format!("{}...", truncated)
                                                    } else {
                                                        content.to_string()
                                                    }
                                                })
                                        })
                                        .collect();
                                    if let Some(obj) = entry.as_object_mut() {
                                        obj.insert(
                                            "iterations_approx".into(),
                                            serde_json::json!(history_len / 2),
                                        );
                                        obj.insert(
                                            "recent_messages".into(),
                                            serde_json::json!(recent),
                                        );
                                    }
                                }
                                results.push(entry);
                            } else {
                                results.push(serde_json::json!({
                                    "task_id": id_str,
                                    "error": "task not found"
                                }));
                            }
                        }
                        Err(_) => {
                            results.push(serde_json::json!({
                                "task_id": id_str,
                                "error": "invalid task_id"
                            }));
                        }
                    }
                }
                KernelActionResult {
                    success: true,
                    result: serde_json::json!({ "results": results }),
                }
            }
            KernelAction::CancelAgent { task_id, reason } => {
                match task_id.parse::<agentos_types::TaskID>() {
                    Ok(tid) => {
                        // Verify the caller is the parent
                        let is_parent = self
                            .scheduler
                            .get_task(&tid)
                            .await
                            .map(|t| t.parent_task_id == Some(task.id))
                            .unwrap_or(false);
                        if !is_parent {
                            return KernelActionResult {
                                success: false,
                                result: serde_json::json!({
                                    "error": "not parent of this task — cannot cancel"
                                }),
                            };
                        }

                        // Cancel the task (cascades to children via existing logic)
                        let response = self.cmd_cancel_task(tid).await;
                        let cancelled =
                            matches!(response, agentos_bus::KernelResponse::Success { .. });
                        self.audit_log(AuditEntry {
                            timestamp: Utc::now(),
                            trace_id,
                            event_type: AuditEventType::TaskStateChanged,
                            agent_id: Some(task.agent_id),
                            task_id: Some(tid),
                            tool_id: None,
                            details: serde_json::json!({
                                "action": "cancel_agent",
                                "new_state": if cancelled { "cancelled" } else { "unchanged" },
                                "reason": reason,
                                "cancelled_by": task.id.to_string(),
                            }),
                            severity: AuditSeverity::Info,
                            reversible: false,
                            rollback_ref: None,
                        });
                        match response {
                            agentos_bus::KernelResponse::Success { .. } => KernelActionResult {
                                success: true,
                                result: serde_json::json!({
                                    "cancelled": true,
                                    "task_id": task_id,
                                    "reason": reason,
                                }),
                            },
                            agentos_bus::KernelResponse::Error { message } => KernelActionResult {
                                success: false,
                                result: serde_json::json!({ "error": message }),
                            },
                            _ => KernelActionResult {
                                success: false,
                                result: serde_json::json!({
                                    "error": "unexpected response from cancel"
                                }),
                            },
                        }
                    }
                    Err(_) => KernelActionResult {
                        success: false,
                        result: serde_json::json!({
                            "error": format!("invalid task_id: {}", task_id)
                        }),
                    },
                }
            }
            KernelAction::SpawnAsync {
                target_agent,
                prompt,
                priority,
            } => {
                self.execute_spawn_async(task, &target_agent, &prompt, priority)
                    .await
            }
            KernelAction::A2ADelegate {
                agent_url,
                capability,
                input,
                token,
                wait_for_result,
            } => {
                // SSRF protection: resolve the hostname and reject private/internal addresses.
                if let Some(ssrf_err) = check_a2a_url_ssrf(&agent_url).await {
                    return KernelActionResult {
                        success: false,
                        result: serde_json::json!({
                            "error": ssrf_err,
                            "agent_url": agent_url,
                        }),
                    };
                }

                let mut client = agentos_mcp::a2a::A2AClient::new(&agent_url);
                if let Some(ref t) = token {
                    client = client.with_token(t);
                }

                let sender_url = format!("agentos://agent/{}", task.agent_id);

                match client
                    .submit_task(&capability, input.clone(), &sender_url)
                    .await
                {
                    Ok(task_id) => {
                        if !wait_for_result {
                            KernelActionResult {
                                success: true,
                                result: serde_json::json!({
                                    "task_id": task_id,
                                    "agent_url": agent_url,
                                    "capability": capability,
                                    "status": "submitted",
                                }),
                            }
                        } else {
                            // Poll until terminal with 5-minute timeout
                            let deadline =
                                std::time::Instant::now() + std::time::Duration::from_secs(300);
                            loop {
                                if std::time::Instant::now() > deadline {
                                    break KernelActionResult {
                                        success: false,
                                        result: serde_json::json!({
                                            "error": "A2A task timed out after 300s",
                                            "task_id": task_id,
                                        }),
                                    };
                                }
                                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                                match client.poll_task(&task_id).await {
                                    Ok(a2a_task) if a2a_task.is_terminal() => {
                                        break KernelActionResult {
                                            success: matches!(
                                                a2a_task.status,
                                                agentos_mcp::a2a::A2ATaskStatus::Completed { .. }
                                            ),
                                            result: serde_json::to_value(&a2a_task)
                                                .unwrap_or(serde_json::json!({})),
                                        };
                                    }
                                    Ok(_) => continue,
                                    Err(e) => {
                                        break KernelActionResult {
                                            success: false,
                                            result: serde_json::json!({
                                                "error": format!("Poll failed: {}", e),
                                                "task_id": task_id,
                                            }),
                                        };
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => KernelActionResult {
                        success: false,
                        result: serde_json::json!({
                            "error": format!("A2A delegation failed: {}", e),
                            "agent_url": agent_url,
                            "capability": capability,
                        }),
                    },
                }
            }
            KernelAction::EventSubscribeAction {
                event_filter,
                payload_filter,
                throttle,
                priority,
            } => {
                self.execute_event_subscribe(
                    task,
                    event_filter,
                    payload_filter,
                    throttle,
                    priority,
                    trace_id,
                )
                .await
            }
            KernelAction::EventUnsubscribeAction { subscription_id } => {
                self.execute_event_unsubscribe(task, subscription_id, trace_id)
                    .await
            }
            KernelAction::EventListSubscriptionsAction => {
                self.execute_event_list_subscriptions(task).await
            }
            KernelAction::EventListAvailableAction => self.execute_event_list_available(task).await,
            KernelAction::SetTimer {
                name,
                delay_secs,
                agent_name,
                action,
            } => {
                self.execute_set_timer(task, name, delay_secs, agent_name, action)
                    .await
            }
            KernelAction::CancelTimer { name } => self.execute_cancel_timer(task, name).await,
            KernelAction::ListTimers => self.execute_list_timers(task).await,
            KernelAction::ScheduleOnce {
                name,
                action,
                agent_name,
                fire_at,
            } => {
                self.execute_schedule_once(task, name, action, agent_name, fire_at)
                    .await
            }
            KernelAction::CancelOnceJob { name } => self.execute_cancel_once_job(task, name).await,
            KernelAction::ListOnceJobs => self.execute_list_once_jobs(task).await,
            KernelAction::GetScheduleRuns {
                schedule_id,
                limit,
                state_filter,
            } => {
                self.execute_get_schedule_runs(task, schedule_id, limit, state_filter)
                    .await
            }
            KernelAction::ListMySchedules {
                kinds,
                include_inactive,
            } => {
                self.execute_list_my_schedules(task, kinds, include_inactive)
                    .await
            }
            KernelAction::GetTaskLogs { run_id } => self.execute_get_task_logs(task, run_id).await,
            KernelAction::CreateSchedule {
                name,
                cron,
                agent_name,
                mode,
                task_prompt,
                notify_subject,
                notify_body,
                notify_priority,
                tool,
                tool_args,
            } => {
                self.execute_create_schedule(
                    task,
                    name,
                    cron,
                    agent_name,
                    mode,
                    task_prompt,
                    notify_subject,
                    notify_body,
                    notify_priority,
                    tool,
                    tool_args,
                )
                .await
            }
            KernelAction::ControlSchedule { action, name } => {
                self.execute_control_schedule(task, action, name).await
            }
            KernelAction::ChannelSend {
                channel,
                text,
                thread_id,
                attachment,
                file_id,
                file_path,
                caption,
                filename,
            } => {
                self.execute_channel_send(
                    task, channel, text, thread_id, attachment, file_id, file_path, caption,
                    filename, trace_id,
                )
                .await
            }
        };

        let severity = if result.success {
            agentos_audit::AuditSeverity::Info
        } else {
            agentos_audit::AuditSeverity::Error
        };

        self.audit_log(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id,
            event_type: agentos_audit::AuditEventType::ToolExecutionCompleted,
            agent_id: Some(task.agent_id),
            task_id: Some(task.id),
            tool_id: None,
            details: serde_json::json!({
                "kernel_action": action_name,
                "success": result.success,
            }),
            severity,
            reversible: false,
            rollback_ref: None,
        });

        result
    }

    async fn execute_spawn_async(
        &self,
        task: &AgentTask,
        target_agent: &str,
        prompt: &str,
        priority: u8,
    ) -> KernelActionResult {
        let timeout_secs = self.config.kernel.default_task_timeout_secs;
        match self
            .handle_spawn_async(task, target_agent, prompt, priority, timeout_secs)
            .await
        {
            Ok(value) => KernelActionResult {
                success: true,
                result: value,
            },
            Err(e) => KernelActionResult {
                success: false,
                result: serde_json::json!({ "error": e.to_string() }),
            },
        }
    }

    async fn execute_delegate_task(
        &self,
        task: &AgentTask,
        target_agent: &str,
        prompt: &str,
        priority: u8,
    ) -> KernelActionResult {
        let timeout_secs = self.config.kernel.default_task_timeout_secs;
        match self
            .handle_task_delegation(task, target_agent, prompt, priority, timeout_secs)
            .await
        {
            Ok(value) => KernelActionResult {
                success: true,
                result: value,
            },
            Err(e) => KernelActionResult {
                success: false,
                result: serde_json::json!({ "error": e.to_string() }),
            },
        }
    }

    /// Close every DM session whose clock has run out, asking the operator
    /// first when `dm_expiry_escalation` is on.
    ///
    /// A DM session ends on exactly two things: its turns are completed (the
    /// runner's own ceiling, status `complete`), or its clock runs out — this.
    /// Only `running` rows are swept; a finished session's lapsed deadline is
    /// enforced passively by `find_or_create_dm`, which will not reuse it.
    pub async fn sweep_dm_session_expiry(&self) {
        let store = Arc::clone(&self.convo_store);
        let expired =
            match tokio::task::spawn_blocking(move || store.expired_running_dm_sessions()).await {
                Ok(Ok(v)) => v,
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "DM session expiry sweep failed");
                    return;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "spawn_blocking panicked in the DM expiry sweep");
                    return;
                }
            };

        for (convo_id, participants) in expired {
            if !self.config.kernel.convo.dm_expiry_escalation {
                self.close_dm_session(&convo_id).await;
                continue;
            }

            // Hold the session open while the question is pending, so the next
            // sweep cannot raise a second card for the same session. If the
            // kernel dies mid-question the deadline simply lapses again and the
            // next boot's sweep re-asks — no state to reconcile.
            {
                let store = Arc::clone(&self.convo_store);
                let id = convo_id.clone();
                let hold = crate::escalation::DEFAULT_ESCALATION_TIMEOUT_SECS as u64 + 60;
                let _ = tokio::task::spawn_blocking(move || store.extend_dm(&id, hold)).await;
            }

            let kernel = {
                let slot = self.self_weak.lock().unwrap_or_else(|e| e.into_inner());
                slot.as_ref().and_then(|w| w.upgrade())
            };
            let Some(kernel) = kernel else { continue };
            // Spawned: the sweep must not hold up the TimeoutChecker for the
            // five minutes this question can stay open.
            tokio::spawn(async move {
                kernel.ask_to_extend_dm(convo_id, participants).await;
            });
        }
    }

    /// Ask whether a lapsed session should keep running. Extends on approval,
    /// closes on anything else.
    async fn ask_to_extend_dm(&self, convo_id: String, participants: Vec<String>) {
        let peers = participants.join(" and ");
        let turns = {
            let store = Arc::clone(&self.convo_store);
            let id = convo_id.clone();
            tokio::task::spawn_blocking(move || store.get_turns(&id))
                .await
                .ok()
                .and_then(|r| r.ok())
                .map(|t| {
                    t.iter()
                        .filter(|t| t.agent_name != crate::convo_store::USER_SPEAKER)
                        .count()
                })
                .unwrap_or(0)
        };
        let agent_id = {
            let registry = self.agent_registry.read().await;
            participants
                .first()
                .and_then(|n| registry.get_by_name(n))
                .map(|a| a.id)
        };
        let Some(agent_id) = agent_id else {
            self.close_dm_session(&convo_id).await;
            return;
        };

        // `blocking: false` with a synthetic task id: nothing is parked on this
        // question. `EscalationManager::resolve` fires the resolution channel
        // for every escalation regardless of the flag, while the expiry path in
        // `run_loop` only touches the task when `blocking` is true — so a
        // blocking card here would make the sweeper try to resume a task that
        // does not exist.
        let (esc_id, rx) = self
            .escalation_manager
            .create_escalation_with_resolution(
                TaskID::new(),
                agent_id,
                EscalationReason::Other("dm_session_expired".to_string()),
                format!(
                    "The conversation between {peers} has been open for its full time \
                     limit and is still going ({turns} turns so far)."
                ),
                "Extend this conversation, or let it close?".to_string(),
                vec![
                    "Extend — give it another full session".to_string(),
                    "Close — the agents keep the history, not the live thread".to_string(),
                ],
                // Housekeeping, not an incident: it must not out-rank a real
                // approval card in the operator's queue.
                "low".to_string(),
                false,
                TraceID::new(),
                Some(crate::escalation::AutoAction::Deny),
            )
            .await;

        // Above the escalation's own 300s timeout, the same margin
        // `APPROVAL_WAIT_TIMEOUT_SECS` uses, so the sweep resolves the card
        // first and this waiter never leaves a live Approve button behind.
        const DM_EXTEND_WAIT_SECS: u64 = 360;
        let approved = match rx {
            Some(rx) => matches!(
                tokio::time::timeout(Duration::from_secs(DM_EXTEND_WAIT_SECS), rx).await,
                Ok(Ok(crate::escalation::ResolutionOutcome::Approved))
            ),
            // Cap reached (esc_id == u64::MAX) or no channel installed.
            None => false,
        };
        tracing::info!(
            convo_id,
            escalation_id = esc_id,
            approved,
            "DM session expiry decided"
        );

        if approved {
            let ttl = self.config.kernel.convo.dm_session_ttl_secs;
            let store = Arc::clone(&self.convo_store);
            let id = convo_id.clone();
            let _ = tokio::task::spawn_blocking(move || {
                let _ = store.extend_dm(&id, ttl);
                store.add_turn(
                    &id,
                    crate::convo_store::USER_SPEAKER,
                    "_[operator extended this session]_",
                    0,
                )
            })
            .await;
        } else {
            self.close_dm_session(&convo_id).await;
        }
    }

    /// End a session on the clock.
    ///
    /// `status = 'stopped'` is what `run_convo` already checks before each turn
    /// and again after the in-flight one, so a running loop exits by itself
    /// after finishing the turn it is on — the same path the operator's `/stop`
    /// takes. The closing note is an operator row, so it does not count toward
    /// `max_turns` and the transcript explains its own ending.
    async fn close_dm_session(&self, convo_id: &str) {
        let store = Arc::clone(&self.convo_store);
        let id = convo_id.to_string();
        let _ = tokio::task::spawn_blocking(move || {
            let _ = store.add_turn(
                &id,
                crate::convo_store::USER_SPEAKER,
                "_[session closed — time limit reached]_",
                0,
            );
            store.set_status(&id, "stopped")
        })
        .await;
        tracing::info!(convo_id, "DM session closed on its time limit");
    }

    /// Append `content` to this pair's current DM session and make sure a
    /// runner is working it. Returns the session id for the event payload, or
    /// `None` when no session could be opened (logged; the durable inbox row
    /// still stands).
    ///
    /// Three outcomes, all normal:
    ///  * new session  → `find_or_create_dm` already wrote `running`; spawn.
    ///  * reused, idle → `claim_resume` flips it back to `running` and raises the
    ///    ceiling by `dm_max_turns`; spawn.
    ///  * reused, live → `Busy`; the running loop re-reads the transcript every
    ///    turn, so it picks this row up by itself. Do NOT spawn.
    ///
    /// Never waits on an operator. Closing a lapsed session (and asking first)
    /// is the expiry sweep's job, not this path's.
    pub(crate) async fn append_dm_turn(
        &self,
        from_name: &str,
        to_name: &str,
        content: &str,
    ) -> Option<String> {
        let max_turns = self.config.kernel.convo.dm_max_turns;
        let ttl = self.config.kernel.convo.dm_session_ttl_secs;
        let store = Arc::clone(&self.convo_store);

        let (convo_id, created) = {
            let store = Arc::clone(&store);
            let (a, b) = (from_name.to_string(), to_name.to_string());
            match tokio::task::spawn_blocking(move || {
                store.find_or_create_dm(&a, &b, max_turns, ttl)
            })
            .await
            {
                Ok(Ok(v)) => v,
                Ok(Err(e)) => {
                    tracing::warn!(from = from_name, to = to_name, error = %e, "Could not open a DM session");
                    return None;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "spawn_blocking panicked opening a DM session");
                    return None;
                }
            }
        };

        // Written before the spawn decision, so a live runner sees the row on
        // its next pass. It does not move the session deadline — only an
        // operator extension does.
        {
            let store = Arc::clone(&store);
            let (id, name, body) = (convo_id.clone(), from_name.to_string(), content.to_string());
            match tokio::task::spawn_blocking(move || store.add_turn(&id, &name, &body, 0)).await {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    tracing::error!(convo_id, error = %e, "Failed to persist the DM turn");
                    return None;
                }
                Err(e) => {
                    tracing::error!(convo_id, error = %e, "spawn_blocking panicked persisting the DM turn");
                    return None;
                }
            }
        }

        let spawn = if created {
            true
        } else {
            let store = Arc::clone(&store);
            let id = convo_id.clone();
            let claimed = tokio::task::spawn_blocking(move || {
                match store.claim_resume(&id, max_turns) {
                    Ok(_) => true,
                    Err(crate::convo_store::ResumeError::Busy) if store.is_live(&id) => false,
                    Err(crate::convo_store::ResumeError::Busy) => {
                        // The row says `running` but nothing in this process is
                        // running it — a runner future dropped mid-turn without
                        // its `RunGuard` cleanup. Settle it and claim once, or
                        // the session wedges and every later message lands in a
                        // transcript nobody reads.
                        let _ = store.set_status(&id, "error");
                        store.claim_resume(&id, max_turns).is_ok()
                    }
                    Err(e) => {
                        tracing::warn!(convo_id = %id, error = ?e, "DM session could not be reopened");
                        false
                    }
                }
            })
            .await;
            claimed.unwrap_or(false)
        };

        if spawn {
            self.spawn_convo_runner(convo_id.clone()).await;
        }
        Some(convo_id)
    }

    /// Ask the convo-runner pump to start this conversation's turn loop.
    ///
    /// Posting an id rather than spawning here is deliberate: the runner calls
    /// back into tool dispatch, which can reach `append_dm_turn` again, and
    /// rustc cannot compute `Send` through that cycle. The pump
    /// (`wire_inbound_chat_bridge`) sits outside it.
    pub(crate) async fn spawn_convo_runner(&self, convo_id: String) {
        if let Err(e) = self.convo_run_tx.try_send(convo_id) {
            // Full or closed: the transcript still holds the turn, so the next
            // message (or an operator Continue) picks the conversation up.
            tracing::warn!(error = %e, "Convo runner queue unavailable — turn loop not started");
        }
    }

    async fn execute_send_message(
        &self,
        task: &AgentTask,
        to: &str,
        content: &str,
        trace_id: TraceID,
    ) -> KernelActionResult {
        let from_name = {
            let registry = self.agent_registry.read().await;
            match registry.get_by_id(&task.agent_id) {
                Some(agent) => agent.name.clone(),
                None => {
                    return KernelActionResult {
                        success: false,
                        result: serde_json::json!({
                            "error": format!("Sender agent '{}' not found", task.agent_id)
                        }),
                    };
                }
            }
        };

        let registry = self.agent_registry.read().await;
        let to_agent = match registry.get_by_name(to) {
            Some(a) => a.clone(),
            None => {
                // Fallback: try parsing as UUID for agents that use IDs instead of names
                match to.parse::<AgentID>() {
                    Ok(id) => match registry.get_by_id(&id) {
                        Some(a) => a.clone(),
                        None => {
                            return KernelActionResult {
                                success: false,
                                result: serde_json::json!({
                                    "error": format!("Target agent '{}' not found", to)
                                }),
                            };
                        }
                    },
                    Err(_) => {
                        return KernelActionResult {
                            success: false,
                            result: serde_json::json!({
                                "error": format!("Target agent '{}' not found", to)
                            }),
                        };
                    }
                }
            }
        };
        drop(registry);

        // Durable row first; the bus message reuses its id so the
        // DirectMessageReceived `message_id` resolves via agent-messages-read.
        let entry_id = self
            .agent_inbox_writer
            .write_message(
                task.agent_id,
                from_name.clone(),
                to_agent.id,
                content.to_string(),
            )
            .await;

        let now = chrono::Utc::now();
        let ttl_seconds: u64 = 60;
        let mut msg = AgentMessage {
            id: MessageID::from_uuid(*entry_id.as_uuid()),
            from: task.agent_id,
            to: MessageTarget::Direct(to_agent.id),
            content: MessageContent::Text(content.to_string()),
            reply_to: None,
            timestamp: now,
            trace_id,
            signature: None,
            ttl_seconds,
            expires_at: Some(now + chrono::Duration::seconds(ttl_seconds as i64)),
        };

        // Sign the message with the sender's Ed25519 identity key (Spec §7).
        // Return early if signing fails so the unsigned message is never sent
        // (the bus would reject it anyway, but with a misleading error).
        let payload = msg.signing_payload();
        match self
            .identity_manager
            .sign_message(&task.agent_id, &payload)
            .await
        {
            Ok(sig_hex) => msg.signature = Some(sig_hex),
            Err(e) => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": format!("Agent has no identity key — message signing failed: {}", e)
                    }),
                };
            }
        }

        // An Offline recipient still gets the durable inbox row and reads it on
        // reconnect, but the sender must not be told "delivered" — it may be
        // waiting for a reply that cannot come. This is the only remaining
        // producer of AgentUnreachable.
        let recipient_online = to_agent.status != AgentStatus::Offline;
        if !recipient_online {
            self.emit_event_with_trace(
                EventType::AgentUnreachable,
                EventSource::AgentMessageBus,
                EventSeverity::Warning,
                serde_json::json!({
                    "unreachable_agent": to_agent.id.to_string(),
                    "unreachable_agent_name": to_agent.name,
                    "from_agent": task.agent_id.to_string(),
                    "reason": "offline",
                }),
                task.event_chain_depth(),
                Some(trace_id),
                Some(task.agent_id),
                Some(task.id),
            )
            .await;
        }

        // The thread must exist before the event is emitted: `event_dispatch`
        // keys its "do not spawn a one-shot reaction task" decision off the
        // `convo_id` in the payload. No thread is opened for an offline
        // recipient — a conversation nobody can answer should not look live.
        let convo_id = if recipient_online {
            self.append_dm_turn(&from_name, &to_agent.name, content)
                .await
        } else {
            None
        };

        match self
            .message_bus
            .send_direct(msg, task.event_chain_depth(), convo_id.as_deref())
            .await
        {
            Ok(_) => KernelActionResult {
                success: true,
                result: serde_json::json!({
                    "status": if recipient_online { "delivered" } else { "queued" },
                    "recipient_online": recipient_online,
                    "to": to,
                    "from": from_name,
                    "convo_id": convo_id,
                }),
            },
            Err(e) => KernelActionResult {
                success: false,
                result: serde_json::json!({ "error": e.to_string() }),
            },
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_escalation(
        &self,
        task: &AgentTask,
        reason: EscalationReason,
        context_summary: &str,
        decision_point: &str,
        options: &[String],
        urgency: &str,
        blocking: bool,
        trace_id: TraceID,
    ) -> KernelActionResult {
        let severity = match urgency {
            "critical" | "high" => agentos_audit::AuditSeverity::Security,
            _ => agentos_audit::AuditSeverity::Warn,
        };

        // Record escalation in audit log
        self.audit_log(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id,
            event_type: agentos_audit::AuditEventType::PermissionDenied,
            agent_id: Some(task.agent_id),
            task_id: Some(task.id),
            tool_id: None,
            details: serde_json::json!({
                "escalation": true,
                "reason": format!("{:?}", reason),
                "context_summary": context_summary,
                "decision_point": decision_point,
                "options": options,
                "urgency": urgency,
                "blocking": blocking,
            }),
            severity,
            reversible: false,
            rollback_ref: None,
        });

        // Store escalation for the escalation manager
        self.escalation_manager
            .create_escalation(
                task.id,
                task.agent_id,
                reason,
                context_summary.to_string(),
                decision_point.to_string(),
                options.to_vec(),
                urgency.to_string(),
                blocking,
                trace_id,
                None, // auto_action: default deny on expiry
            )
            .await;

        // If blocking, set task state to Waiting
        if blocking {
            self.scheduler
                .update_state(&task.id, TaskState::Waiting)
                .await
                .ok();
        }

        KernelActionResult {
            success: true,
            result: serde_json::json!({
                "status": if blocking { "escalation_pending_blocking" } else { "escalation_logged" },
                "message": if blocking {
                    "Task paused. Waiting for human review."
                } else {
                    "Escalation logged. Continuing task execution."
                },
                "urgency": urgency,
            }),
        }
    }

    async fn execute_switch_partition(
        &self,
        task: &AgentTask,
        partition: &str,
    ) -> KernelActionResult {
        let target_partition = match partition {
            "scratchpad" => ContextPartition::Scratchpad,
            "active" => ContextPartition::Active,
            _ => ContextPartition::Active,
        };

        match self
            .context_manager
            .set_partition_for_task(&task.id, target_partition)
            .await
        {
            Ok(()) => KernelActionResult {
                success: true,
                result: serde_json::json!({
                    "status": "partition_switched",
                    "partition": partition,
                }),
            },
            Err(_) => KernelActionResult {
                success: false,
                result: serde_json::json!({
                    "error": "Context not found for task"
                }),
            },
        }
    }

    async fn execute_memory_block_write(
        &self,
        task: &AgentTask,
        label: &str,
        content: &str,
    ) -> KernelActionResult {
        match self.memory_blocks.write(&task.agent_id, label, content) {
            Ok(block) => KernelActionResult {
                success: true,
                result: serde_json::json!({
                    "status": "memory_block_written",
                    "label": block.label,
                    "size": block.content.len(),
                }),
            },
            Err(e) => KernelActionResult {
                success: false,
                result: serde_json::json!({ "error": e.to_string() }),
            },
        }
    }

    async fn execute_memory_block_read(&self, task: &AgentTask, label: &str) -> KernelActionResult {
        match self.memory_blocks.get(&task.agent_id, label) {
            Ok(Some(block)) => KernelActionResult {
                success: true,
                result: serde_json::json!({
                    "label": block.label,
                    "content": block.content,
                    "updated_at": block.updated_at.to_rfc3339(),
                }),
            },
            Ok(None) => KernelActionResult {
                success: false,
                result: serde_json::json!({ "error": format!("Memory block '{}' not found", label) }),
            },
            Err(e) => KernelActionResult {
                success: false,
                result: serde_json::json!({ "error": e.to_string() }),
            },
        }
    }

    async fn execute_memory_block_list(&self, task: &AgentTask) -> KernelActionResult {
        match self.memory_blocks.list(&task.agent_id) {
            Ok(blocks) => KernelActionResult {
                success: true,
                result: serde_json::json!({
                    "count": blocks.len(),
                    "blocks": blocks.into_iter().map(|b| serde_json::json!({
                        "label": b.label,
                        "size": b.content.len(),
                        "updated_at": b.updated_at.to_rfc3339(),
                    })).collect::<Vec<_>>(),
                }),
            },
            Err(e) => KernelActionResult {
                success: false,
                result: serde_json::json!({ "error": e.to_string() }),
            },
        }
    }

    async fn execute_memory_block_delete(
        &self,
        task: &AgentTask,
        label: &str,
    ) -> KernelActionResult {
        match self.memory_blocks.delete(&task.agent_id, label) {
            Ok(true) => KernelActionResult {
                success: true,
                result: serde_json::json!({ "status": "deleted" }),
            },
            Ok(false) => KernelActionResult {
                success: false,
                result: serde_json::json!({ "error": "Block not found" }),
            },
            Err(e) => KernelActionResult {
                success: false,
                result: serde_json::json!({ "error": format!("Failed to delete memory block: {}", e) }),
            },
        }
    }

    async fn execute_agent_inbox_list(
        &self,
        task: &AgentTask,
        limit: u32,
        unread_only: bool,
    ) -> KernelActionResult {
        match self
            .agent_inbox
            .list(task.agent_id, unread_only, limit)
            .await
        {
            Ok(entries) => {
                // Project to titles + metadata only — bodies are fetched on demand
                // via agent-inbox-read so prompts and tool-result context stay lean.
                let projected: Vec<serde_json::Value> = entries
                    .iter()
                    .map(|e| {
                        serde_json::json!({
                            "id": e.id.to_string(),
                            "kind": e.kind.as_str(),
                            "title": e.title,
                            "ref_id": e.ref_id,
                            "created_at": e.created_at.to_rfc3339(),
                            "read": e.read,
                        })
                    })
                    .collect();
                KernelActionResult {
                    success: true,
                    result: serde_json::json!({
                        "entries": projected,
                        "count": entries.len(),
                    }),
                }
            }
            Err(e) => KernelActionResult {
                success: false,
                result: serde_json::json!({ "error": format!("Failed to list inbox: {}", e) }),
            },
        }
    }

    async fn execute_agent_inbox_read(&self, task: &AgentTask, id: &str) -> KernelActionResult {
        use std::str::FromStr;
        let entry_id = match agentos_types::AgentInboxEntryID::from_str(id) {
            Ok(id) => id,
            Err(e) => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({ "error": format!("Invalid ID: {}", e) }),
                }
            }
        };
        // Ownership check before any mutation: never reveal that an unrelated entry
        // exists. Both "missing" and "wrong owner" return the same error.
        let entry = match self.agent_inbox.get(entry_id).await {
            Ok(Some(e)) if e.agent_id == task.agent_id => e,
            Ok(_) => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({ "error": "not found" }),
                }
            }
            Err(e) => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({ "error": format!("Failed to fetch: {}", e) }),
                }
            }
        };
        if let Err(e) = self.agent_inbox.mark_read(entry_id).await {
            tracing::warn!(error = %e, %entry_id, "agent-inbox-read: mark_read failed");
        }
        KernelActionResult {
            success: true,
            result: serde_json::json!({
                "id": entry.id.to_string(),
                "kind": entry.kind.as_str(),
                "title": entry.title,
                "body": entry.body,
                "ref_id": entry.ref_id,
                "created_at": entry.created_at.to_rfc3339(),
            }),
        }
    }

    async fn execute_agent_inbox_dismiss(&self, task: &AgentTask, id: &str) -> KernelActionResult {
        use std::str::FromStr;
        let entry_id = match agentos_types::AgentInboxEntryID::from_str(id) {
            Ok(id) => id,
            Err(e) => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({ "error": format!("Invalid ID: {}", e) }),
                }
            }
        };
        // Ownership check before deletion. Mirrors execute_agent_inbox_read so an
        // attacker cannot dismiss another agent's notifications by guessing IDs.
        match self.agent_inbox.get(entry_id).await {
            Ok(Some(e)) if e.agent_id == task.agent_id => {}
            Ok(_) => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({ "error": "not found" }),
                }
            }
            Err(e) => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({ "error": format!("Failed to fetch: {}", e) }),
                }
            }
        }
        match self.agent_inbox.dismiss(entry_id).await {
            Ok(()) => KernelActionResult {
                success: true,
                result: serde_json::json!({ "status": "dismissed", "id": entry_id.to_string() }),
            },
            Err(e) => KernelActionResult {
                success: false,
                result: serde_json::json!({ "error": format!("Failed to dismiss: {}", e) }),
            },
        }
    }

    async fn execute_agent_messages_list(
        &self,
        task: &AgentTask,
        limit: u32,
        unread_only: bool,
    ) -> KernelActionResult {
        match self
            .agent_message_inbox
            .list(task.agent_id, unread_only, limit)
            .await
        {
            Ok(entries) => {
                // Project: titles ("from") and metadata, omit body — fetched via
                // agent-messages-read on demand.
                let projected: Vec<serde_json::Value> = entries
                    .iter()
                    .map(|m| {
                        serde_json::json!({
                            "id": m.id.to_string(),
                            "from": m.from_agent_name,
                            "from_agent_id": m.from_agent_id.to_string(),
                            "reply_to": m.reply_to.map(|r| r.to_string()),
                            "created_at": m.created_at.to_rfc3339(),
                            "read": m.read,
                        })
                    })
                    .collect();
                KernelActionResult {
                    success: true,
                    result: serde_json::json!({
                        "messages": projected,
                        "count": entries.len(),
                    }),
                }
            }
            Err(e) => KernelActionResult {
                success: false,
                result: serde_json::json!({ "error": format!("Failed to list messages: {}", e) }),
            },
        }
    }

    async fn execute_agent_messages_read(&self, task: &AgentTask, id: &str) -> KernelActionResult {
        use std::str::FromStr;
        let entry_id = match agentos_types::AgentMessageEntryID::from_str(id) {
            Ok(id) => id,
            Err(e) => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({ "error": format!("Invalid ID: {}", e) }),
                }
            }
        };
        let entry = match self.agent_message_inbox.get(entry_id).await {
            Ok(Some(m)) if m.to_agent_id == task.agent_id => m,
            Ok(_) => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({ "error": "not found" }),
                }
            }
            Err(e) => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({ "error": format!("Failed to fetch: {}", e) }),
                }
            }
        };
        if let Err(e) = self.agent_message_inbox.mark_read(entry_id).await {
            tracing::warn!(error = %e, %entry_id, "agent-messages-read: mark_read failed");
        }
        KernelActionResult {
            success: true,
            result: serde_json::json!({
                "id": entry.id.to_string(),
                "from": entry.from_agent_name,
                "from_agent_id": entry.from_agent_id.to_string(),
                "body": entry.body,
                "reply_to": entry.reply_to.map(|r| r.to_string()),
                "created_at": entry.created_at.to_rfc3339(),
            }),
        }
    }

    async fn execute_agent_messages_dismiss(
        &self,
        task: &AgentTask,
        id: &str,
    ) -> KernelActionResult {
        use std::str::FromStr;
        let entry_id = match agentos_types::AgentMessageEntryID::from_str(id) {
            Ok(id) => id,
            Err(e) => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({ "error": format!("Invalid ID: {}", e) }),
                }
            }
        };
        match self.agent_message_inbox.get(entry_id).await {
            Ok(Some(m)) if m.to_agent_id == task.agent_id => {}
            Ok(_) => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({ "error": "not found" }),
                }
            }
            Err(e) => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({ "error": format!("Failed to fetch: {}", e) }),
                }
            }
        }
        match self.agent_message_inbox.dismiss(entry_id).await {
            Ok(()) => KernelActionResult {
                success: true,
                result: serde_json::json!({ "status": "dismissed", "id": entry_id.to_string() }),
            },
            Err(e) => KernelActionResult {
                success: false,
                result: serde_json::json!({ "error": format!("Failed to dismiss: {}", e) }),
            },
        }
    }

    /// Execute a fire-and-forget notification on behalf of a running task.
    ///
    /// Defense-in-depth: validates `user.notify:w` from the task's capability
    /// token even though `ToolRunner` already checked it.
    async fn execute_notify_user(
        &self,
        task: &AgentTask,
        subject: String,
        body: String,
        priority: String,
        channels: Vec<String>,
        trace_id: TraceID,
    ) -> KernelActionResult {
        // Defense-in-depth permission check.
        if !task
            .capability_token
            .permissions
            .check(agentos_capability::PERM_USER_NOTIFY, PermissionOp::Write)
        {
            return KernelActionResult {
                success: false,
                result: serde_json::json!({
                    "error": format!(
                        "Permission denied: '{}:w' required for notify-user",
                        agentos_capability::PERM_USER_NOTIFY
                    )
                }),
            };
        }

        let priority_parsed = parse_priority(&priority);

        let agent_name = {
            let reg = self.agent_registry.read().await;
            reg.get_by_id(&task.agent_id)
                .map(|a| a.name.clone())
                .unwrap_or_else(|| task.agent_id.to_string())
        };
        let subject_prefixed = format!("[{agent_name}] {}", subject.as_str());
        let subject_line: String = subject_prefixed.chars().take(80).collect();

        // Resolve `channels` selectors against the registered-channels list.
        // Each selector may be a registered channel's display_name, its
        // `ChannelInstanceID`, or a generic kind id (telegram/slack/cli/...).
        let (instance_ids, channel_kinds, unmatched, available) = if channels.is_empty() {
            (
                std::collections::HashSet::new(),
                std::collections::HashSet::new(),
                Vec::new(),
                Vec::new(),
            )
        } else {
            let registered = match self.channel_registry.list_active().await {
                Ok(list) => list,
                Err(e) => {
                    return KernelActionResult {
                        success: false,
                        result: serde_json::json!({
                            "error": format!("Failed to list channels: {e}")
                        }),
                    };
                }
            };
            let mut ids: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut kinds: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut unmatched: Vec<String> = Vec::new();
            for sel in &channels {
                let lc = sel.to_ascii_lowercase();
                let mut matched = false;
                // Try ChannelInstanceID match.
                if let Some(c) = registered.iter().find(|c| c.id.to_string() == *sel) {
                    ids.insert(c.id.to_string());
                    matched = true;
                }
                // Try display_name match (case-sensitive — display names are user-set).
                if !matched {
                    let by_name: Vec<&agentos_types::RegisteredChannel> = registered
                        .iter()
                        .filter(|c| c.display_name == *sel)
                        .collect();
                    if !by_name.is_empty() {
                        for c in &by_name {
                            ids.insert(c.id.to_string());
                        }
                        matched = true;
                    }
                }
                // Treat as kind id (telegram/slack/webhook/desktop/cli/web/ntfy/email/...)
                // Also expands to all registered channels of that kind so adapters
                // owned by `channel_manager` (Discord/WhatsApp/...) reachable via
                // their instance_id receive the message.
                if !matched {
                    kinds.insert(lc.clone());
                    let kind_matches: Vec<&agentos_types::RegisteredChannel> = registered
                        .iter()
                        .filter(|c| c.kind.to_string() == lc)
                        .collect();
                    if !kind_matches.is_empty() {
                        for c in &kind_matches {
                            ids.insert(c.id.to_string());
                        }
                        matched = true;
                    } else if BUILTIN_DELIVERY_KINDS.contains(&lc.as_str()) {
                        // Built-in adapter id (no registered-channel row required).
                        matched = true;
                    }
                }
                if !matched {
                    unmatched.push(sel.clone());
                }
            }
            let available: Vec<serde_json::Value> = registered
                .iter()
                .map(|c| {
                    serde_json::json!({
                        "id": c.id.to_string(),
                        "name": c.display_name,
                        "kind": c.kind.to_string(),
                    })
                })
                .collect();
            (ids, kinds, unmatched, available)
        };

        if !unmatched.is_empty() {
            return KernelActionResult {
                success: false,
                result: serde_json::json!({
                    "error": format!(
                        "notify-user: unknown channel selector(s): {}",
                        unmatched.join(", ")
                    ),
                    "available_channels": available,
                    "builtin_kinds": BUILTIN_DELIVERY_KINDS,
                }),
            };
        }

        let msg = UserMessage {
            actions: Vec::new(),
            id: NotificationID::new(),
            from: NotificationSource::Agent(task.agent_id),
            task_id: Some(task.id),
            trace_id,
            kind: UserMessageKind::Notification,
            priority: priority_parsed,
            subject: subject_line,
            body,
            interaction: None,
            delivery_status: HashMap::new(),
            response: None,
            created_at: Utc::now(),
            expires_at: None,
            read: false,
            thread_id: Some(task.id.to_string()),
            reply_to_external_id: None,
            attachment: None,
        };

        let notification_id = msg.id;

        let delivery = if channels.is_empty() {
            self.notification_router.deliver(msg).await.map(|_| ())
        } else {
            self.notification_router
                .deliver_filtered(msg, &instance_ids, &channel_kinds)
                .await
        };

        match delivery {
            Ok(_) => {
                self.audit_log(AuditEntry {
                    timestamp: Utc::now(),
                    trace_id,
                    event_type: AuditEventType::NotificationSent,
                    agent_id: Some(task.agent_id),
                    task_id: Some(task.id),
                    tool_id: None,
                    details: serde_json::json!({
                        "notification_id": notification_id.to_string(),
                        "source": "notify-user tool",
                        "channels": channels,
                    }),
                    severity: AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
                KernelActionResult {
                    success: true,
                    result: serde_json::json!({
                        "status": "notification_sent",
                        "notification_id": notification_id.to_string(),
                        "channels": channels,
                    }),
                }
            }
            Err(e) => KernelActionResult {
                success: false,
                result: serde_json::json!({ "error": e.to_string() }),
            },
        }
    }

    /// Execute a blocking ask-user interaction.
    ///
    /// Delivers a `Question` notification to the user inbox, then parks the
    /// task in `Waiting` state until the user responds (or the timeout fires).
    ///
    /// Defense-in-depth: validates `user.interact:x` from the task's capability
    /// token even though `ToolRunner` already checked it.
    ///
    /// While parked the tokio runtime continues executing other tasks — this is
    /// a cooperative async suspension, not a thread block.
    #[allow(clippy::too_many_arguments)]
    async fn execute_ask_user(
        &self,
        task: &AgentTask,
        question: String,
        options: Option<Vec<String>>,
        timeout_secs: u64,
        priority: String,
        auto_action: String,
        trace_id: TraceID,
    ) -> KernelActionResult {
        // Defense-in-depth permission check.
        if !task.capability_token.permissions.check(
            agentos_capability::PERM_USER_INTERACT,
            PermissionOp::Execute,
        ) {
            return KernelActionResult {
                success: false,
                result: serde_json::json!({
                    "error": format!(
                        "Permission denied: '{}:x' required for ask-user",
                        agentos_capability::PERM_USER_INTERACT
                    )
                }),
            };
        }

        // Park the task while the user is asked; the shared helper only knows
        // about notifications, so the scheduler transition lives here.
        // Chat turns run under a synthetic task id the scheduler never saw.
        // Remember whether parking took, so the post-answer liveness check
        // below does not read "not found" as "terminated" and discard the
        // user's answer.
        let parked = match self
            .scheduler
            .update_state(&task.id, TaskState::Waiting)
            .await
        {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(
                    task_id = %task.id,
                    error = %e,
                    "ask-user: failed to set task state to Waiting"
                );
                false
            }
        };
        let (notification_id, response) = match ask_user_blocking(
            &self.notification_router,
            &self.agent_registry,
            &self.cancellation_token,
            task.agent_id,
            task.id,
            trace_id,
            AskUserArgs {
                question,
                options,
                timeout_secs,
                priority,
                auto_action: auto_action.clone(),
            },
        )
        .await
        {
            Ok(v) => v,
            Err(failed) => return failed,
        };

        let restored = !parked
            || self
                .scheduler
                .update_state_if_not_terminal(&task.id, TaskState::Running)
                .await
                .unwrap_or(false);

        if !restored {
            tracing::info!(
                task_id = %task.id,
                "ask-user: task entered terminal state while waiting for user response; aborting"
            );
            return KernelActionResult {
                success: false,
                result: serde_json::json!({
                    "error": "Task was cancelled or terminated while waiting for user response"
                }),
            };
        }

        // Audit the received response.
        self.audit_log(AuditEntry {
            timestamp: Utc::now(),
            trace_id,
            event_type: AuditEventType::UserResponseReceived,
            agent_id: Some(task.agent_id),
            task_id: Some(task.id),
            tool_id: None,
            details: serde_json::json!({
                "notification_id": notification_id.to_string(),
                "channel": response.channel.to_string(),
                "auto_actioned": response.text == auto_action || response.text == "kernel_shutdown",
            }),
            severity: AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        });

        KernelActionResult {
            success: true,
            result: serde_json::json!({
                "response": response.text,
                "channel": response.channel.to_string(),
                "responded_at": response.responded_at.to_rfc3339(),
            }),
        }
    }

    /// Ask the operator for access to a host folder and park until they decide.
    ///
    /// The escalation carries `metadata.kind = "workspace_access"`, which
    /// `EscalationManager::resolve` acts on: the grant is written BEFORE the
    /// caller is woken, so a woken-approved call finds the access already live.
    /// Before this existed, approving the failing tool call was the only thing
    /// an operator could do, and it changed nothing — three approvals in the
    /// 2026-09-21 deadlock, all of them no-ops.
    async fn execute_workspace_request(
        &self,
        task: &AgentTask,
        path: String,
        mode: String,
        reason: String,
        timeout_secs: u64,
        trace_id: TraceID,
    ) -> KernelActionResult {
        let failed = |error: String| KernelActionResult {
            success: false,
            result: serde_json::json!({ "granted": false, "error": error }),
        };

        // Defense in depth: the tool validated this payload, but the tool runs
        // on model output and the metadata below is acted on later by `resolve`.
        if let Err(e) =
            agentos_tools::workspace_request::validate_request(&path, &mode, &self.data_dir)
        {
            return failed(e.to_string());
        }
        if !task
            .capability_token
            .permissions
            .check("fs.workspace", PermissionOp::Read)
        {
            return failed(
                "Permission denied: 'fs.workspace:r' required for workspace-request".into(),
            );
        }

        let agent_name = {
            let registry = self.agent_registry.read().await;
            registry
                .get_by_id(&task.agent_id)
                .map(|a| a.name.clone())
                .unwrap_or_else(|| task.agent_id.to_string())
        };

        // One pending question per (agent, path). Without this, a retry loop
        // turns into an approval-fatigue loop — the operator sees the same
        // request once per iteration and stops reading any of them.
        if let Some(existing) = self
            .escalation_manager
            .list_pending()
            .await
            .into_iter()
            .find(|e| {
                e.agent_id == task.agent_id
                    && e.metadata.get("kind").and_then(|v| v.as_str()) == Some("workspace_access")
                    && e.metadata.get("path").and_then(|v| v.as_str()) == Some(path.as_str())
            })
        {
            return KernelActionResult {
                success: true,
                result: serde_json::json!({
                    "granted": false,
                    "pending": true,
                    "escalation_id": existing.id,
                    "message": format!(
                        "A request for '{path}' is already waiting for the operator (escalation {}). \
                         Do not ask again — continue with something else, or say you are blocked.",
                        existing.id
                    ),
                }),
            };
        }

        let escalation_id = self
            .escalation_manager
            .create_escalation_with_metadata(
                task.id,
                task.agent_id,
                EscalationReason::AuthorizationRequired,
                format!("{reason}\n\nRequested: {path} ({mode})"),
                format!("Grant {agent_name} '{mode}' access to {path}?"),
                vec!["approve".to_string(), "deny".to_string()],
                "high".to_string(),
                true,
                trace_id,
                Some(crate::escalation::AutoAction::Deny),
                serde_json::json!({
                    "kind": "workspace_access",
                    "path": path,
                    "mode": mode,
                }),
            )
            .await;

        self.escalation_manager
            .prepare_resolution(escalation_id)
            .await;
        // The operator can answer between creation and this park; installing
        // the channel first means such a resolution still fires it.
        let already_resolved = self
            .escalation_manager
            .get(escalation_id)
            .await
            .map(|e| e.resolved)
            .unwrap_or(false);
        let mut approved = false;
        if !already_resolved {
            if let Some(rx) = self
                .escalation_manager
                .take_resolution_receiver(escalation_id)
                .await
            {
                approved = matches!(
                    tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), rx).await,
                    Ok(Ok(crate::escalation::ResolutionOutcome::Approved))
                );
            }
        } else {
            approved = self
                .escalation_manager
                .get(escalation_id)
                .await
                .and_then(|e| e.resolution)
                .map(|r| crate::escalation::resolution_is_approval(&r))
                .unwrap_or(false);
        }

        // The grant list, not the wake outcome, is the truth: `resolve` writes
        // the grant before waking us, and a write can still fail after the
        // operator said yes.
        let live = self
            .workspace_grants
            .list_for_agent(&task.agent_id)
            .into_iter()
            .any(|g| g.path == std::path::Path::new(&path));

        if live {
            KernelActionResult {
                success: true,
                result: serde_json::json!({
                    "granted": true,
                    "path": path,
                    "mode": mode,
                    "escalation_id": escalation_id,
                    "next": "Access is live now — retry the call that failed.",
                }),
            }
        } else {
            KernelActionResult {
                success: true,
                result: serde_json::json!({
                    "granted": false,
                    "path": path,
                    "escalation_id": escalation_id,
                    "reason": if approved { "approved but the grant could not be written" } else { "the operator did not grant it" },
                    "next": "Do not ask again for this path in this task. Say what you are blocked on.",
                }),
            }
        }
    }

    /// Execute a synchronous agent-to-agent RPC call.
    ///
    /// Creates a child task for the target agent, registers a pending call
    /// in `RpcManager`, then blocks until the child completes. The child
    /// task runs through the same `execute_task_sync` path as any other
    /// task, preserving all security and audit guarantees.
    async fn execute_agent_rpc_call(
        &self,
        task: &AgentTask,
        target_agent: &str,
        prompt: &str,
        timeout_secs: u64,
        trace_id: TraceID,
    ) -> KernelActionResult {
        // 1. Resolve target agent
        let registry = self.agent_registry.read().await;
        let target = match registry.get_by_name(target_agent) {
            Some(a) if a.status != AgentStatus::Offline => a.clone(),
            Some(_) => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": format!("Agent '{}' is offline", target_agent)
                    }),
                };
            }
            None => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": format!("Agent '{}' not found", target_agent)
                    }),
                };
            }
        };
        let target_permissions = registry.compute_effective_permissions(&target.id);
        drop(registry);

        // 1b. Prevent self-calls — an agent cannot RPC itself
        if target.id == task.agent_id {
            return KernelActionResult {
                success: false,
                result: serde_json::json!({
                    "error": "An agent cannot call itself via RPC"
                }),
            };
        }

        // 2-3. Scope the child via the shared hardened path: depth cap, pure
        //      parent∩target intersection (no process.exec re-grant), parent-token
        //      signature/expiry verification, fresh child TaskID.
        let (child_token, child_depth) = match self
            .scope_child_task(
                task,
                target.id,
                &target_permissions,
                Duration::from_secs(timeout_secs),
            )
            .await
        {
            Ok(scoped) => scoped,
            Err(e) => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": format!("Failed to scope child capabilities: {}", e)
                    }),
                };
            }
        };
        let child_task_id = child_token.task_id;

        // 4. Register the RPC call in the manager (get oneshot receiver)
        let rx = match self
            .rpc_manager
            .register_call(task.id, target.id, child_task_id, timeout_secs)
            .await
        {
            Ok(rx) => rx,
            Err(e) => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({ "error": e.to_string() }),
                };
            }
        };

        // 5. Create and register the child task
        let child_task = AgentTask {
            id: child_task_id,
            state: TaskState::Queued,
            agent_id: target.id,
            capability_token: child_token,
            assigned_llm: None,
            priority: task.priority,
            created_at: chrono::Utc::now(),
            started_at: None,
            timeout: Duration::from_secs(timeout_secs),
            original_prompt: prompt.to_string(),
            history: Vec::new(),
            parent_task: Some(task.id),
            reasoning_hints: Some(crate::commands::task::infer_reasoning_hints(prompt)),
            max_iterations: None,
            trigger_source: None,
            // Children are always bounded — never inherit parent autonomy.
            autonomous: false,
            parent_task_id: Some(task.id),
            spawn_depth: child_depth,
            is_team_coordinator: false,
            skip_checkpoint: false,
            thinking_level: ThinkingLevel::Off,
            spawner_agent_id: Some(task.agent_id),
            tool_categories: task.tool_categories.clone(),
            disable_tool_scoping: false,
            // Inherit the caller's causal depth so an event-triggered agent-RPC
            // chain still trips `max_chain_depth`.
            chain_depth: task.event_chain_depth(),
        };

        self.scheduler.register_external(child_task.clone()).await;
        // Register for cascade-cancel bookkeeping (parent → child edge).
        self.scheduler.register_child(task.id, child_task_id).await;
        self.scheduler
            .update_state_if_not_terminal(&child_task_id, TaskState::Running)
            .await
            .ok();
        self.scheduler.mark_started(&child_task_id).await.ok();

        // 6. Emit audit and event
        self.audit_log(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id,
            event_type: agentos_audit::AuditEventType::TaskCreated,
            agent_id: Some(target.id),
            task_id: Some(child_task_id),
            tool_id: None,
            details: serde_json::json!({
                "rpc_call": true,
                "caller_task_id": task.id.to_string(),
                "caller_agent_id": task.agent_id.to_string(),
                "target_agent": target_agent,
                "timeout_secs": timeout_secs,
            }),
            severity: agentos_audit::AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        });

        self.emit_event_with_trace(
            EventType::AgentRpcCallStarted,
            EventSource::AgentMessageBus,
            EventSeverity::Info,
            serde_json::json!({
                "caller_task_id": task.id.to_string(),
                "caller_agent_id": task.agent_id.to_string(),
                "rpc_task_id": child_task_id.to_string(),
                "target_agent_id": target.id.to_string(),
                "target_agent_name": target_agent,
                "timeout_secs": timeout_secs,
            }),
            0,
            Some(trace_id),
            Some(task.agent_id),
            Some(task.id),
        )
        .await;

        // 7. Set caller task to Waiting while the RPC child runs
        self.scheduler
            .update_state_if_not_terminal(&task.id, TaskState::Waiting)
            .await
            .ok();

        // 8. Start trace for child task
        self.trace_collector
            .start_task(child_task_id, target.id, prompt)
            .await;

        // 9. Execute child task. Box::pin breaks the recursive async future
        //    cycle (execute_agent_rpc_call → execute_task_sync → tool loop →
        //    dispatch_kernel_action → execute_agent_rpc_call).
        let child_trace_id = TraceID::new();
        let start = chrono::Utc::now();
        let child_task_span = self.otel.start_task_span(
            &child_task.id.to_string(),
            &child_task.agent_id.to_string(),
            &target.model,
        );
        self.otel.adjust_active_tasks(1);
        let child_result =
            Box::pin(self.execute_task_sync(&child_task, &child_trace_id, &child_task_span)).await;
        let duration_ms = (chrono::Utc::now() - start).num_milliseconds().max(0) as u64;

        // 10. Finish child trace and handle completion
        match child_result {
            Ok(task_result) => {
                self.trace_collector
                    .finish_task(&child_task_id, "Complete", chrono::Utc::now())
                    .await;
                child_task_span.set_string_attribute("task.status", "complete");
                child_task_span.set_i64_attribute("task.iterations", task_result.iterations as i64);
                self.otel.record_task_metric(
                    &child_task.agent_id.to_string(),
                    "complete",
                    duration_ms,
                );
                self.otel.adjust_active_tasks(-1);
                self.complete_task_success(&child_task, &task_result, duration_ms, child_trace_id)
                    .await;
            }
            Err(e) => {
                self.trace_collector
                    .finish_task(&child_task_id, "Failed", chrono::Utc::now())
                    .await;
                child_task_span.set_string_attribute("task.status", "failed");
                child_task_span.record_error(e.to_string());
                self.otel.record_task_metric(
                    &child_task.agent_id.to_string(),
                    "failed",
                    duration_ms,
                );
                self.otel.adjust_active_tasks(-1);
                self.complete_task_failure(&child_task, e, duration_ms, child_trace_id)
                    .await;
            }
        }

        // 11. Restore caller task to Running
        self.scheduler
            .update_state_if_not_terminal(&task.id, TaskState::Running)
            .await
            .ok();

        // 12. Wait for the result from the oneshot (should already be available
        // since complete_task_success/failure calls rpc_manager.complete_call)
        let safety_timeout = Duration::from_secs(timeout_secs.saturating_add(30));
        let rpc_result = tokio::select! {
            result = tokio::time::timeout(safety_timeout, rx) => {
                match result {
                    Ok(Ok(r)) => r,
                    Ok(Err(_)) => {
                        // Sender dropped — RPC was never completed (should not happen)
                        crate::rpc_manager::RpcResult {
                            output: String::new(),
                            success: false,
                            error: Some("RPC call aborted: result channel dropped".to_string()),
                        }
                    }
                    Err(_) => {
                        // Safety timeout
                        crate::rpc_manager::RpcResult {
                            output: String::new(),
                            success: false,
                            error: Some("RPC call timed out".to_string()),
                        }
                    }
                }
            }
            _ = self.cancellation_token.cancelled() => {
                crate::rpc_manager::RpcResult {
                    output: String::new(),
                    success: false,
                    error: Some("Kernel shutting down".to_string()),
                }
            }
        };

        // 13. Emit completion event
        self.emit_event_with_trace(
            EventType::AgentRpcCallCompleted,
            EventSource::AgentMessageBus,
            if rpc_result.success {
                EventSeverity::Info
            } else {
                EventSeverity::Warning
            },
            serde_json::json!({
                "caller_task_id": task.id.to_string(),
                "rpc_task_id": child_task_id.to_string(),
                "success": rpc_result.success,
                "error": rpc_result.error,
            }),
            0,
            Some(trace_id),
            Some(task.agent_id),
            Some(task.id),
        )
        .await;

        if rpc_result.success {
            KernelActionResult {
                success: true,
                result: serde_json::json!({
                    "status": "rpc_complete",
                    "target_agent": target_agent,
                    "rpc_task_id": child_task_id.to_string(),
                    "output": rpc_result.output,
                }),
            }
        } else {
            KernelActionResult {
                success: false,
                result: serde_json::json!({
                    "error": rpc_result.error.unwrap_or_else(|| "RPC call failed".to_string()),
                    "rpc_task_id": child_task_id.to_string(),
                }),
            }
        }
    }

    // ─── Event self-subscription handlers ────────────────────────────
    //
    // These power the four `event-*` agent tools. Each handler runs with
    // `task.agent_id` as the calling identity and never accepts a
    // `target_agent` argument — agents can only manage their own
    // subscriptions. Per-category permission gating happens in
    // `event_permissions::check_subscribe_permission`.

    async fn execute_event_subscribe(
        &self,
        task: &AgentTask,
        event_filter: String,
        payload_filter: Option<String>,
        throttle: Option<String>,
        priority: Option<String>,
        trace_id: TraceID,
    ) -> KernelActionResult {
        let parsed_filter = match crate::event_bus::parse_event_type_filter(&event_filter) {
            Some(f) => f,
            None => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": format!(
                            "Invalid event filter '{}'. Use 'all', 'category:<Name>', or an exact event type like 'AgentAdded'.",
                            event_filter
                        ),
                    }),
                };
            }
        };

        // Permission check — gated per category. Uses the capability token
        // permissions, which already include any role-derived observe grants.
        if let Err(e) = crate::event_permissions::check_subscribe_permission(
            &task.capability_token.permissions,
            &parsed_filter,
        ) {
            self.audit_log(AuditEntry {
                timestamp: Utc::now(),
                trace_id,
                event_type: AuditEventType::PermissionDenied,
                agent_id: Some(task.agent_id),
                task_id: Some(task.id),
                tool_id: None,
                details: serde_json::json!({
                    "tool": "event-subscribe",
                    "event_filter": event_filter,
                    "reason": e.to_string(),
                }),
                severity: AuditSeverity::Warn,
                reversible: false,
                rollback_ref: None,
            });
            return KernelActionResult {
                success: false,
                result: serde_json::json!({
                    "error": e.to_string(),
                    "hint": "Ask an operator to grant the required `events.<category>:observe` permission, then retry.",
                }),
            };
        }

        // Fail closed: an unspecified throttle gets the same bounded default
        // as kernel-seeded role subscriptions. The 2026-08-31 budget-loop
        // incident ran on an agent-created Category sub stored with
        // `throttle: None`. Explicit "none" remains an auditable opt-out.
        let throttle_policy = match throttle.as_deref() {
            None | Some("") => crate::event_bus::default_role_subscription_throttle(),
            Some("none") => ThrottlePolicy::None,
            Some(s) => match parse_throttle_str(s) {
                Some(p) => p,
                None => {
                    return KernelActionResult {
                        success: false,
                        result: serde_json::json!({
                            "error": format!(
                                "Invalid throttle '{}'. Use 'none', 'once_per:<duration>' (e.g. 'once_per:30s'), or 'max:<count>/<duration>' (e.g. 'max:5/60s').",
                                s
                            ),
                        }),
                    };
                }
            },
        };

        let sub_priority = match crate::event_bus::parse_subscription_priority(priority.as_deref())
        {
            Some(p) => p,
            None => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": format!(
                            "Invalid priority '{}'. Use 'critical', 'high', 'normal', or 'low'.",
                            priority.as_deref().unwrap_or_default()
                        ),
                    }),
                };
            }
        };

        let payload_filter = payload_filter.and_then(|raw| {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });
        if let Some(Err(e)) = payload_filter
            .as_deref()
            .map(crate::event_bus::validate_filter)
        {
            return KernelActionResult {
                success: false,
                result: serde_json::json!({ "error": e }),
            };
        }

        let sub = EventSubscription {
            id: SubscriptionID::new(),
            agent_id: task.agent_id,
            event_type_filter: parsed_filter,
            filter: payload_filter.clone(),
            priority: sub_priority,
            throttle: throttle_policy,
            enabled: true,
            created_at: Utc::now(),
        };

        let sub_id = self.event_bus.subscribe(sub).await;

        self.audit_log(AuditEntry {
            timestamp: Utc::now(),
            trace_id,
            event_type: AuditEventType::EventSubscriptionCreated,
            agent_id: Some(task.agent_id),
            task_id: Some(task.id),
            tool_id: None,
            details: serde_json::json!({
                "subscription_id": sub_id.to_string(),
                "event_filter": event_filter,
                "payload_filter": payload_filter,
                "self_subscribed": true,
            }),
            severity: AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        });

        KernelActionResult {
            success: true,
            result: serde_json::json!({
                "subscription_id": sub_id.to_string(),
                "event_filter": event_filter,
                "status": "subscribed",
                "message": "Subscription created. The kernel will dispatch matching events as new tasks for this agent.",
            }),
        }
    }

    async fn execute_event_unsubscribe(
        &self,
        task: &AgentTask,
        subscription_id: String,
        trace_id: TraceID,
    ) -> KernelActionResult {
        let id = match subscription_id.parse::<SubscriptionID>() {
            Ok(id) => id,
            Err(_) => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": format!("Invalid subscription ID: {}", subscription_id),
                    }),
                };
            }
        };

        // Verify the subscription belongs to the calling agent — agents must
        // not be able to cancel subscriptions owned by other agents.
        match self.event_bus.get_subscription(&id).await {
            Some(sub) if sub.agent_id == task.agent_id => {}
            Some(_) => {
                self.audit_log(AuditEntry {
                    timestamp: Utc::now(),
                    trace_id,
                    event_type: AuditEventType::PermissionDenied,
                    agent_id: Some(task.agent_id),
                    task_id: Some(task.id),
                    tool_id: None,
                    details: serde_json::json!({
                        "tool": "event-unsubscribe",
                        "subscription_id": subscription_id,
                        "reason": "subscription belongs to a different agent",
                    }),
                    severity: AuditSeverity::Warn,
                    reversible: false,
                    rollback_ref: None,
                });
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": "Subscription belongs to a different agent",
                    }),
                };
            }
            None => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": format!("Subscription '{}' not found", subscription_id),
                    }),
                };
            }
        }

        if self.event_bus.unsubscribe(&id).await {
            self.audit_log(AuditEntry {
                timestamp: Utc::now(),
                trace_id,
                event_type: AuditEventType::EventSubscriptionRemoved,
                agent_id: Some(task.agent_id),
                task_id: Some(task.id),
                tool_id: None,
                details: serde_json::json!({
                    "subscription_id": subscription_id,
                    "self_unsubscribed": true,
                }),
                severity: AuditSeverity::Info,
                reversible: false,
                rollback_ref: None,
            });
            KernelActionResult {
                success: true,
                result: serde_json::json!({
                    "subscription_id": subscription_id,
                    "status": "unsubscribed",
                }),
            }
        } else {
            KernelActionResult {
                success: false,
                result: serde_json::json!({
                    "error": format!("Subscription '{}' not found", subscription_id),
                }),
            }
        }
    }

    async fn execute_event_list_subscriptions(&self, task: &AgentTask) -> KernelActionResult {
        let subs = self
            .event_bus
            .list_subscriptions_for_agent(&task.agent_id)
            .await;
        let values: Vec<serde_json::Value> = subs
            .iter()
            .map(|s| {
                serde_json::json!({
                    "id": s.id.to_string(),
                    "event_type_filter": format!("{:?}", s.event_type_filter),
                    "payload_filter": s.filter,
                    "priority": format!("{:?}", s.priority),
                    "throttle": format!("{:?}", s.throttle),
                    "enabled": s.enabled,
                    "created_at": s.created_at.to_rfc3339(),
                })
            })
            .collect();
        KernelActionResult {
            success: true,
            result: serde_json::json!({
                "count": values.len(),
                "subscriptions": values,
            }),
        }
    }

    async fn execute_event_list_available(&self, task: &AgentTask) -> KernelActionResult {
        // Static category → event-types catalog. Mirrors EventType::category()
        // and stays in sync because the kernel test suite asserts coverage.
        let category_events: &[(&str, &str, &[&str])] = &[
            (
                "AgentLifecycle",
                "events.agent_lifecycle",
                &[
                    "AgentAdded",
                    "AgentRemoved",
                    "AgentPermissionGranted",
                    "AgentPermissionRevoked",
                ],
            ),
            (
                "TaskLifecycle",
                "events.task_lifecycle",
                &[
                    "TaskStarted",
                    "TaskCompleted",
                    "TaskFailed",
                    "TaskTimedOut",
                    "TaskSuspended",
                    "TaskDelegated",
                    "TaskRetrying",
                    "TaskDeadlockDetected",
                    "TaskPreempted",
                ],
            ),
            (
                "SecurityEvents",
                "events.security",
                &[
                    "PromptInjectionAttempt",
                    "CapabilityViolation",
                    "UnauthorizedToolAccess",
                    "SecretsAccessAttempt",
                    "SandboxEscapeAttempt",
                    "AuditLogTamperAttempt",
                    "AgentImpersonationAttempt",
                    "UnverifiedToolInstalled",
                ],
            ),
            (
                "MemoryEvents",
                "events.memory",
                &[
                    "ContextWindowNearLimit",
                    "ContextWindowExhausted",
                    "EpisodicMemoryWritten",
                    "SemanticMemoryConflict",
                    "MemorySearchFailed",
                    "WorkingMemoryEviction",
                ],
            ),
            (
                "SystemHealth",
                "events.system_health",
                &[
                    "CPUSpikeDetected",
                    "MemoryPressure",
                    "DiskSpaceLow",
                    "DiskSpaceCritical",
                    "ProcessCrashed",
                    "NetworkInterfaceDown",
                    "ContainerResourceQuotaExceeded",
                    "KernelSubsystemError",
                    "BudgetWarning",
                    "BudgetExhausted",
                ],
            ),
            (
                "HardwareEvents",
                "events.hardware",
                &[
                    "GPUAvailable",
                    "GPUMemoryPressure",
                    "SensorReadingThresholdExceeded",
                    "DeviceConnected",
                    "DeviceDisconnected",
                    "HardwareAccessGranted",
                    "DeviceMounted",
                    "DeviceUnmounted",
                    "DeviceEjected",
                    "PrintJobSubmitted",
                    "PrintJobCancelled",
                    "AudioCaptureStarted",
                    "AudioCaptureStopped",
                    "AudioPlaybackStarted",
                    "WebcamCaptureStarted",
                    "WebcamCaptureStopped",
                    "BluetoothScanStarted",
                    "BluetoothPairRequested",
                    "BluetoothConnected",
                    "DisplayConfigApplied",
                    "DisplayConfigReverted",
                    "RawUsbDeviceOpened",
                    "RawUsbTransferCompleted",
                ],
            ),
            (
                "ToolEvents",
                "events.tool",
                &[
                    "ToolInstalled",
                    "ToolRemoved",
                    "ToolExecutionFailed",
                    "ToolSandboxViolation",
                    "ToolResourceQuotaExceeded",
                    "ToolChecksumMismatch",
                    "ToolRegistryUpdated",
                    "ToolCallStarted",
                    "ToolCallCompleted",
                    "ToolFallbackAttempted",
                    "ToolFallbackSucceeded",
                    "ToolFallbackExhausted",
                ],
            ),
            (
                "AgentCommunication",
                "events.agent_communication",
                &[
                    "DirectMessageReceived",
                    "BroadcastReceived",
                    "DelegationReceived",
                    "DelegationResponseReceived",
                    "MessageDeliveryFailed",
                    "AgentUnreachable",
                    "AgentRpcCallStarted",
                    "AgentRpcCallCompleted",
                    "AgentRpcCallTimedOut",
                    "SubAgentProgress",
                    "SubAgentCompleted",
                    "SubAgentFailed",
                ],
            ),
            (
                "ScheduleEvents",
                "events.schedule",
                &[
                    "CronJobFired",
                    "ScheduledTaskMissed",
                    "ScheduledTaskCompleted",
                    "ScheduledTaskFailed",
                ],
            ),
            (
                "ExternalEvents",
                "events.external",
                &[
                    "WebhookReceived",
                    "ExternalFileChanged",
                    "ExternalAPIEvent",
                    "ExternalAlertReceived",
                ],
            ),
        ];

        let perms = &task.capability_token.permissions;
        let categories: Vec<serde_json::Value> = category_events
            .iter()
            .map(|(cat_name, perm_resource, events)| {
                let allowed = perms.check(perm_resource, agentos_types::PermissionOp::Observe);
                serde_json::json!({
                    "category": cat_name,
                    "permission": format!("{}:observe", perm_resource),
                    "subscribable": allowed,
                    "events": events,
                })
            })
            .collect();

        KernelActionResult {
            success: true,
            result: serde_json::json!({
                "categories": categories,
                "filter_syntax": {
                    "all": "Subscribe to every event (requires observe on every category — usually root-only).",
                    "category": "category:<CategoryName> — e.g. 'category:HardwareEvents'",
                    "exact": "<EventType> — e.g. 'DeviceConnected', or fully qualified 'HardwareEvents.DeviceConnected'",
                },
                "throttle_syntax": {
                    "none": "No throttle (default).",
                    "once_per": "once_per:<duration>  e.g. once_per:30s, once_per:5m",
                    "max": "max:<count>/<duration>  e.g. max:5/60s",
                },
                "priority_values": ["critical", "high", "normal", "low"],
                "tip": "Subscribable=false means you don't have observe permission for that category — ask an operator to grant `events.<category>:observe`.",
            }),
        }
    }
}

/// Parse a throttle string like "once_per:30s" or "max:5/60s".
/// Mirrors the parser in `commands/event.rs` so the agent-tool path does
/// not depend on a private CLI helper.
fn parse_throttle_str(s: &str) -> Option<ThrottlePolicy> {
    if let Some(dur_str) = s.strip_prefix("once_per:") {
        let duration = parse_duration_str(dur_str)?;
        return Some(ThrottlePolicy::MaxOncePerDuration(duration));
    }
    if let Some(rest) = s.strip_prefix("max:") {
        let parts: Vec<&str> = rest.splitn(2, '/').collect();
        if parts.len() != 2 {
            return None;
        }
        let count: u32 = parts[0].parse().ok()?;
        let duration = parse_duration_str(parts[1])?;
        return Some(ThrottlePolicy::MaxCountPerDuration(count, duration));
    }
    None
}

fn parse_duration_str(s: &str) -> Option<std::time::Duration> {
    let s = s.trim();
    if let Some(secs) = s.strip_suffix('s') {
        return secs.parse::<u64>().ok().map(std::time::Duration::from_secs);
    }
    if let Some(mins) = s.strip_suffix('m') {
        return mins
            .parse::<u64>()
            .ok()
            .map(|n| std::time::Duration::from_secs(n * 60));
    }
    if let Some(hours) = s.strip_suffix('h') {
        return hours
            .parse::<u64>()
            .ok()
            .map(|n| std::time::Duration::from_secs(n * 3600));
    }
    s.parse::<u64>().ok().map(std::time::Duration::from_secs)
}

/// SSRF protection for outbound A2A delegation requests.
///
/// Parses the URL, resolves the hostname, and checks every resolved IP against
/// private/internal ranges. Returns `Some(error_message)` if the URL should be
/// blocked, `None` if it is safe to proceed.
async fn check_a2a_url_ssrf(url: &str) -> Option<String> {
    let parsed = match url::Url::parse(url) {
        Ok(u) => u,
        Err(e) => return Some(format!("Invalid agent_url: {}", e)),
    };

    // Only allow http/https schemes
    match parsed.scheme() {
        "http" | "https" => {}
        other => {
            return Some(format!(
                "Blocked scheme '{}' — only http/https allowed",
                other
            ))
        }
    }

    let host = match parsed.host_str() {
        Some(h) => h.to_string(),
        None => return Some("agent_url has no host".to_string()),
    };

    let port = parsed.port_or_known_default().unwrap_or(80);

    // Resolve and check each IP
    let addrs: Vec<std::net::IpAddr> =
        match tokio::net::lookup_host(format!("{}:{}", host, port)).await {
            Ok(iter) => iter.map(|sa| sa.ip()).collect(),
            Err(e) => return Some(format!("DNS resolution failed for '{}': {}", host, e)),
        };

    for ip in &addrs {
        if is_private_addr(ip) {
            tracing::warn!(
                url = %url,
                %ip,
                "A2A delegation SSRF blocked: private/internal IP"
            );
            return Some(format!(
                "SSRF blocked: '{}' resolves to private/internal IP {}",
                host, ip
            ));
        }
    }

    None
}

/// Returns true if `ip` is a private, loopback, link-local, or otherwise
/// internal address that should never be reachable via agent-initiated A2A.
fn is_private_addr(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_unspecified()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_multicast()
                // 100.64.0.0/10 — Carrier-Grade NAT (RFC 6598)
                || {
                    let o = v4.octets();
                    o[0] == 100 && o[1] >= 64 && o[1] < 128
                }
        }
        std::net::IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_private_addr(&std::net::IpAddr::V4(v4));
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (v6.segments()[0] & 0xfe00) == 0xfc00 // fc00::/7 unique-local
                || (v6.segments()[0] & 0xffc0) == 0xfe80 // fe80::/10 link-local
        }
    }
}

/// Parse a priority string into a `NotificationPriority`.
///
/// Unrecognised values fall back to `Info`.
fn parse_priority(s: &str) -> NotificationPriority {
    match s.to_ascii_lowercase().as_str() {
        "warning" => NotificationPriority::Warning,
        "urgent" => NotificationPriority::Urgent,
        "critical" => NotificationPriority::Critical,
        _ => NotificationPriority::Info,
    }
}

impl Kernel {
    /// Resolve the run-as agent for an agent-created schedule/timer/once-job:
    /// an AgentID maps to its display name, empty means the calling agent.
    ///
    /// Targeting *another* agent is allowed (orchestrator fan-out), but it does
    /// not lend the caller that agent's grants: the creator is recorded on the
    /// schedule and every fire runs with `target ∩ creator` permissions — see
    /// `commands::background::clamp_to_schedule_creator`.
    async fn resolve_schedule_agent(&self, task: &AgentTask, agent_name: String) -> String {
        let registry = self.agent_registry.read().await;
        if agent_name.is_empty() {
            return registry
                .get_by_id(&task.agent_id)
                .map(|a| a.name.clone())
                .unwrap_or_else(|| task.agent_id.to_string());
        }
        match agent_name.parse::<AgentID>() {
            Ok(aid) => registry
                .get_by_id(&aid)
                .map(|a| a.name.clone())
                .unwrap_or(agent_name),
            Err(_) => agent_name,
        }
    }

    async fn execute_set_timer(
        &self,
        task: &AgentTask,
        name: String,
        delay_secs: u64,
        agent_name: String,
        action: TimerAction,
    ) -> KernelActionResult {
        let resolved = self.resolve_schedule_agent(task, agent_name).await;

        match self
            .schedule_manager
            .create_timer_with_creator(
                name.clone(),
                delay_secs,
                resolved,
                action,
                None,
                task.agent_id,
            )
            .await
        {
            Ok(id) => {
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: agentos_audit::AuditEventType::TimerCreated,
                    agent_id: Some(task.agent_id),
                    task_id: Some(task.id),
                    tool_id: None,
                    details: serde_json::json!({
                        "timer_name": name,
                        "timer_id": id.to_string(),
                        "delay_secs": delay_secs,
                        "source": "agent_tool",
                    }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
                let fire_at = Utc::now() + Duration::from_secs(delay_secs);
                KernelActionResult {
                    success: true,
                    result: serde_json::json!({
                        "timer_id": id.to_string(),
                        "timer_name": name,
                        "fires_at": fire_at.to_rfc3339(),
                        "delay_secs": delay_secs,
                        "message": format!("Timer '{}' set — fires in {}s", name, delay_secs),
                    }),
                }
            }
            Err(e) => KernelActionResult {
                success: false,
                result: serde_json::json!({ "error": e.to_string() }),
            },
        }
    }

    async fn execute_cancel_timer(&self, task: &AgentTask, name: String) -> KernelActionResult {
        // Same ownership rule as `execute_control_schedule`: only the creator may cancel.
        if let Some(entry) = self.schedule_manager.get_timer_by_name(&name).await {
            if self.schedule_manager.creator_of(&entry.id).await != Some(task.agent_id) {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": format!("Permission denied: timer '{}' is not owned by the calling agent", name),
                        "error_kind": "permission_denied",
                    }),
                };
            }
        }
        match self.schedule_manager.cancel_timer_by_name(&name).await {
            Ok(timer) => {
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: agentos_audit::AuditEventType::TimerCancelled,
                    agent_id: Some(task.agent_id),
                    task_id: Some(task.id),
                    tool_id: None,
                    details: serde_json::json!({ "timer_name": timer.name, "timer_id": timer.id.to_string() }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
                KernelActionResult {
                    success: true,
                    result: serde_json::json!({
                        "cancelled": true,
                        "timer_name": timer.name,
                        "timer_id": timer.id.to_string(),
                    }),
                }
            }
            Err(e) => KernelActionResult {
                success: false,
                result: serde_json::json!({ "error": e.to_string() }),
            },
        }
    }

    async fn execute_list_timers(&self, _task: &AgentTask) -> KernelActionResult {
        let timers = self.schedule_manager.list_timers().await;
        let list: Vec<serde_json::Value> = timers
            .iter()
            .map(|t| {
                serde_json::json!({
                    "id": t.id.to_string(),
                    "name": t.name,
                    "agent_name": t.agent_name,
                    "fires_at": t.fire_at.to_rfc3339(),
                })
            })
            .collect();
        KernelActionResult {
            success: true,
            result: serde_json::json!({ "timers": list, "count": list.len() }),
        }
    }

    async fn execute_schedule_once(
        &self,
        task: &AgentTask,
        name: String,
        action: agentos_types::schedule::OnceJobAction,
        agent_name: String,
        fire_at: chrono::DateTime<chrono::Utc>,
    ) -> KernelActionResult {
        let resolved = self.resolve_schedule_agent(task, agent_name).await;

        let action_tag = action.tag();
        match self
            .schedule_manager
            .create_once_job_with_creator(name.clone(), fire_at, resolved, action, task.agent_id)
            .await
        {
            Ok(id) => {
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: agentos_audit::AuditEventType::ScheduledJobCreated,
                    agent_id: Some(task.agent_id),
                    task_id: Some(task.id),
                    tool_id: None,
                    details: serde_json::json!({
                        "job_name": name,
                        "schedule_id": id.to_string(),
                        "fire_at": fire_at.to_rfc3339(),
                        "once": true,
                        "action": action_tag,
                        "source": "agent_tool",
                    }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
                KernelActionResult {
                    success: true,
                    result: serde_json::json!({
                        "job_id": id.to_string(),
                        "job_name": name,
                        "fires_at": fire_at.to_rfc3339(),
                        "action": action_tag,
                        "message": format!("Once-job '{}' scheduled for {} ({})", name, fire_at.to_rfc3339(), action_tag),
                    }),
                }
            }
            Err(e) => KernelActionResult {
                success: false,
                result: serde_json::json!({ "error": e.to_string() }),
            },
        }
    }

    async fn execute_cancel_once_job(&self, task: &AgentTask, name: String) -> KernelActionResult {
        // Same ownership rule as `execute_control_schedule`: only the creator may cancel.
        if let Some(entry) = self.schedule_manager.get_once_job_by_name(&name).await {
            if self.schedule_manager.creator_of(&entry.id).await != Some(task.agent_id) {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": format!("Permission denied: once-job '{}' is not owned by the calling agent", name),
                        "error_kind": "permission_denied",
                    }),
                };
            }
        }
        match self.schedule_manager.cancel_once_job_by_name(&name).await {
            Ok(job) => {
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: agentos_audit::AuditEventType::ScheduledJobDeleted,
                    agent_id: Some(task.agent_id),
                    task_id: Some(task.id),
                    tool_id: None,
                    details: serde_json::json!({ "job_name": job.name, "job_id": job.id.to_string(), "once": true }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
                KernelActionResult {
                    success: true,
                    result: serde_json::json!({
                        "cancelled": true,
                        "job_name": job.name,
                        "job_id": job.id.to_string(),
                    }),
                }
            }
            Err(e) => KernelActionResult {
                success: false,
                result: serde_json::json!({ "error": e.to_string() }),
            },
        }
    }

    async fn execute_list_once_jobs(&self, _task: &AgentTask) -> KernelActionResult {
        let jobs = self.schedule_manager.list_once_jobs().await;
        let list: Vec<serde_json::Value> = jobs
            .iter()
            .map(|j| {
                serde_json::json!({
                    "id": j.id.to_string(),
                    "name": j.name,
                    "agent_name": j.agent_name,
                    "fires_at": j.fire_at.to_rfc3339(),
                })
            })
            .collect();
        KernelActionResult {
            success: true,
            result: serde_json::json!({ "jobs": list, "count": list.len() }),
        }
    }

    async fn execute_get_schedule_runs(
        &self,
        task: &AgentTask,
        schedule_id_str: String,
        limit: u32,
        state_filter: Option<String>,
    ) -> KernelActionResult {
        // Resolve schedule_id from string. Accept either a UUID or a name.
        let schedule_id = match schedule_id_str.parse::<ScheduleID>() {
            Ok(id) => id,
            Err(_) => match self.schedule_manager.get_by_name(&schedule_id_str).await {
                Some(j) => j.id,
                None => {
                    return KernelActionResult {
                        success: false,
                        result: serde_json::json!({
                            "error": format!("Schedule '{}' not found (not a UUID or known name)", schedule_id_str),
                            "error_kind": "not_found",
                        }),
                    };
                }
            },
        };

        // Ownership check: only the creator may inspect run history.
        let creator = self.schedule_manager.creator_of(&schedule_id).await;
        if creator != Some(task.agent_id) {
            return KernelActionResult {
                success: false,
                result: serde_json::json!({
                    "error": format!("Permission denied: schedule '{}' is not owned by the calling agent", schedule_id_str),
                    "error_kind": "permission_denied",
                }),
            };
        }

        let store = match self.schedule_manager.store() {
            Some(s) => s.clone(),
            None => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": "Run-history store not configured on this kernel",
                        "error_kind": "unavailable",
                    }),
                };
            }
        };

        let runs = match store.list_runs_for_schedule(schedule_id, limit).await {
            Ok(r) => r,
            Err(e) => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": e.to_string(),
                        "error_kind": "store_error",
                    }),
                };
            }
        };

        // Validate the optional state filter up front so a bad value fails
        // loudly instead of silently returning an empty list. Accept any case.
        let state_filter = match state_filter {
            Some(s) => match agentos_types::schedule::RunState::parse(&s.to_ascii_lowercase()) {
                Some(rs) => Some(rs),
                None => {
                    return KernelActionResult {
                        success: false,
                        result: serde_json::json!({
                            "error": format!("Invalid state filter '{}'. Valid values: running, complete, failed, missed.", s),
                            "error_kind": "invalid_input",
                        }),
                    };
                }
            },
            None => None,
        };

        let filtered: Vec<&agentos_types::schedule::ScheduledRun> = match state_filter {
            Some(rs) => runs.iter().filter(|r| r.state == rs).collect(),
            None => runs.iter().collect(),
        };

        let out: Vec<serde_json::Value> = filtered
            .iter()
            .map(|r| {
                serde_json::json!({
                    "run_id": r.run_id.to_string(),
                    "schedule_id": r.parent_id.to_string(),
                    "schedule_name": r.parent_name,
                    "state": r.state.as_str(),
                    "started_at": r.started_at.to_rfc3339(),
                    "completed_at": r.completed_at.map(|t| t.to_rfc3339()),
                    "task_id": r.task_id.map(|t| t.to_string()),
                    "error": r.error,
                    "delivered": r.delivered,
                })
            })
            .collect();

        KernelActionResult {
            success: true,
            result: serde_json::json!({
                "runs": out,
                "count": out.len(),
                "schedule_id": schedule_id.to_string(),
            }),
        }
    }

    /// List the calling agent's own schedules across all three kinds
    /// (cron / once / timer). Only schedules whose `creator_agent_id` matches
    /// the caller are returned — operator/CLI-created schedules (creator `None`)
    /// are never visible to agents. `kinds` optionally narrows by kind; an
    /// empty list means all. `include_inactive` adds paused/disabled cron jobs
    /// and fired/cancelled once-jobs (default: active only).
    async fn execute_list_my_schedules(
        &self,
        task: &AgentTask,
        kinds: Vec<String>,
        include_inactive: bool,
    ) -> KernelActionResult {
        let me = task.agent_id;
        let want = |k: &str| kinds.is_empty() || kinds.iter().any(|x| x == k);
        let owned = |c: &Option<AgentID>| *c == Some(me);
        let mut out: Vec<serde_json::Value> = Vec::new();

        // Cron schedules
        if want("cron") || want("schedule") {
            for job in self.schedule_manager.list_jobs().await {
                if !owned(&job.creator_agent_id) {
                    continue;
                }
                let active = job.state == agentos_types::schedule::ScheduleState::Active;
                if !active && !include_inactive {
                    continue;
                }
                out.push(serde_json::json!({
                    "id": job.id.to_string(),
                    "name": job.name,
                    "kind": "cron",
                    "schedule": job.cron_expression,
                    "timezone": job.timezone,
                    "state": serde_json::to_value(job.state).unwrap_or(serde_json::Value::Null),
                    "next_run_at": job.next_run_at.map(|t| t.to_rfc3339()),
                    "last_run_at": job.last_run_at.map(|t| t.to_rfc3339()),
                    "run_count": job.run_count,
                    "delivery": serde_json::to_value(&job.delivery).unwrap_or(serde_json::Value::Null),
                }));
            }
        }

        // One-shot jobs
        if want("once") {
            for job in self.schedule_manager.list_once_jobs().await {
                if !owned(&job.creator_agent_id) {
                    continue;
                }
                let active = job.state == agentos_types::schedule::OnceJobState::Pending;
                if !active && !include_inactive {
                    continue;
                }
                out.push(serde_json::json!({
                    "id": job.id.to_string(),
                    "name": job.name,
                    "kind": "once",
                    "fire_at": job.fire_at.to_rfc3339(),
                    "state": serde_json::to_value(job.state).unwrap_or(serde_json::Value::Null),
                    "delivery": serde_json::to_value(&job.delivery).unwrap_or(serde_json::Value::Null),
                }));
            }
        }

        // Timers (always pending while listed)
        if want("timer") {
            for timer in self.schedule_manager.list_timers().await {
                if !owned(&timer.creator_agent_id) {
                    continue;
                }
                out.push(serde_json::json!({
                    "id": timer.id.to_string(),
                    "name": timer.name,
                    "kind": "timer",
                    "fire_at": timer.fire_at.to_rfc3339(),
                    "state": "pending",
                    "delivery": serde_json::to_value(&timer.delivery).unwrap_or(serde_json::Value::Null),
                }));
            }
        }

        // Cap the response so a prolific agent's schedule list cannot exhaust
        // the context window on a single tool result.
        const MAX_SCHEDULES: usize = 200;
        let total = out.len();
        let truncated = total > MAX_SCHEDULES;
        out.truncate(MAX_SCHEDULES);

        KernelActionResult {
            success: true,
            result: serde_json::json!({
                "schedules": out,
                "count": out.len(),
                "total": total,
                "truncated": truncated,
            }),
        }
    }

    /// Fetch the recorded outcome plus audit-log trail for a single scheduled
    /// run, identified by its `RunID`. Ownership is enforced via the run's
    /// denormalised `creator_agent_id`: an agent may only inspect runs of its
    /// own schedules.
    async fn execute_get_task_logs(
        &self,
        task: &AgentTask,
        run_id_str: String,
    ) -> KernelActionResult {
        let run_id = match run_id_str.parse::<RunID>() {
            Ok(id) => id,
            Err(_) => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": format!("'{}' is not a valid run_id (expected a UUID)", run_id_str),
                        "error_kind": "invalid_input",
                    }),
                };
            }
        };

        let store = match self.schedule_manager.store() {
            Some(s) => s.clone(),
            None => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": "Run-history store not configured on this kernel",
                        "error_kind": "unavailable",
                    }),
                };
            }
        };

        let run = match store.get_run(run_id).await {
            Ok(Some(r)) => r,
            Ok(None) => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": format!("Run '{}' not found", run_id_str),
                        "error_kind": "not_found",
                    }),
                };
            }
            Err(e) => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": e.to_string(),
                        "error_kind": "store_error",
                    }),
                };
            }
        };

        // Ownership check: only the schedule's creator may read its run logs.
        if run.creator_agent_id != Some(task.agent_id) {
            return KernelActionResult {
                success: false,
                result: serde_json::json!({
                    "error": format!("Permission denied: run '{}' is not owned by the calling agent", run_id_str),
                    "error_kind": "permission_denied",
                }),
            };
        }

        // Pull the audit-log trail for the backing task, if one was spawned.
        // The audit query is a synchronous SQLite read, so run it off the async
        // runtime via spawn_blocking (per the "never block the runtime" rule).
        let logs: Vec<String> = match run.task_id {
            Some(tid) => {
                let audit = self.audit.clone();
                tokio::task::spawn_blocking(move || {
                    audit
                        .query_since_for_task(&tid, 0, 500)
                        .map(|entries| {
                            entries
                                .into_iter()
                                .map(|(_, e)| {
                                    format!(
                                        "[{}] {:?} {}",
                                        e.timestamp.format("%Y-%m-%d %H:%M:%S"),
                                        e.event_type,
                                        e.details
                                    )
                                })
                                .collect::<Vec<String>>()
                        })
                        .unwrap_or_default()
                })
                .await
                .unwrap_or_default()
            }
            None => Vec::new(),
        };
        // Direct notify/tool runs have no backing task, hence no audit trail.
        let logs_note = if run.task_id.is_none() {
            Some("No backing task for this run (direct notify/tool action) — no audit trail.")
        } else {
            None
        };

        KernelActionResult {
            success: true,
            result: serde_json::json!({
                "run_id": run.run_id.to_string(),
                "schedule_id": run.parent_id.to_string(),
                "schedule_name": run.parent_name,
                "state": run.state.as_str(),
                "task_id": run.task_id.map(|t| t.to_string()),
                "started_at": run.started_at.to_rfc3339(),
                "completed_at": run.completed_at.map(|t| t.to_rfc3339()),
                "result": run.result,
                "error": run.error,
                "logs": logs,
                "logs_note": logs_note,
            }),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_create_schedule(
        &self,
        task: &AgentTask,
        name: String,
        cron: String,
        agent_name: String,
        mode: String,
        task_prompt: Option<String>,
        notify_subject: Option<String>,
        notify_body: Option<String>,
        notify_priority: Option<String>,
        tool: Option<String>,
        tool_args: Option<serde_json::Value>,
    ) -> KernelActionResult {
        use agentos_types::schedule::OnceJobAction;

        let resolved = self.resolve_schedule_agent(task, agent_name).await;

        // Build typed action. Anti-recursion guard for mode=tool is enforced
        // here too (the tool-layer check is belt-and-suspenders; this is the
        // authoritative gate, since a forged `_kernel_action` payload would
        // otherwise bypass the tool entry point). Single source of truth in
        // `agentos_types::schedule::BLOCKED_SCHEDULE_TOOL_NAMES`.
        use agentos_types::schedule::BLOCKED_SCHEDULE_TOOL_NAMES;

        let action = match mode.as_str() {
            "notify" => {
                let subject = match notify_subject {
                    Some(v) if !v.is_empty() => v,
                    _ => {
                        return KernelActionResult {
                            success: false,
                            result: serde_json::json!({
                                "error": "mode=notify requires notify_subject",
                                "error_kind": "schema_validation",
                            }),
                        };
                    }
                };
                let body = match notify_body {
                    Some(v) if !v.is_empty() => v,
                    _ => {
                        return KernelActionResult {
                            success: false,
                            result: serde_json::json!({
                                "error": "mode=notify requires notify_body",
                                "error_kind": "schema_validation",
                            }),
                        };
                    }
                };
                let priority = notify_priority.unwrap_or_else(|| "info".to_string());
                OnceJobAction::NotifyUser {
                    subject,
                    body,
                    priority,
                }
            }
            "tool" => {
                let tool_name = match tool {
                    Some(v) if !v.is_empty() => v,
                    _ => {
                        return KernelActionResult {
                            success: false,
                            result: serde_json::json!({
                                "error": "mode=tool requires tool",
                                "error_kind": "schema_validation",
                            }),
                        };
                    }
                };
                if BLOCKED_SCHEDULE_TOOL_NAMES.contains(&tool_name.as_str()) {
                    return KernelActionResult {
                        success: false,
                        result: serde_json::json!({
                            "error": format!(
                                "Tool '{}' cannot be scheduled (anti-recursion guard)",
                                tool_name
                            ),
                            "error_kind": "permission_denied",
                        }),
                    };
                }
                let args = tool_args.unwrap_or(serde_json::json!({}));
                OnceJobAction::RunTool {
                    tool: tool_name,
                    args,
                }
            }
            "task" | "" => match task_prompt {
                Some(v) if !v.is_empty() => OnceJobAction::RunTask { prompt: v },
                _ => {
                    return KernelActionResult {
                        success: false,
                        result: serde_json::json!({
                            "error": "mode=task requires task_prompt",
                            "error_kind": "schema_validation",
                        }),
                    };
                }
            },
            other => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": format!(
                            "Unknown mode '{}'. Must be one of [task, notify, tool].",
                            other
                        ),
                        "error_kind": "schema_validation",
                    }),
                };
            }
        };

        let action_tag = action.tag();
        match self
            .schedule_manager
            .create_job_full(
                name.clone(),
                cron.clone(),
                None,
                resolved,
                action,
                vec![],
                task.agent_id,
            )
            .await
        {
            Ok(id) => {
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: Utc::now(),
                    trace_id: TraceID::new(),
                    event_type: agentos_audit::AuditEventType::ScheduledJobCreated,
                    agent_id: Some(task.agent_id),
                    task_id: Some(task.id),
                    tool_id: None,
                    details: serde_json::json!({
                        "job_name": name,
                        "schedule_id": id.to_string(),
                        "cron": cron,
                        "mode": mode,
                        "action": action_tag,
                        "once": false,
                        "creator_agent_id": task.agent_id.to_string(),
                        "source": "agent_tool",
                    }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: true,
                    rollback_ref: None,
                });
                KernelActionResult {
                    success: true,
                    result: serde_json::json!({
                        "schedule_id": id.to_string(),
                        "schedule_name": name,
                        "mode": mode,
                        "action": action_tag,
                        "message": format!("Recurring schedule '{}' created ({})", name, action_tag),
                    }),
                }
            }
            Err(e) => KernelActionResult {
                success: false,
                result: serde_json::json!({
                    "error": e.to_string(),
                    "error_kind": "schema_validation",
                }),
            },
        }
    }

    async fn execute_control_schedule(
        &self,
        task: &AgentTask,
        action: String,
        name: String,
    ) -> KernelActionResult {
        // Resolve target by name across all three kinds. Ownership is checked
        // against the recorded `creator_agent_id` (the agent that called the
        // tool), NOT the target `agent_name` field. This prevents agent A
        // from losing access to its own schedule whenever it spawned a fan-out
        // that targeted a different agent, and prevents agent B from
        // controlling schedules it never created merely because the schedule
        // happens to fire on B.
        let caller = task.agent_id;

        if let Some(job) = self.schedule_manager.get_by_name(&name).await {
            let creator = self.schedule_manager.creator_of(&job.id).await;
            if creator != Some(caller) {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": format!("Permission denied: schedule '{}' is not owned by the calling agent", name),
                        "error_kind": "permission_denied",
                    }),
                };
            }
            let op = match action.as_str() {
                "pause" => self.schedule_manager.pause(&job.id).await,
                "resume" => self.schedule_manager.resume(&job.id).await,
                "delete" => self.schedule_manager.delete(&job.id).await,
                _ => {
                    return KernelActionResult {
                        success: false,
                        result: serde_json::json!({
                            "error": "Unknown action",
                            "error_kind": "schema_validation",
                        }),
                    };
                }
            };
            return match op {
                Ok(_) => KernelActionResult {
                    success: true,
                    result: serde_json::json!({"updated": true, "action": action, "name": name, "kind": "schedule"}),
                },
                Err(e) => KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": e.to_string(),
                        "error_kind": "schema_validation",
                    }),
                },
            };
        }

        // Once-jobs and timers do not support pause/resume. Surface a clear
        // message instead of silently 404ing.
        if let Some(job) = self.schedule_manager.get_once_job_by_name(&name).await {
            let creator = self.schedule_manager.creator_of(&job.id).await;
            if creator != Some(caller) {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": format!("Permission denied: once-job '{}' is not owned by the calling agent", name),
                        "error_kind": "permission_denied",
                    }),
                };
            }
            if action != "delete" {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": format!("Once-jobs only support 'delete'; '{}' is not valid for one-shot schedules", action),
                        "error_kind": "unsupported_action",
                    }),
                };
            }
            return match self.schedule_manager.cancel_once_job_by_name(&name).await {
                Ok(_) => KernelActionResult {
                    success: true,
                    result: serde_json::json!({"updated": true, "action": action, "name": name, "kind": "once"}),
                },
                Err(e) => KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": e.to_string(),
                        "error_kind": "schema_validation",
                    }),
                },
            };
        }
        if let Some(timer) = self.schedule_manager.get_timer_by_name(&name).await {
            let creator = self.schedule_manager.creator_of(&timer.id).await;
            if creator != Some(caller) {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": format!("Permission denied: timer '{}' is not owned by the calling agent", name),
                        "error_kind": "permission_denied",
                    }),
                };
            }
            if action != "delete" {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": format!("Timers only support 'delete'; '{}' is not valid for one-shot timers", action),
                        "error_kind": "unsupported_action",
                    }),
                };
            }
            return match self.schedule_manager.cancel_timer_by_name(&name).await {
                Ok(_) => KernelActionResult {
                    success: true,
                    result: serde_json::json!({"updated": true, "action": action, "name": name, "kind": "timer"}),
                },
                Err(e) => KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": e.to_string(),
                        "error_kind": "schema_validation",
                    }),
                },
            };
        }

        KernelActionResult {
            success: false,
            result: serde_json::json!({
                "error": format!("No matching schedulable item found for '{}'", name),
                "error_kind": "not_found",
            }),
        }
    }

    /// Send a message to a single connected channel by display name or ID.
    ///
    /// Resolution: try as `ChannelInstanceID` UUID first; fall back to a unique
    /// `display_name` match. Ambiguous display_name (multiple matches) returns
    /// an error listing IDs so the agent can disambiguate.
    ///
    /// Dispatch: Telegram/Ntfy/Email are owned by `notification_router`;
    /// Discord/Slack/WhatsApp/Webhook are owned by `channel_manager`. The
    /// kind drives which transport handles the send. On miss, the error
    /// payload includes `available_channels` so the agent can self-correct
    /// in one shot.
    #[allow(clippy::too_many_arguments)]
    async fn execute_channel_send(
        &self,
        task: &AgentTask,
        channel: String,
        text: String,
        thread_id: Option<String>,
        mut attachment: Option<MessageAttachment>,
        file_id: Option<String>,
        file_path: Option<String>,
        caption: Option<String>,
        filename: Option<String>,
        trace_id: TraceID,
    ) -> KernelActionResult {
        use agentos_types::ChannelKind;

        // Defense-in-depth permission check.
        if !task
            .capability_token
            .permissions
            .check(agentos_capability::PERM_CHANNEL_SEND, PermissionOp::Write)
        {
            return KernelActionResult {
                success: false,
                result: serde_json::json!({
                    "error": format!(
                        "Permission denied: '{}:w' required for channel-send",
                        agentos_capability::PERM_CHANNEL_SEND
                    )
                }),
            };
        }

        // Defense-in-depth payload validation (mirrors ChannelSendTool::execute).
        if channel.trim().is_empty() {
            return KernelActionResult {
                success: false,
                result: serde_json::json!({
                    "error": "channel-send 'channel' must be non-empty"
                }),
            };
        }
        if text.is_empty() && attachment.is_none() && file_id.is_none() && file_path.is_none() {
            return KernelActionResult {
                success: false,
                result: serde_json::json!({
                    "error": "channel-send requires non-empty 'text' or an attachment"
                }),
            };
        }

        let registered = match self.channel_registry.list_active().await {
            Ok(list) => list,
            Err(e) => {
                return KernelActionResult {
                    success: false,
                    result: serde_json::json!({
                        "error": format!("Failed to list channels: {e}")
                    }),
                };
            }
        };

        let active_summary: Vec<serde_json::Value> = registered
            .iter()
            .filter(|c| c.active)
            .map(|c| {
                serde_json::json!({
                    "id": c.id.to_string(),
                    "name": c.display_name,
                    "kind": c.kind.to_string(),
                })
            })
            .collect();

        // Lookup priority: try UUID first, then display_name. Ambiguous
        // display_name (matches > 1) is an error so the agent can pick by ID.
        let by_id = registered.iter().find(|c| c.id.to_string() == channel);
        let target = if let Some(c) = by_id {
            c
        } else {
            let by_name: Vec<&agentos_types::RegisteredChannel> = registered
                .iter()
                .filter(|c| c.display_name == channel)
                .collect();
            match by_name.len() {
                0 => {
                    return KernelActionResult {
                        success: false,
                        result: serde_json::json!({
                            "error": format!("Channel '{channel}' not found"),
                            "available_channels": active_summary,
                        }),
                    };
                }
                1 => by_name[0],
                _ => {
                    return KernelActionResult {
                        success: false,
                        result: serde_json::json!({
                            "error": format!(
                                "Channel name '{channel}' is ambiguous ({} matches). Pass the channel ID instead.",
                                by_name.len()
                            ),
                            "matches": by_name
                                .iter()
                                .map(|c| serde_json::json!({
                                    "id": c.id.to_string(),
                                    "name": c.display_name,
                                    "kind": c.kind.to_string(),
                                }))
                                .collect::<Vec<_>>(),
                        }),
                    };
                }
            }
        };

        if !target.active {
            return KernelActionResult {
                success: false,
                result: serde_json::json!({
                    "error": format!("Channel '{channel}' is registered but not active"),
                    "available_channels": active_summary,
                }),
            };
        }

        let target_id = target.id;
        let target_name = target.display_name.clone();
        let target_kind = target.kind.clone();

        // Byte uploads ride `attachment.inline`, which only the Telegram adapter
        // reads; every other adapter gets the (empty) URL and reported
        // "delivered" while nothing arrived. Refuse before reading any file.
        if (file_path.is_some() || file_id.is_some()) && target_kind != ChannelKind::Telegram {
            return KernelActionResult {
                success: false,
                result: serde_json::json!({
                    "error": format!(
                        "file_path/file_id uploads are Telegram-only; channel '{target_name}' is {target_kind}. Send a public image_url/document_url instead."
                    ),
                }),
            };
        }

        let registered_name = {
            let reg = self.agent_registry.read().await;
            reg.get_by_id(&task.agent_id).map(|a| a.name.clone())
        };
        // Audit detail for byte uploads (`file_path` only; `file_id` bytes come
        // from the user's own upload store).
        let mut upload_audit: Option<serde_json::Value> = None;

        // One of the agent's own files. The tool wrapper contained the path to
        // the agent home, but any tool result can carry `_kernel_action` (WASM
        // tools return raw JSON), so re-check against THIS agent's home — not
        // just `data_dir/agents/` — on the open handle, bounded.
        if let Some(path) = file_path {
            let home = agentos_tools::traits::agent_home_dir(
                &self.data_dir,
                registered_name.as_deref(),
                &task.agent_id,
            );
            let path_for_read = std::path::PathBuf::from(&path);
            let read = tokio::task::spawn_blocking(move || {
                read_agent_file_blocking(&home, &path_for_read, CHANNEL_SEND_MAX_FILE_BYTES)
            })
            .await
            .unwrap_or_else(|e| Err(AgentFileError::Other(format!("read task failed: {e}"))));
            let bytes = match read {
                Ok(b) => b,
                Err(AgentFileError::Outside(real)) => {
                    self.audit_log(agentos_audit::AuditEntry {
                        timestamp: Utc::now(),
                        trace_id,
                        event_type: agentos_audit::AuditEventType::PermissionDenied,
                        agent_id: Some(task.agent_id),
                        task_id: Some(task.id),
                        tool_id: None,
                        details: serde_json::json!({
                            "action": "channel_send",
                            "file_path": path,
                            "resolved": real,
                            "reason": "file_path outside the calling agent's home",
                        }),
                        severity: agentos_audit::AuditSeverity::Security,
                        reversible: false,
                        rollback_ref: None,
                    });
                    return KernelActionResult {
                        success: false,
                        result: serde_json::json!({
                            "error": format!("file_path '{path}' is outside your files directory"),
                        }),
                    };
                }
                Err(AgentFileError::Other(e)) => {
                    return KernelActionResult {
                        success: false,
                        result: serde_json::json!({ "error": format!("file_path '{path}': {e}") }),
                    };
                }
            };
            let mime = crate::adapters::telegram::sniff_mime(&bytes).to_string();
            // Telegram sendPhoto caps photos at 10 MB; larger images go as a
            // document (20 MB) instead of failing with HTTP 400.
            let kind =
                if mime.starts_with("image/") && bytes.len() as u64 <= TELEGRAM_PHOTO_MAX_BYTES {
                    AttachmentKind::Image
                } else {
                    AttachmentKind::Document
                };
            upload_audit = Some(serde_json::json!({
                "file_path": path,
                "bytes": bytes.len(),
                "mime": mime,
            }));
            use base64::Engine;
            attachment = Some(MessageAttachment {
                url: String::new(),
                kind,
                filename: filename.clone().or_else(|| {
                    std::path::Path::new(&path)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                }),
                caption: caption.clone(),
                inline: Some(agentos_types::InlineAttachment {
                    mime,
                    data_base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
                }),
                group_urls: Vec::new(),
            });
        } else if let Some(fid) = file_id {
            // Resolve a stored `file_id` to inline bytes via the image resolver
            // (the web FileStore impl), uploaded directly by the Telegram adapter.
            let resolver = self
                .image_resolver
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let fid_for_blocking = fid.clone();
            let resolved = tokio::task::spawn_blocking(move || {
                let bytes = resolver.resolve_base64(&fid_for_blocking);
                let name = resolver.resolve_filename(&fid_for_blocking);
                (bytes, name)
            })
            .await;
            match resolved {
                Ok((Ok((mime, data_base64)), resolved_name)) => {
                    let kind = if mime.starts_with("image/") {
                        AttachmentKind::Image
                    } else {
                        AttachmentKind::Document
                    };
                    attachment = Some(MessageAttachment {
                        url: String::new(),
                        kind,
                        filename: filename.or(resolved_name),
                        caption,
                        inline: Some(agentos_types::InlineAttachment { mime, data_base64 }),
                        group_urls: Vec::new(),
                    });
                }
                Ok((Err(e), _)) => {
                    return KernelActionResult {
                        success: false,
                        result: serde_json::json!({
                            "error": format!("Could not resolve file_id '{fid}': {e}"),
                        }),
                    };
                }
                Err(_) => {
                    return KernelActionResult {
                        success: false,
                        result: serde_json::json!({
                            "error": format!("file_id '{fid}' resolution task failed"),
                        }),
                    };
                }
            }
        }

        let max_chars = match &target_kind {
            ChannelKind::Discord => 2_000,
            ChannelKind::Slack => 40_000,
            ChannelKind::WhatsApp => 4_096,
            ChannelKind::Telegram => 4_096,
            ChannelKind::Ntfy => 4_096,
            ChannelKind::Webhook => 100_000,
            _ => 16_000,
        };
        let send_text: String = text.chars().take(max_chars).collect();

        // One outbound path for every kind: `send_to_channel` routes by where
        // the instance is actually registered, not by a `ChannelKind` match.
        // The old kind-dispatch called `deliver_to_channel`, which returns
        // `Ok(())` when no adapter owns the instance — so a channel left
        // `active: true` by a failed boot restore (e.g. an ntfy topic on
        // `http://ntfy.local`, now rejected by the SSRF blocklist) reported
        // "delivered" to the agent while nothing was ever sent.
        let agent_name = registered_name.unwrap_or_else(|| task.agent_id.to_string());
        // The delivery-stack adapters render "<subject>\n\n<body>"; the
        // manager stack had no subject, so keep it empty there (`outbound_from`
        // then emits the body alone) rather than restyling Discord/Slack.
        let subject_line: String = match &target_kind {
            ChannelKind::Telegram | ChannelKind::Ntfy | ChannelKind::Email => {
                format!("[{agent_name}]").chars().take(80).collect()
            }
            _ => String::new(),
        };
        // Telegram renders the attachment natively (sendPhoto/sendDocument) and
        // the manager-stack adapters either render the URL or auto-embed it.
        // Ntfy/Email have no media handling at all, so fold the URL (and
        // caption, and any album members) into the body rather than dropping it.
        let (msg_body, msg_attachment) = match (&target_kind, &attachment) {
            (ChannelKind::Ntfy | ChannelKind::Email, Some(att)) => {
                let mut b = send_text.clone();
                if let Some(cap) = att.caption.as_deref().filter(|c| !c.is_empty()) {
                    if !b.is_empty() {
                        b.push('\n');
                    }
                    b.push_str(cap);
                }
                if !b.is_empty() {
                    b.push('\n');
                }
                b.push_str(&att.url);
                for extra in &att.group_urls {
                    b.push('\n');
                    b.push_str(extra);
                }
                (b, None)
            }
            _ => (send_text.clone(), attachment.clone()),
        };
        let msg = UserMessage {
            actions: Vec::new(),
            id: NotificationID::new(),
            from: NotificationSource::Agent(task.agent_id),
            task_id: Some(task.id),
            trace_id,
            kind: UserMessageKind::Notification,
            priority: NotificationPriority::Info,
            subject: subject_line,
            body: msg_body,
            interaction: None,
            delivery_status: HashMap::new(),
            response: None,
            created_at: Utc::now(),
            expires_at: None,
            read: false,
            thread_id: thread_id.clone().or_else(|| Some(task.id.to_string())),
            reply_to_external_id: thread_id.clone(),
            attachment: msg_attachment,
        };
        let send_result = self
            .notification_router
            .send_to_channel(msg, &target_id.to_string())
            .await;

        match send_result {
            Ok(()) => {
                let preview: String = send_text.chars().take(120).collect();
                self.audit_log(agentos_audit::AuditEntry {
                    timestamp: Utc::now(),
                    trace_id,
                    event_type: agentos_audit::AuditEventType::ChannelMessageSent,
                    agent_id: Some(task.agent_id),
                    task_id: Some(task.id),
                    tool_id: None,
                    details: serde_json::json!({
                        "channel_id": target_id.to_string(),
                        "channel_name": target_name,
                        "kind": target_kind.to_string(),
                        "thread_id": thread_id,
                        "text_preview": preview,
                        "text_len": text.chars().count(),
                        "sent_text_len": send_text.chars().count(),
                        "has_attachment": attachment.is_some(),
                        "attachment_kind": attachment.as_ref().map(|a| match a.kind {
                            agentos_types::AttachmentKind::Image => "image",
                            agentos_types::AttachmentKind::Document => "document",
                        }),
                        "attachment_url": attachment.as_ref().map(|a| a.url.clone()),
                        "upload": upload_audit,
                    }),
                    severity: agentos_audit::AuditSeverity::Info,
                    reversible: false,
                    rollback_ref: None,
                });
                KernelActionResult {
                    success: true,
                    result: serde_json::json!({
                        "status": "delivered",
                        "channel_id": target_id.to_string(),
                        "channel": target_name,
                        "kind": target_kind.to_string(),
                    }),
                }
            }
            Err(e) => KernelActionResult {
                success: false,
                result: serde_json::json!({
                    "error": e.to_string(),
                    "channel_id": target_id.to_string(),
                    "kind": target_kind.to_string(),
                }),
            },
        }
    }
}

/// Parameters for [`ask_user_blocking`].
pub(crate) struct AskUserArgs {
    pub question: String,
    pub options: Option<Vec<String>>,
    pub timeout_secs: u64,
    pub priority: String,
    pub auto_action: String,
}

/// Deliver a blocking `ask-user` question to the operator and wait for the
/// answer (or the `auto_action` fallback on timeout / kernel shutdown).
///
/// Shared by the task/chat kernel-action path and the Claude MCP gateway, so a
/// `claude-code` agent's question reaches the operator inbox exactly like any
/// other agent's. Does not touch scheduler state — callers own that.
///
/// Returns `Err(result)` with a ready-to-return failure payload when the
/// notification could not be delivered.
pub(crate) async fn ask_user_blocking(
    notification_router: &crate::notification_router::NotificationRouter,
    agent_registry: &tokio::sync::RwLock<crate::agent_registry::AgentRegistry>,
    cancellation_token: &tokio_util::sync::CancellationToken,
    agent_id: AgentID,
    task_id: TaskID,
    trace_id: TraceID,
    args: AskUserArgs,
) -> Result<(NotificationID, UserResponse), KernelActionResult> {
    let AskUserArgs {
        question,
        options,
        timeout_secs,
        priority,
        auto_action,
    } = args;
    let priority_parsed = parse_priority(&priority);
    // Clamp to the range declared in the TOML manifest (10 s – 24 h).
    let timeout_secs = timeout_secs.clamp(10, 86_400);
    let expires_at = Utc::now() + chrono::Duration::seconds(timeout_secs as i64);

    let agent_name = {
        let reg = agent_registry.read().await;
        reg.get_by_id(&agent_id)
            .map(|a| a.name.clone())
            .unwrap_or_else(|| agent_id.to_string())
    };
    let subject_prefixed = format!("[{agent_name}] {}", question.as_str());
    let subject_line: String = subject_prefixed.chars().take(80).collect();
    let body_prefixed = format!("{agent_name} asks:\n\n{question}");

    let msg = UserMessage {
        actions: Vec::new(),
        id: NotificationID::new(),
        from: NotificationSource::Agent(agent_id),
        task_id: Some(task_id),
        trace_id,
        kind: UserMessageKind::Question {
            question: question.clone(),
            options,
            free_text_allowed: true,
        },
        priority: priority_parsed,
        subject: subject_line,
        body: body_prefixed,
        interaction: Some(InteractionRequest {
            blocking: true,
            timeout_secs,
            auto_action: auto_action.clone(),
            max_concurrent: 3,
        }),
        delivery_status: HashMap::new(),
        response: None,
        created_at: Utc::now(),
        expires_at: Some(expires_at),
        read: false,
        thread_id: Some(task_id.to_string()),
        reply_to_external_id: None,
        attachment: None,
    };

    let notification_id = msg.id;

    let rx = match notification_router.deliver(msg).await {
        Ok(Some(rx)) => rx,
        Ok(None) => {
            tracing::error!(
                task_id = %task_id,
                "ask-user: blocking delivery returned no receiver"
            );
            return Err(KernelActionResult {
                success: false,
                result: serde_json::json!({
                    "error": "Internal error: blocking notification returned no receiver"
                }),
            });
        }
        Err(e) => {
            return Err(KernelActionResult {
                success: false,
                result: serde_json::json!({ "error": e.to_string() }),
            });
        }
    };

    tracing::info!(
        task_id = %task_id,
        notification_id = %notification_id,
        timeout_secs,
        "ask-user: awaiting user response"
    );

    let fallback = |text: &str| UserResponse {
        text: text.to_string(),
        responded_at: Utc::now(),
        channel: DeliveryChannel::cli(),
    };
    // Expire on time. The router sweep only runs every 10 min, and until the
    // waiter is removed `InboundRouter` counts it as open — with two stale
    // waiters every free-text channel message was answered "N agents are
    // waiting" and dropped. The sweep stays as a backup.
    let response = tokio::select! {
        result = tokio::time::timeout(Duration::from_secs(timeout_secs), rx) => {
            match result {
                Ok(Ok(resp)) => resp,
                Ok(Err(_recv_err)) => {
                    notification_router.remove_waiting_task(&notification_id).await;
                    fallback(&auto_action)
                }
                Err(_timeout) => {
                    notification_router
                        .expire_waiter(&notification_id, Some(task_id), &auto_action)
                        .await;
                    tracing::info!(
                        task_id = %task_id,
                        notification_id = %notification_id,
                        "ask-user: question expired unanswered; returning auto_action"
                    );
                    fallback(&auto_action)
                }
            }
        }
        _ = cancellation_token.cancelled() => {
            notification_router.remove_waiting_task(&notification_id).await;
            tracing::info!(
                task_id = %task_id,
                "ask-user: kernel shutting down while waiting for user response"
            );
            fallback("kernel_shutdown")
        }
    };
    Ok((notification_id, response))
}

#[cfg(test)]
mod schedule_visibility_parse_tests {
    use super::KernelAction;

    #[test]
    fn parses_list_my_schedules() {
        let v = serde_json::json!({
            "_kernel_action": "list_my_schedules",
            "kinds": ["cron", "once"],
            "include_inactive": true,
        });
        match KernelAction::from_tool_result(&v) {
            Some(KernelAction::ListMySchedules {
                kinds,
                include_inactive,
            }) => {
                assert_eq!(kinds, vec!["cron".to_string(), "once".to_string()]);
                assert!(include_inactive);
            }
            other => panic!("expected ListMySchedules, got {other:?}"),
        }
    }

    #[test]
    fn list_my_schedules_defaults() {
        let v = serde_json::json!({ "_kernel_action": "list_my_schedules" });
        match KernelAction::from_tool_result(&v) {
            Some(KernelAction::ListMySchedules {
                kinds,
                include_inactive,
            }) => {
                assert!(kinds.is_empty());
                assert!(!include_inactive);
            }
            other => panic!("expected ListMySchedules, got {other:?}"),
        }
    }

    #[test]
    fn parses_get_task_logs() {
        let v = serde_json::json!({ "_kernel_action": "get_task_logs", "run_id": "r-1" });
        match KernelAction::from_tool_result(&v) {
            Some(KernelAction::GetTaskLogs { run_id }) => assert_eq!(run_id, "r-1"),
            other => panic!("expected GetTaskLogs, got {other:?}"),
        }
    }

    #[test]
    fn get_task_logs_requires_run_id() {
        let v = serde_json::json!({ "_kernel_action": "get_task_logs" });
        assert!(KernelAction::from_tool_result(&v).is_none());
    }
}

#[cfg(test)]
mod channel_send_file_read_tests {
    use super::{read_agent_file_blocking, AgentFileError};

    #[test]
    fn reads_only_the_callers_own_home_within_the_cap() {
        let data = tempfile::tempdir().unwrap();
        let home_a = data.path().join("agents/A");
        let home_b = data.path().join("agents/B");
        std::fs::create_dir_all(home_a.join("captures")).unwrap();
        std::fs::create_dir_all(&home_b).unwrap();
        std::fs::write(home_a.join("captures/frame.jpg"), b"\xFF\xD8\xFFdata").unwrap();
        std::fs::write(home_b.join("secret.md"), b"b's notes").unwrap();

        // Own file: read.
        let bytes =
            read_agent_file_blocking(&home_a, &home_a.join("captures/frame.jpg"), 1024).ok();
        assert_eq!(bytes.as_deref(), Some(&b"\xFF\xD8\xFFdata"[..]));

        // A forged envelope naming ANOTHER agent's file (still under
        // data_dir/agents/) is refused as Outside.
        assert!(matches!(
            read_agent_file_blocking(&home_a, &home_b.join("secret.md"), 1024),
            Err(AgentFileError::Outside(_))
        ));

        // Over the cap.
        assert!(matches!(
            read_agent_file_blocking(&home_a, &home_a.join("captures/frame.jpg"), 3),
            Err(AgentFileError::Other(_))
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn symlinks_and_fifos_cannot_escape_or_hang() {
        let data = tempfile::tempdir().unwrap();
        let home = data.path().join("agents/A");
        std::fs::create_dir_all(&home).unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("id_ed25519"), b"key").unwrap();

        // Symlinked directory inside the home → the open handle resolves
        // outside, so it is refused even though the path string is "inside".
        std::os::unix::fs::symlink(outside.path(), home.join("captures")).unwrap();
        assert!(matches!(
            read_agent_file_blocking(&home, &home.join("captures/id_ed25519"), 1024),
            Err(AgentFileError::Outside(_))
        ));

        // A FIFO must not block in open(); it is rejected as not a regular file.
        let fifo = home.join("pipe");
        let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        assert!(matches!(
            read_agent_file_blocking(&home, &fifo, 1024),
            Err(AgentFileError::Other(_))
        ));
    }
}
