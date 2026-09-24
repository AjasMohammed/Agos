//! Implementation of [`KernelService`] for the real [`Kernel`].
//!
//! Each method delegates to the appropriate kernel subsystem (agent_registry,
//! scheduler, tool_registry, etc.) and converts internal types into the
//! `Api`-prefixed DTOs defined in `crate::types`.

use crate::error::ApiError;
use crate::service::{CredentialCheck, KernelService};
use crate::types::*;
use agentos_audit::{AuditEntry, AuditEventType, AuditSeverity};
use agentos_kernel::{ChatStreamEvent, Kernel};
use agentos_types::{
    DeliveryChannel, LLMProvider, NotificationID, SecretMetadata, SecretScope, TaskID, TaskState,
    ToolID, UserResponse,
};
use async_trait::async_trait;
use tokio::sync::mpsc;

// ── Stable string serialization helpers ─────────────────────────────────────

fn provider_str(p: &LLMProvider) -> &str {
    match p {
        LLMProvider::Ollama => "ollama",
        LLMProvider::OpenAI => "openai",
        LLMProvider::Anthropic => "anthropic",
        LLMProvider::Gemini => "gemini",
        LLMProvider::Custom(s) => s.as_str(),
    }
}

fn status_str(s: &agentos_types::AgentStatus) -> &str {
    match s {
        agentos_types::AgentStatus::Online => "online",
        agentos_types::AgentStatus::Idle => "idle",
        agentos_types::AgentStatus::Busy => "busy",
        agentos_types::AgentStatus::Offline => "offline",
    }
}

/// `resource:rwxqo` — the exact string `POST /agents/{name}/permissions/revoke`
/// accepts, so the panel can hand a listed row straight back.
fn permission_str(e: &agentos_types::PermissionEntry) -> Option<String> {
    let bits: String = [
        (e.read, 'r'),
        (e.write, 'w'),
        (e.execute, 'x'),
        (e.query, 'q'),
        (e.observe, 'o'),
    ]
    .iter()
    .filter(|(on, _)| *on)
    .map(|(_, c)| *c)
    .collect();
    (!bits.is_empty()).then(|| format!("{}:{}", e.resource, bits))
}

/// Grant/revoke fail on caller input (unknown agent → 404, malformed or
/// role-derived permission → 400) or on the registry write (→ 500, so the
/// client retries instead of blaming its input).
fn permission_cmd_err(message: String) -> ApiError {
    if message.contains("not found") {
        ApiError::NotFound(message)
    } else if message.starts_with("Failed to update permissions") {
        ApiError::Internal(message)
    } else {
        ApiError::BadRequest(message)
    }
}

fn trust_tier_str(t: &agentos_types::TrustTier) -> &str {
    match t {
        agentos_types::TrustTier::Core => "core",
        agentos_types::TrustTier::Verified => "verified",
        agentos_types::TrustTier::Community => "community",
        agentos_types::TrustTier::Blocked => "blocked",
    }
}

fn tool_status_str(s: &agentos_types::ToolStatus) -> &str {
    match s {
        agentos_types::ToolStatus::Available => "available",
        agentos_types::ToolStatus::Running => "running",
        agentos_types::ToolStatus::Disabled => "disabled",
    }
}

// ── Helper conversions ──────────────────────────────────────────────────────

fn parse_provider(s: &str) -> Result<LLMProvider, ApiError> {
    match s.to_lowercase().as_str() {
        "ollama" => Ok(LLMProvider::Ollama),
        "openai" => Ok(LLMProvider::OpenAI),
        "anthropic" => Ok(LLMProvider::Anthropic),
        "gemini" => Ok(LLMProvider::Gemini),
        other => Ok(LLMProvider::Custom(other.to_string())),
    }
}

/// Validate a secret scope string: `global` | `kernel` | `agent:<name>` | `tool:<name>`.
///
/// `agent:`/`tool:` names are resolved to a real `AgentID`/`ToolID` kernel-side from
/// `scope_raw` (same path the CLI uses), so the value returned for them is only a
/// placeholder the kernel ignores. It is `Kernel` — not `Global` — so that if the
/// `scope_raw` wiring ever regressed the secret would be locked to the kernel rather
/// than silently readable by every agent.
fn parse_scope(s: &str) -> Result<SecretScope, ApiError> {
    match s {
        // No empty/default arm: `scope` is required on the wire, so an absent or
        // blank scope is a bad request rather than a silent `global` (readable by
        // every agent on the host).
        _ if s.eq_ignore_ascii_case("global") => Ok(SecretScope::Global),
        _ if s.eq_ignore_ascii_case("kernel") => Ok(SecretScope::Kernel),
        // Names are case-sensitive — match the prefix exactly, like the CLI does.
        _ if s.strip_prefix("agent:").is_some_and(|n| !n.is_empty()) => Ok(SecretScope::Kernel),
        _ if s.strip_prefix("tool:").is_some_and(|n| !n.is_empty()) => Ok(SecretScope::Kernel),
        other => Err(ApiError::BadRequest(format!(
            "Invalid scope '{}'. Use 'global', 'kernel', 'agent:<name>', or 'tool:<name>'",
            other
        ))),
    }
}

/// Truncate to at most `max` chars on a char boundary, for item titles.
fn truncate_title(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

fn episodic_to_item(e: agentos_memory::EpisodicEntry) -> ApiMemoryItem {
    ApiMemoryItem {
        id: e.id.to_string(),
        tier: "episodic".to_string(),
        kind: format!("{:?}", e.entry_type),
        title: e
            .summary
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| truncate_title(&e.content, 80)),
        content: e.content,
        created_at: e.timestamp,
        score: None,
        metadata: e.metadata.unwrap_or_else(|| serde_json::json!({})),
    }
}

fn semantic_entry_to_item(e: agentos_memory::MemoryEntry, score: Option<f32>) -> ApiMemoryItem {
    ApiMemoryItem {
        id: e.id,
        tier: "semantic".to_string(),
        kind: "fact".to_string(),
        title: e.key,
        content: e.full_content,
        created_at: e.created_at,
        score,
        metadata: serde_json::json!({
            "tags": e.tags,
            "use_count": e.use_count,
            "confidence": e.confidence,
        }),
    }
}

fn procedure_to_item(p: agentos_memory::types::Procedure, score: Option<f32>) -> ApiMemoryItem {
    ApiMemoryItem {
        id: p.id,
        tier: "procedural".to_string(),
        kind: "procedure".to_string(),
        title: p.name,
        content: p.description,
        // Procedural browse orders by `updated_at`, so surface that as the item
        // timestamp — otherwise the list would look out of order by the shown time.
        created_at: p.updated_at,
        score,
        metadata: serde_json::json!({
            "success_count": p.success_count,
            "failure_count": p.failure_count,
            "steps": p.steps.len(),
            "status": format!("{:?}", p.status),
        }),
    }
}

fn inbox_target_str(t: &agentos_types::MessageTarget) -> String {
    match t {
        agentos_types::MessageTarget::Direct(a) => format!("direct:{a}"),
        agentos_types::MessageTarget::DirectByName(n) => format!("name:{n}"),
        agentos_types::MessageTarget::Group(g) => format!("group:{g}"),
        agentos_types::MessageTarget::Broadcast => "broadcast".to_string(),
    }
}

fn inbox_content_parts(c: &agentos_types::MessageContent) -> (&'static str, String) {
    match c {
        agentos_types::MessageContent::Text(s) => ("text", s.clone()),
        agentos_types::MessageContent::Structured(v) => ("structured", v.to_string()),
        agentos_types::MessageContent::TaskDelegation { prompt, .. } => {
            ("delegation", prompt.clone())
        }
        agentos_types::MessageContent::TaskResult { task_id, .. } => {
            ("result", format!("result for task {task_id}"))
        }
    }
}

fn inbox_message_to_api(m: agentos_types::AgentMessage) -> ApiInboxMessage {
    let (kind, preview) = inbox_content_parts(&m.content);
    ApiInboxMessage {
        id: m.id.to_string(),
        from: m.from.to_string(),
        to: inbox_target_str(&m.to),
        kind: kind.to_string(),
        preview: truncate_title(&preview, 200),
        reply_to: m.reply_to.map(|r| r.to_string()),
        timestamp: m.timestamp,
        signed: m.signature.is_some(),
    }
}

fn skill_summary(m: &agentos_types::skill::SkillManifest) -> ApiSkillSummary {
    ApiSkillSummary {
        name: m.skill.name.clone(),
        version: m.skill.version.clone(),
        description: m.skill.description.clone(),
        author: m.skill.author.clone(),
        trust_tier: m.skill.trust_tier.clone(),
        roles: m.agent.roles.clone(),
        schedule: m.triggers.schedule.clone(),
        events: m.triggers.events.clone(),
    }
}

fn skill_detail(inst: &agentos_skills::InstalledSkill) -> ApiSkillDetail {
    let m = &inst.manifest;
    ApiSkillDetail {
        summary: skill_summary(m),
        license: m.skill.license.clone(),
        default_provider: m.agent.default_provider.clone(),
        default_model: m.agent.default_model.clone(),
        tools_required: m.tools.required.clone(),
        tools_optional: m.tools.optional.clone(),
        permissions_required: m.permissions.required.clone(),
        max_cost_per_run: m.budget.max_cost_per_run,
        max_tokens_per_run: m.budget.max_tokens_per_run,
        system_prompt: inst.system_prompt.clone(),
    }
}

fn agent_summary(profile: &agentos_types::AgentProfile, supports_images: bool) -> ApiAgentSummary {
    ApiAgentSummary {
        id: profile.id,
        name: profile.name.clone(),
        provider: provider_str(&profile.provider).to_string(),
        model: profile.model.clone(),
        status: status_str(&profile.status).to_string(),
        roles: profile.roles.clone(),
        connected_at: profile.created_at,
        last_active: profile.last_active,
        supports_images,
        avatar: profile.avatar.clone(),
    }
}

/// Max length of a stored avatar data URL. It rides along in `agents.json` and
/// every agent list payload, and `agents.json` is rewritten on every status change
/// or heartbeat; the panel downscales to 256px WebP (~10-30 KB).
const AVATAR_MAX_LEN: usize = 64 * 1024;

/// Accept only a base64 raster image data URL whose bytes match the declared type.
/// SVG is refused: it can carry script, and the value is echoed to every client
/// that lists agents.
fn validate_avatar(url: &str) -> Result<(), ApiError> {
    use base64::Engine;
    if url.len() > AVATAR_MAX_LEN {
        return Err(ApiError::BadRequest("Avatar too large (max 64 KB)".into()));
    }
    let (kind, payload) = ["png", "jpeg", "webp", "gif"]
        .iter()
        .find_map(|t| {
            url.strip_prefix(&format!("data:image/{t};base64,"))
                .map(|p| (*t, p))
        })
        .ok_or_else(|| {
            ApiError::BadRequest(
                "Avatar must be a data:image/{png,jpeg,webp,gif};base64 URL".into(),
            )
        })?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .map_err(|_| ApiError::BadRequest("Avatar is not valid base64".into()))?;
    let magic_ok = match kind {
        "png" => bytes.starts_with(b"\x89PNG"),
        "jpeg" => bytes.starts_with(&[0xFF, 0xD8, 0xFF]),
        "gif" => bytes.starts_with(b"GIF8"),
        _ => bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
    };
    if !magic_ok {
        return Err(ApiError::BadRequest(format!(
            "Avatar data is not a {kind} image"
        )));
    }
    Ok(())
}

/// Resolve an agent path segment that may be either a name or a UUID.
///
/// Every other `/api/v1/agents/{...}` route is keyed by name; memory and inbox
/// were the only two that demanded a UUID, which forced clients to hold the same
/// agent under two different identities. Accepting both keeps existing UUID
/// callers working while letting a client use one identifier throughout.
async fn resolve_agent_id(
    registry: &tokio::sync::RwLock<agentos_kernel::agent_registry::AgentRegistry>,
    id_or_name: &str,
) -> Result<agentos_types::AgentID, ApiError> {
    if let Ok(aid) = id_or_name.parse::<agentos_types::AgentID>() {
        return Ok(aid);
    }
    registry
        .read()
        .await
        .get_by_name(id_or_name)
        .map(|p| p.id)
        .ok_or_else(|| ApiError::NotFound(format!("Agent '{id_or_name}' not found")))
}

fn thinking_level_str(t: &agentos_types::ThinkingLevel) -> &'static str {
    use agentos_types::ThinkingLevel::*;
    match t {
        Off => "off",
        Low => "low",
        Medium => "medium",
        High => "high",
        Max => "max",
    }
}

fn tool_summary(tool: &agentos_types::RegisteredTool) -> ApiToolSummary {
    ApiToolSummary {
        id: tool.id,
        name: tool.manifest.manifest.name.clone(),
        version: tool.manifest.manifest.version.clone(),
        description: tool.manifest.manifest.description.clone(),
        author: tool.manifest.manifest.author.clone(),
        trust_tier: trust_tier_str(&tool.manifest.manifest.trust_tier).to_string(),
        status: tool_status_str(&tool.status).to_string(),
        risk_class: Some(risk_class_str(&tool.manifest.risk_class).to_string()),
        permissions: tool.manifest.capabilities_required.permissions.clone(),
    }
}

/// How long a provider health probe is reused before re-checking.
const PROVIDER_HEALTH_TTL: std::time::Duration = std::time::Duration::from_secs(30);

/// Memoised `health_check` per agent. `GET /api/v1/agents/{name}` is polled by
/// the panel and a probe is not free — the claude-code adapter spawns a
/// subprocess, HTTP adapters hit the provider — so an open dashboard would
/// otherwise issue one probe per refresh per viewer.
async fn cached_provider_health(
    agent_id: agentos_types::AgentID,
    llm: std::sync::Arc<dyn agentos_llm::LLMCore>,
) -> Option<bool> {
    type Cache = std::sync::Mutex<
        std::collections::HashMap<agentos_types::AgentID, (std::time::Instant, bool)>,
    >;
    static CACHE: std::sync::OnceLock<Cache> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);

    if let Ok(guard) = cache.lock() {
        if let Some((at, healthy)) = guard.get(&agent_id) {
            if at.elapsed() < PROVIDER_HEALTH_TTL {
                return Some(*healthy);
            }
        }
    }

    let healthy = tokio::time::timeout(std::time::Duration::from_secs(3), llm.health_check())
        .await
        .ok()
        .map(|h| h.is_healthy())?;

    if let Ok(mut guard) = cache.lock() {
        guard.insert(agent_id, (std::time::Instant::now(), healthy));
    }
    Some(healthy)
}

fn risk_class_str(r: &agentos_types::RiskClass) -> &'static str {
    use agentos_types::RiskClass::*;
    match r {
        ReadonlyScoped => "readonly_scoped",
        ReadonlyExternal => "readonly_external",
        WriteAgentState => "write_agent_state",
        WriteScoped => "write_scoped",
        ExecCapable => "exec_capable",
        ControlPlane => "control_plane",
        Interactive => "interactive",
    }
}

// ── Governance conversion helpers (Phase 04) ────────────────────────────────

fn escalation_to_api(e: agentos_kernel::escalation::PendingEscalation) -> ApiEscalation {
    ApiEscalation {
        id: e.id,
        task_id: e.task_id.to_string(),
        agent_id: e.agent_id.to_string(),
        reason: format!("{:?}", e.reason),
        context_summary: e.context_summary,
        decision_point: e.decision_point,
        options: e.options,
        urgency: e.urgency,
        blocking: e.blocking,
        created_at: e.created_at,
        expires_at: e.expires_at,
        resolved: e.resolved,
        resolution: e.resolution,
        metadata: e.metadata,
    }
}

/// `WorkspaceGrant` → DTO. `mode` is the same short `rwx` string the CLI
/// prints and `POST /workspace-grants` accepts, so a listed row can be handed
/// straight back.
fn workspace_grant_to_api(g: agentos_types::WorkspaceGrant) -> ApiWorkspaceGrant {
    ApiWorkspaceGrant {
        id: g.id,
        path: g.path.to_string_lossy().to_string(),
        agent_id: g.agent_id.map(|a| a.to_string()),
        mode: g.mode.to_string(),
        granted_at: g.granted_at,
        source: g.source,
        granted_by: g.granted_by,
    }
}

/// Map a kernel workspace-command error string onto a status code. The kernel
/// already validated the path and the agent name, so the message is the only
/// signal available: an unknown agent is the caller's fault (404), a duplicate
/// grant is a conflict (409), a rejected mode or path is a bad request (400),
/// and anything else is a store failure (500).
fn workspace_cmd_err(message: String) -> ApiError {
    if message.contains(agentos_kernel::workspace_grant_store::GRANT_DUPLICATE_RESOURCE) {
        ApiError::Conflict(message)
    } else if message.starts_with("Agent not found") {
        ApiError::NotFound(message)
    } else if message.starts_with("invalid mode") || message.contains("fs.workspace_grant") {
        ApiError::BadRequest(message)
    } else {
        ApiError::Internal(message)
    }
}

fn approval_policy_to_api(
    e: agentos_kernel::approval_policy_store::ApprovalPolicyEntry,
) -> ApiApprovalPolicy {
    ApiApprovalPolicy {
        id: e.id,
        tool_name: e.tool_name,
        action: e.action,
        path_glob: e.path_glob,
        agent_id: e.agent_id.map(|a| a.to_string()),
        granted_at: e.granted_at,
        granted_by: e.granted_by,
        source: e.source,
        expires_at: e.expires_at,
    }
}

fn proposal_to_api(p: agentos_kernel::user_pref_proposals::UserPrefProposal) -> ApiPrefProposal {
    use agentos_kernel::user_pref_proposals::{ProposalKind, ProposalStatus};
    let kind = match p.kind {
        ProposalKind::Add => "add",
        ProposalKind::Replace => "replace",
        ProposalKind::Delete => "delete",
    };
    let status = match p.status {
        ProposalStatus::Pending => "pending",
        ProposalStatus::Accepted => "accepted",
        ProposalStatus::Rejected => "rejected",
        ProposalStatus::Expired => "expired",
    };
    ApiPrefProposal {
        id: p.id,
        task_id: p.task_id.to_string(),
        agent_id: p.agent_id.to_string(),
        kind: kind.to_string(),
        content: p.content,
        confidence: p.confidence,
        evidence: p.evidence,
        status: status.to_string(),
        created_at: p.created_at,
        reviewed_at: p.reviewed_at,
    }
}

fn role_to_api(role: &agentos_types::Role) -> ApiRole {
    let permissions = role
        .permissions
        .entries()
        .iter()
        .map(|e| {
            let mut flags = String::new();
            if e.read {
                flags.push('r');
            }
            if e.write {
                flags.push('w');
            }
            if e.execute {
                flags.push('x');
            }
            if e.query {
                flags.push('q');
            }
            if e.observe {
                flags.push('o');
            }
            format!("{}:{}", e.resource, flags)
        })
        .collect();
    ApiRole {
        name: role.name.clone(),
        description: role.description.clone(),
        permissions,
        created_at: role.created_at,
    }
}

// ── Observability conversion + helpers (Phase 07) ───────────────────────────

fn cost_entry_from_snapshot(s: agentos_types::CostSnapshot) -> CostSummaryEntry {
    let budget = CostBudget {
        max_cost_usd_per_day: s.budget.max_cost_usd_per_day,
        max_tokens_per_day: s.budget.max_tokens_per_day,
        spent_today_usd: s.cost_usd,
        pct: s.cost_pct,
    };
    let has_budget = s.budget.max_cost_usd_per_day > 0.0
        || s.budget.max_tokens_per_day > 0
        || s.budget.max_tool_calls_per_day > 0;
    CostSummaryEntry {
        agent_id: s.agent_id,
        agent_name: s.agent_name,
        period_start: s.period_start,
        tokens_used: s.tokens_used,
        cost_usd: s.cost_usd,
        tool_calls: s.tool_calls,
        cost_pct: s.cost_pct,
        tokens_pct: s.tokens_pct,
        tool_calls_pct: s.tool_calls_pct,
        forecast_exhaustion_hours: s.forecast_exhaustion_hours,
        budget: if has_budget { Some(budget) } else { None },
    }
}

/// True when a config key name looks secret-bearing and its value must never
/// leave the process. Shared by the tree redactor and the single-key reader.
fn is_secret_key(k: &str) -> bool {
    let k = k.to_ascii_lowercase();
    k.contains("token") || k.contains("secret") || k.contains("password") || k.contains("api_key")
}

/// Read one dotted key from the config file, secrets redacted.
fn read_config_key(path: &std::path::Path, key: &str) -> Result<serde_json::Value, ApiError> {
    let content = std::fs::read_to_string(path).map_err(|e| {
        ApiError::Internal(format!("Cannot read config at {}: {e}", path.display()))
    })?;
    let doc: toml_edit::DocumentMut = content
        .parse()
        .map_err(|e| ApiError::Internal(format!("Config parse error: {e}")))?;
    let mut value = resolve_dotted_key(&doc, key)?;
    // Redact nested secret-bearing leaves when the key resolves to a table.
    redact_secrets(&mut value);
    // A scalar secret (e.g. `api.operator_token`) resolves to a bare value
    // with no key context for `redact_secrets` to match, so redact it here
    // based on the requested key's own leaf name. Without this, a low-privilege
    // `system:r` caller could read `operator_token` and escalate via /auth/login.
    if key.rsplit('.').next().is_some_and(is_secret_key) && !value.is_null() {
        value = serde_json::Value::String("***REDACTED***".to_string());
    }
    Ok(value)
}

/// Recursively redact leaves whose key name looks secret-bearing.
fn redact_secrets(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                if is_secret_key(k) && !v.is_null() {
                    *v = serde_json::Value::String("***REDACTED***".to_string());
                } else {
                    redact_secrets(v);
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr.iter_mut() {
                redact_secrets(v);
            }
        }
        _ => {}
    }
}

/// Resolve an arbitrary-depth dotted key from a TOML document into a JSON value.
fn resolve_dotted_key(
    doc: &toml_edit::DocumentMut,
    key: &str,
) -> Result<serde_json::Value, ApiError> {
    let parts: Vec<&str> = key.split('.').collect();
    let mut current: &toml_edit::Item = doc.as_item();
    for (i, part) in parts.iter().enumerate() {
        current = current.get(part).ok_or_else(|| {
            ApiError::NotFound(format!("Key '{}' not found", parts[..=i].join(".")))
        })?;
    }
    Ok(toml_item_to_json(current))
}

fn toml_item_to_json(item: &toml_edit::Item) -> serde_json::Value {
    // Tables must become objects, not their TOML text: `redact_secrets` walks
    // object keys, so a stringified `[api]` table carried `operator_token` out
    // in cleartext.
    if let Some(table) = item.as_table_like() {
        serde_json::Value::Object(
            table
                .iter()
                .map(|(k, v)| (k.to_string(), toml_item_to_json(v)))
                .collect(),
        )
    } else if let Some(tables) = item.as_array_of_tables() {
        serde_json::Value::Array(
            tables
                .iter()
                .map(|t| toml_item_to_json(&toml_edit::Item::Table(t.clone())))
                .collect(),
        )
    } else if let Some(array) = item.as_array() {
        serde_json::Value::Array(
            array
                .iter()
                .map(|v| toml_item_to_json(&toml_edit::Item::Value(v.clone())))
                .collect(),
        )
    } else if let Some(s) = item.as_str() {
        serde_json::Value::String(s.to_string())
    } else if let Some(b) = item.as_bool() {
        serde_json::Value::Bool(b)
    } else if let Some(i) = item.as_integer() {
        serde_json::Value::Number(i.into())
    } else if let Some(f) = item.as_float() {
        serde_json::Number::from_f64(f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null)
    } else {
        serde_json::Value::String(item.to_string().trim().to_string())
    }
}

/// Set an arbitrary-depth dotted key, parsing the value as int/float/bool/string.
fn set_dotted_key(
    doc: &mut toml_edit::DocumentMut,
    key: &str,
    value: &str,
) -> Result<(), ApiError> {
    use toml_edit::{Item, Table};
    let parts: Vec<&str> = key.split('.').collect();
    if parts.is_empty() {
        return Err(ApiError::BadRequest("Empty key".to_string()));
    }
    let toml_value = if let Ok(i) = value.parse::<i64>() {
        toml_edit::value(i)
    } else if let Ok(f) = value.parse::<f64>() {
        toml_edit::value(f)
    } else if let Ok(b) = value.parse::<bool>() {
        toml_edit::value(b)
    } else {
        toml_edit::value(value)
    };
    if parts.len() == 1 {
        doc[parts[0]] = toml_value;
        return Ok(());
    }
    let (path_parts, leaf) = parts.split_at(parts.len() - 1);
    let leaf = leaf[0];
    let mut table: &mut Table = doc.as_table_mut();
    for part in path_parts {
        if table.get(part).is_none() {
            table[part] = Item::Table(Table::new());
        }
        table = table[part]
            .as_table_mut()
            .ok_or_else(|| ApiError::BadRequest(format!("'{part}' is not a table")))?;
    }
    table[leaf] = toml_value;
    Ok(())
}

/// Run the doctor checks and map to the API `DoctorCheck` DTO. When `fix` is
/// true, attempts auto-repair of missing directories.
fn doctor_run_checks(
    config_path: &std::path::Path,
    vault_path: &std::path::Path,
    audit_path: &std::path::Path,
    socket_path: &std::path::Path,
    core_tools_dir: &str,
    fix: bool,
) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    checks.push(if config_path.exists() {
        doctor_pass(
            "Config file exists",
            format!("Found at {}", config_path.display()),
        )
    } else {
        doctor_fail(
            "Config file exists",
            format!("Not found at {}", config_path.display()),
            true,
        )
    });

    checks.push(match std::fs::read_to_string(config_path) {
        Ok(content) => match content.parse::<toml_edit::DocumentMut>() {
            Ok(_) => doctor_pass("Config valid TOML", "Parses as valid TOML".to_string()),
            Err(e) => doctor_fail("Config valid TOML", format!("TOML parse error: {e}"), false),
        },
        Err(_) => doctor_warn(
            "Config valid TOML",
            "Config file missing — skipping parse check".to_string(),
        ),
    });

    checks.push(doctor_dir_writable(
        "Vault database directory",
        vault_path,
        fix,
    ));
    checks.push(doctor_dir_writable("Audit log directory", audit_path, fix));
    checks.push(doctor_socket_dir("IPC socket directory", socket_path, fix));
    checks.push(doctor_tools_dir("Core tool manifests", core_tools_dir));

    checks
}

fn doctor_pass(name: &str, detail: String) -> DoctorCheck {
    DoctorCheck {
        name: name.to_string(),
        status: "pass".to_string(),
        detail,
        fixable: false,
    }
}
fn doctor_warn(name: &str, detail: String) -> DoctorCheck {
    DoctorCheck {
        name: name.to_string(),
        status: "warn".to_string(),
        detail,
        fixable: true,
    }
}
fn doctor_fail(name: &str, detail: String, fixable: bool) -> DoctorCheck {
    DoctorCheck {
        name: name.to_string(),
        status: "fail".to_string(),
        detail,
        fixable,
    }
}

fn doctor_dir_writable(name: &str, path: &std::path::Path, fix: bool) -> DoctorCheck {
    let parent = path.parent().unwrap_or(std::path::Path::new("."));
    if !parent.exists() {
        if fix {
            return match std::fs::create_dir_all(parent) {
                Ok(_) => doctor_pass(name, format!("Created directory {}", parent.display())),
                Err(e) => doctor_fail(
                    name,
                    format!("Cannot create {}: {e}", parent.display()),
                    false,
                ),
            };
        }
        return doctor_warn(
            name,
            format!("Directory does not exist: {}", parent.display()),
        );
    }
    let probe = parent.join(".agentos_write_probe");
    match std::fs::write(&probe, b"") {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            doctor_pass(name, format!("{} exists and is writable", parent.display()))
        }
        Err(e) => doctor_fail(
            name,
            format!("{} exists but is not writable: {e}", parent.display()),
            false,
        ),
    }
}

fn doctor_socket_dir(name: &str, socket_path: &std::path::Path, fix: bool) -> DoctorCheck {
    if socket_path.exists() {
        return doctor_pass(
            name,
            format!("Socket {} exists (kernel running)", socket_path.display()),
        );
    }
    let socket_dir = socket_path.parent().unwrap_or(socket_path);
    if socket_dir.exists() {
        doctor_pass(
            name,
            format!("{} exists (socket dir ready)", socket_dir.display()),
        )
    } else if fix {
        match std::fs::create_dir_all(socket_dir) {
            Ok(_) => doctor_pass(name, format!("Created {}", socket_dir.display())),
            Err(e) => doctor_fail(name, format!("Failed to create socket dir: {e}"), false),
        }
    } else {
        doctor_warn(
            name,
            format!("{} not found (kernel not running?)", socket_dir.display()),
        )
    }
}

fn doctor_tools_dir(name: &str, core_tools_dir: &str) -> DoctorCheck {
    let tools_dir = std::path::PathBuf::from(core_tools_dir);
    if !tools_dir.exists() {
        return doctor_warn(name, format!("{} directory not found", tools_dir.display()));
    }
    let count = std::fs::read_dir(&tools_dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().extension().map(|x| x == "toml").unwrap_or(false))
                .count()
        })
        .unwrap_or(0);
    if count == 0 {
        doctor_fail(
            name,
            "No .toml tool manifests found in tools/core/".to_string(),
            false,
        )
    } else {
        doctor_pass(name, format!("{count} core tool manifests found"))
    }
}

/// Read the newest kernel log lines from the tracing daily-rolling files in
/// `log_dir` (`agentos.log.YYYY-MM-DD`). Lines look like
/// `2026-08-28T14:41:10.581647Z  WARN agentos_kernel::x: crates/.../x.rs:12: msg`.
/// Returns the last `limit` matching lines, oldest first.
fn query_logs_dir(
    log_dir: &str,
    level: Option<String>,
    since: Option<String>,
    limit: u32,
) -> Vec<LogLine> {
    use std::io::BufRead;
    let level = level.unwrap_or_default().to_lowercase();
    let since_dt = since
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));
    // Bounded: this is a tail, not an export. Also caps memory for hostile `limit`.
    let limit = limit.clamp(1, 1000) as usize;

    if log_dir.is_empty() {
        return Vec::new();
    }
    // Newest two daily files are enough for any sane `limit`; sort by name
    // (date suffix) so the tail is chronological.
    let mut files: Vec<std::path::PathBuf> = match std::fs::read_dir(log_dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("agentos.log"))
            })
            .collect(),
        Err(_) => return Vec::new(),
    };
    files.sort();
    let recent: Vec<_> = files.into_iter().rev().take(2).collect::<Vec<_>>();

    let mut results: std::collections::VecDeque<LogLine> = std::collections::VecDeque::new();
    for path in recent.into_iter().rev() {
        let Ok(mut file) = std::fs::File::open(&path) else {
            continue;
        };
        // Only scan the tail of each file: the panel polls this endpoint and a
        // busy day's log can be tens of MB.
        const TAIL_BYTES: u64 = 2 * 1024 * 1024;
        if let Ok(meta) = file.metadata() {
            if meta.len() > TAIL_BYTES {
                use std::io::Seek;
                let _ = file.seek(std::io::SeekFrom::Start(meta.len() - TAIL_BYTES));
            }
        }
        // Read raw bytes with a lossy decode rather than `lines()`: the tail seek
        // above can land mid-UTF-8, and `Lines` surfaces that as an error —
        // which `map_while` treats as EOF (dropping the whole tail) and
        // `filter_map` can spin on (clippy::lines_filter_map_ok). Decoding
        // lossily means the partial first line is merely unparseable, which the
        // RFC3339 guard in `parse_line` already rejects, and a real I/O error
        // ends this file instead of looping.
        let mut reader = std::io::BufReader::new(file);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) => break, // EOF
                Ok(_) => {}
                Err(_) => break, // real I/O error: stop reading this file
            }
            let raw = String::from_utf8_lossy(&buf);
            let Some(parsed) = parse_line(raw.trim_end()) else {
                continue;
            };
            if !level.is_empty() && !parsed.severity.to_lowercase().contains(&level) {
                continue;
            }
            if let Some(bound) = since_dt {
                let ts = chrono::DateTime::parse_from_rfc3339(&parsed.timestamp)
                    .ok()
                    .map(|dt| dt.with_timezone(&chrono::Utc));
                if ts.is_some_and(|ts| ts < bound) {
                    continue;
                }
            }
            results.push_back(parsed);
            if results.len() > limit {
                results.pop_front();
            }
        }
    }
    results.into()
}

/// Parse a line written by either the text or the JSON tracing layer
/// (`logging.log_format`), so the endpoint never goes blank on a config change.
fn parse_line(raw: &str) -> Option<LogLine> {
    parse_json_line(raw).or_else(|| parse_tracing_line(raw))
}

/// JSON layer: `{"timestamp":..,"level":..,"target":..,"fields":{"message":..,..},..}`.
fn parse_json_line(raw: &str) -> Option<LogLine> {
    let trimmed = raw.trim_start();
    if !trimmed.starts_with('{') {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let timestamp = v.get("timestamp")?.as_str()?.to_string();
    let severity = v.get("level")?.as_str()?.to_string();
    let target = v
        .get("target")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();
    let message = v
        .get("fields")
        .and_then(|f| f.get("message"))
        .or_else(|| v.get("message"))
        .and_then(|m| m.as_str())
        .unwrap_or("");
    Some(LogLine {
        timestamp,
        severity,
        event_type: target.clone(),
        line: if target.is_empty() {
            message.to_string()
        } else {
            format!("{target}: {message}")
        },
    })
}

/// Parse one tracing text line: `<rfc3339>  <LEVEL> <target>: <rest>`.
/// ANSI escapes are stripped so a colourised file still parses.
fn parse_tracing_line(raw: &str) -> Option<LogLine> {
    let line = strip_ansi(raw);
    let line = line.trim_start();
    let mut parts = line
        .splitn(3, char::is_whitespace)
        .filter(|s| !s.is_empty());
    let timestamp = parts.next()?;
    chrono::DateTime::parse_from_rfc3339(timestamp).ok()?;
    let rest = line[timestamp.len()..].trim_start();
    let (severity, rest) = rest.split_once(char::is_whitespace)?;
    if !matches!(severity, "TRACE" | "DEBUG" | "INFO" | "WARN" | "ERROR") {
        return None;
    }
    let rest = rest.trim_start();
    // target ends at the first ": " (span prefixes like `execute_task{..}:` are
    // part of the target for display purposes).
    let event_type = rest
        .split_once(": ")
        .map(|(t, _)| t)
        .unwrap_or("")
        .to_string();
    Some(LogLine {
        timestamp: timestamp.to_string(),
        severity: severity.to_string(),
        event_type,
        line: rest.to_string(),
    })
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for d in chars.by_ref() {
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod config_redaction_tests {
    use super::*;

    /// 2026-07-20 C1: a `system:r` key read `api.operator_token` through the
    /// single-key getter and escalated via /auth/login.
    #[test]
    fn single_key_read_never_returns_a_secret() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[api]\noperator_token = \"hunter2\"\nport = 8080\n\
             [[hooks]]\nname = \"a\"\nsecret = \"hunter2\"\n",
        )
        .unwrap();

        let scalar = read_config_key(&path, "api.operator_token").unwrap();
        assert_eq!(scalar, serde_json::json!("***REDACTED***"));
        let table = read_config_key(&path, "api").unwrap();
        assert_eq!(table["operator_token"], "***REDACTED***");
        assert!(!table.to_string().contains("hunter2"));
        // Arrays of tables are walked too, not stringified.
        let hooks = read_config_key(&path, "hooks").unwrap();
        assert_eq!(hooks[0]["name"], "a");
        assert!(!hooks.to_string().contains("hunter2"));
        // Non-secret siblings still read normally.
        assert_eq!(read_config_key(&path, "api.port").unwrap(), 8080);
    }
}

#[cfg(test)]
mod log_parse_tests {
    use super::*;

    #[test]
    fn parses_plain_and_ansi_lines() {
        let l = parse_tracing_line("2026-08-28T14:41:10.581647Z  WARN agentos_kernel::commands::agent: crates/agentos-kernel/src/commands/agent.rs:1860: Auto-reactivation: LLM backend not fully healthy").unwrap();
        assert_eq!(l.severity, "WARN");
        assert_eq!(l.event_type, "agentos_kernel::commands::agent");
        assert!(l.line.ends_with("not fully healthy"));
        let a = parse_tracing_line(
            "\u{1b}[2m2026-08-28T14:41:10.5Z\u{1b}[0m \u{1b}[32m INFO\u{1b}[0m agentos::chat: hi",
        )
        .unwrap();
        assert_eq!(a.severity, "INFO");
        assert_eq!(a.event_type, "agentos::chat");
        assert!(parse_tracing_line("not a log line").is_none());
        let j = parse_line(r#"{"timestamp":"2026-08-28T14:41:10.5Z","level":"ERROR","target":"agentos_kernel::x","fields":{"message":"boom"}}"#).unwrap();
        assert_eq!(j.severity, "ERROR");
        assert_eq!(j.event_type, "agentos_kernel::x");
        assert_eq!(j.line, "agentos_kernel::x: boom");
    }

    #[test]
    fn reads_tail_with_level_filter() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("agentos.log.2026-08-28");
        std::fs::write(
            &f,
            "2026-08-28T10:00:00Z  INFO a::b: one\n2026-08-28T10:00:01Z  WARN a::b: two\n2026-08-28T10:00:02Z ERROR a::b: three\n",
        )
        .unwrap();
        let all = query_logs_dir(dir.path().to_str().unwrap(), None, None, 2);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].severity, "WARN");
        let warn = query_logs_dir(dir.path().to_str().unwrap(), Some("warn".into()), None, 10);
        assert_eq!(warn.len(), 1);
    }

    /// A >2 MB file is read from a byte offset that can land mid-UTF-8. The
    /// partial first line must be dropped WITHOUT costing us the rest of the
    /// file (the `map_while` bug) or spinning (the `filter_map` bug).
    #[test]
    fn tail_seek_mid_utf8_still_returns_later_lines() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("agentos.log.2026-08-29");
        // Pad past the 2 MiB tail window with multi-byte chars so the seek
        // almost certainly lands inside one.
        let filler = "2026-08-29T10:00:00Z  INFO a::b: ——————————————————————\n";
        let mut content = filler.repeat(3 * 1024 * 1024 / filler.len());
        content.push_str("2026-08-29T10:00:01Z ERROR a::b: last line\n");
        std::fs::write(&f, content).unwrap();

        let out = query_logs_dir(dir.path().to_str().unwrap(), None, None, 5);
        assert!(!out.is_empty(), "tail seek must not drop the whole file");
        assert_eq!(out.last().unwrap().line, "a::b: last line");
    }
}

// ── Automation conversion helpers (Phase 03) ────────────────────────────────

fn delivery_mode_tag(d: &agentos_types::delivery::DeliveryMode) -> &'static str {
    match d {
        agentos_types::delivery::DeliveryMode::Silent => "silent",
        agentos_types::delivery::DeliveryMode::Direct { .. } => "direct",
        agentos_types::delivery::DeliveryMode::ViaAgent { .. } => "via_agent",
    }
}

fn schedule_to_api(j: &agentos_types::ScheduledJob) -> ApiScheduleSummary {
    ApiScheduleSummary {
        id: j.id.to_string(),
        name: j.name.clone(),
        agent_name: j.agent_name.clone(),
        kind: "cron".to_string(),
        cron: Some(j.cron_expression.clone()),
        state: (&j.state).into(),
        prompt: j.task_prompt.clone(),
        run_count: j.run_count,
        last_run_at: j.last_run_at,
        next_run_at: j.next_run_at,
        delivery_mode: delivery_mode_tag(&j.delivery).to_string(),
    }
}

/// Human-readable one-liner for what fires when a once-job triggers.
fn once_action_summary(a: &agentos_types::OnceJobAction) -> String {
    use agentos_types::OnceJobAction;
    match a {
        OnceJobAction::RunTask { prompt } => prompt.clone(),
        OnceJobAction::NotifyUser { subject, .. } => format!("Notify: {subject}"),
        OnceJobAction::RunTool { tool, .. } => format!("Run tool: {tool}"),
    }
}

/// Human-readable one-liner for what fires when a timer triggers.
fn timer_action_summary(a: &agentos_types::TimerAction) -> String {
    use agentos_types::TimerAction;
    match a {
        TimerAction::RunTask { prompt } | TimerAction::RunTaskAndNotify { prompt, .. } => {
            prompt.clone()
        }
        TimerAction::NotifyUser { subject, .. } => format!("Notify: {subject}"),
        TimerAction::RunTool { tool, .. } => format!("Run tool: {tool}"),
    }
}

fn once_job_to_api(j: &agentos_types::OnceJob) -> ApiScheduleSummary {
    use agentos_types::OnceJobState;
    ApiScheduleSummary {
        id: j.id.to_string(),
        name: j.name.clone(),
        agent_name: j.agent_name.clone(),
        kind: "once".to_string(),
        cron: None,
        state: match j.state {
            OnceJobState::Pending => ApiScheduleState::Pending,
            OnceJobState::Fired => ApiScheduleState::Fired,
            OnceJobState::Cancelled => ApiScheduleState::Cancelled,
        },
        prompt: once_action_summary(&j.action),
        run_count: 0,
        last_run_at: None,
        next_run_at: Some(j.fire_at),
        delivery_mode: delivery_mode_tag(&j.delivery).to_string(),
    }
}

fn timer_to_api(t: &agentos_types::TimerEntry) -> ApiScheduleSummary {
    ApiScheduleSummary {
        id: t.id.to_string(),
        name: t.name.clone(),
        agent_name: t.agent_name.clone(),
        kind: "timer".to_string(),
        cron: None,
        state: ApiScheduleState::Pending,
        prompt: timer_action_summary(&t.action),
        run_count: 0,
        last_run_at: None,
        next_run_at: Some(t.fire_at),
        delivery_mode: delivery_mode_tag(&t.delivery).to_string(),
    }
}

/// Reject workflow ids that could escape the workflows dir.
fn validate_workflow_id(id: &str) -> Result<(), ApiError> {
    if id.is_empty()
        || id.len() > 128
        || id.contains("..")
        || id.contains('/')
        || id.contains('\\')
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(ApiError::BadRequest(format!("Invalid workflow id: {id}")));
    }
    Ok(())
}

// ── Extensibility conversion helpers (Phase 05) ─────────────────────────────

/// A plugin whose manifest lives under `plugins/user` was installed by an
/// operator and may be removed; anything else ships with AgentOS.
fn is_user_plugin(
    p: &agentos_kernel::plugin_registry::PluginEntry,
    user_dir: &std::path::Path,
) -> bool {
    p.manifest_path.starts_with(user_dir)
}

fn plugin_to_summary(
    p: agentos_kernel::plugin_registry::PluginEntry,
    user_dir: &std::path::Path,
) -> ApiPluginSummary {
    let (status, blocked_reason) = plugin_status_parts(&p.status);
    let user_installed = is_user_plugin(&p, user_dir);
    ApiPluginSummary {
        id: p.manifest.id.clone(),
        display_name: p.manifest.display_name.clone(),
        version: p.manifest.version.clone(),
        description: p.manifest.description.clone(),
        trust_tier: trust_tier_str(&p.manifest.trust_tier).to_string(),
        status,
        blocked_reason,
        channels: p.manifest.channels.iter().map(|c| c.id.clone()).collect(),
        tools: p.manifest.tools.clone(),
        user_installed,
    }
}

fn plugin_to_detail(
    p: agentos_kernel::plugin_registry::PluginEntry,
    user_dir: &std::path::Path,
) -> ApiPluginDetail {
    let (status, blocked_reason) = plugin_status_parts(&p.status);
    let user_installed = is_user_plugin(&p, user_dir);
    ApiPluginDetail {
        id: p.manifest.id.clone(),
        display_name: p.manifest.display_name.clone(),
        version: p.manifest.version.clone(),
        description: p.manifest.description.clone(),
        author: p.manifest.author.clone(),
        trust_tier: trust_tier_str(&p.manifest.trust_tier).to_string(),
        status,
        blocked_reason,
        channels: p.manifest.channels.iter().map(|c| c.id.clone()).collect(),
        tools: p.manifest.tools.clone(),
        permissions: p.manifest.permissions.clone(),
        memory_backend: p.manifest.memory_backend,
        user_installed,
        // Only the operator-installed ones are editable, so don't read a core
        // plugin's file just to show it greyed out.
        manifest_toml: user_installed
            .then(|| std::fs::read_to_string(&p.manifest_path).ok())
            .flatten(),
    }
}

/// A caller-named channel credential key must stay inside the channel
/// namespace.
///
/// `credential_key` is written to the vault verbatim, and `channels:w` is a much
/// weaker scope than `secrets:w` — without this, a channel request could name
/// `mcp.<server>.auth_token`, an agent secret, or any other key and overwrite
/// it. Derived keys already have this shape (`channel.<kind>.<slug>`).
fn validate_channel_credential_key(key: &str) -> Result<(), ApiError> {
    if key.starts_with("channel.") && !key.contains("..") {
        Ok(())
    } else {
        Err(ApiError::BadRequest(format!(
            "`credential_key` must be a `channel.*` vault key (got '{key}')"
        )))
    }
}

/// Shared shape check for attach and update.
///
/// The kernel's own guard is `auth_token XOR oauth_connector_id`; transport
/// exclusivity is only enforced by whichever field it reads first, so an
/// ambiguous request must be rejected here rather than silently resolved.
fn validate_attach_request(req: &AttachMcpRequest) -> Result<(), ApiError> {
    if !agentos_kernel::plugin_registry::valid_plugin_id(req.name.trim()) {
        return Err(ApiError::BadRequest(
            "Invalid server name: use letters, digits, '-' or '_' (max 64)".into(),
        ));
    }
    let has_cmd = req.command.as_deref().is_some_and(|c| !c.trim().is_empty());
    let has_url = req.url.as_deref().is_some_and(|u| !u.trim().is_empty());
    if has_cmd == has_url {
        return Err(ApiError::BadRequest(
            "Provide exactly one of `command` (stdio) or `url` (http)".into(),
        ));
    }
    if req.auth_token.is_some() && req.oauth_connector_id.is_some() {
        return Err(ApiError::BadRequest(
            "`auth_token` and `oauth_connector_id` are mutually exclusive".into(),
        ));
    }
    Ok(())
}

fn plugin_status_parts(
    status: &agentos_kernel::plugin_registry::PluginStatus,
) -> (String, Option<String>) {
    use agentos_kernel::plugin_registry::PluginStatus;
    match status {
        PluginStatus::Discovered => ("discovered".to_string(), None),
        PluginStatus::Active => ("active".to_string(), None),
        PluginStatus::Disabled => ("disabled".to_string(), None),
        PluginStatus::Blocked { reason } => ("blocked".to_string(), Some(reason.clone())),
    }
}

fn channel_to_summary(
    ch: agentos_types::RegisteredChannel,
    health: &std::collections::HashMap<String, String>,
) -> ApiChannelSummary {
    let id = ch.id.to_string();
    let health_status = health.get(&id).cloned();
    ApiChannelSummary {
        id,
        kind: ch.kind.to_string(),
        display_name: ch.display_name,
        external_id: ch.external_id,
        reply_topic: ch.reply_topic,
        server_url: ch.server_url,
        webhook_url: ch.webhook_url,
        connected_at: ch.connected_at,
        last_active: ch.last_active,
        health: health_status,
        active_agent_name: ch.active_agent_name,
    }
}

/// Map a kernel channel-command failure onto an HTTP status: a missing channel
/// or agent is 404, a malformed id 400, anything else a conflict.
fn channel_err(message: String) -> ApiError {
    if message.contains("not found") || message.starts_with("Unknown agent") {
        ApiError::NotFound(message)
    } else if message.starts_with("Invalid") || message.contains("cannot be empty") {
        ApiError::BadRequest(message)
    } else if message.starts_with("Failed to") {
        // Registry/vault I/O — the client cannot fix its request, so don't tell
        // it to; 409 would send it into a retry-with-different-input loop.
        ApiError::Internal(message)
    } else {
        ApiError::Conflict(message)
    }
}

fn subscription_to_api(s: agentos_types::EventSubscription) -> ApiEventSubscription {
    ApiEventSubscription {
        id: s.id.to_string(),
        agent_id: s.agent_id.to_string(),
        event_type_filter: format!("{:?}", s.event_type_filter),
        payload_filter: s.filter,
        priority: format!("{:?}", s.priority),
        throttle: format!("{:?}", s.throttle),
        enabled: s.enabled,
        created_at: s.created_at,
    }
}

fn webhook_to_api(w: agentos_types::WebhookEndpointMeta) -> ApiWebhookEndpoint {
    let inbound_url = format!("/api/v1/webhooks/incoming/{}", w.id);
    ApiWebhookEndpoint {
        id: w.id.to_string(),
        agent_id: w.agent_id.to_string(),
        provider: w.provider,
        active: w.active,
        debounce_seconds: w.debounce_seconds,
        total_received: w.total_received,
        created_at: w.created_at,
        last_received_at: w.last_received_at,
        inbound_url,
    }
}

fn parse_webhook_provider(p: &str) -> Option<agentos_types::WebhookProvider> {
    match p.trim().to_ascii_lowercase().as_str() {
        "github" => Some(agentos_types::WebhookProvider::GitHub),
        "stripe" => Some(agentos_types::WebhookProvider::Stripe),
        "slack" => Some(agentos_types::WebhookProvider::Slack),
        "pagerduty" => Some(agentos_types::WebhookProvider::PagerDuty),
        "generic" => Some(agentos_types::WebhookProvider::Generic),
        _ => None,
    }
}

/// Parse a throttle string like "once_per:30s" or "max:5/60s".
fn parse_throttle_str(s: &str) -> Option<agentos_types::ThrottlePolicy> {
    use agentos_types::ThrottlePolicy;
    fn dur(s: &str) -> Option<std::time::Duration> {
        let s = s.trim();
        let (num, mult) = if let Some(n) = s.strip_suffix('s') {
            (n, 1)
        } else if let Some(n) = s.strip_suffix('m') {
            (n, 60)
        } else if let Some(n) = s.strip_suffix('h') {
            (n, 3600)
        } else {
            (s, 1)
        };
        num.parse::<u64>()
            .ok()
            .map(|n| std::time::Duration::from_secs(n * mult))
    }
    if let Some(d) = s.strip_prefix("once_per:") {
        return dur(d).map(ThrottlePolicy::MaxOncePerDuration);
    }
    if let Some(rest) = s.strip_prefix("max:") {
        let parts: Vec<&str> = rest.splitn(2, '/').collect();
        if parts.len() != 2 {
            return None;
        }
        let count: u32 = parts[0].parse().ok()?;
        return dur(parts[1]).map(|d| ThrottlePolicy::MaxCountPerDuration(count, d));
    }
    None
}

// ── Files + scratchpad conversion helpers (Phase 06) ────────────────────────

fn file_meta_from(f: agentos_kernel::file_store::UploadedFile) -> ApiFileMeta {
    ApiFileMeta {
        id: f.id,
        name: f.name,
        original_name: f.original_name,
        mime: f.mime,
        size: f.size,
        scope: f.scope,
        tags: f.tags,
        uploaded_at: f.uploaded_at,
    }
}

/// Download-safe Content-Type allowlist. Anything not listed becomes
/// `application/octet-stream` to prevent stored-XSS on download.
fn safe_download_mime(mime: &str) -> String {
    let lower = mime.to_lowercase();
    // SVG is the one `image/*` type that is active content (can carry inline
    // <script>); never serve it with its declared type even as an attachment.
    if lower.starts_with("image/svg") {
        return "application/octet-stream".to_string();
    }
    let allowed = lower == "application/octet-stream"
        || lower == "application/pdf"
        || lower == "application/zip"
        || lower == "application/gzip"
        || lower.starts_with("image/")
        || lower.starts_with("audio/")
        || lower.starts_with("video/")
        || lower.starts_with("text/plain")
        || lower.starts_with("text/csv")
        || lower.starts_with("text/markdown")
        || lower.starts_with("text/x-")
        || lower.starts_with("application/json")
        || lower.starts_with("application/x-ndjson");
    if allowed {
        mime.to_string()
    } else {
        "application/octet-stream".to_string()
    }
}

fn scratch_summary_to_api(p: agentos_scratch::PageSummary) -> ApiPageSummary {
    ApiPageSummary {
        id: p.id,
        title: p.title,
        tags: p.tags,
        updated_at: p.updated_at.to_rfc3339(),
    }
}

fn scratch_page_to_api(
    p: agentos_scratch::ScratchPage,
    backlinks: Vec<agentos_scratch::PageSummary>,
) -> ApiScratchPage {
    ApiScratchPage {
        id: p.id,
        agent_id: p.agent_id,
        title: p.title,
        content: p.content,
        tags: p.tags,
        created_at: p.created_at.to_rfc3339(),
        updated_at: p.updated_at.to_rfc3339(),
        backlinks: backlinks.into_iter().map(scratch_summary_to_api).collect(),
    }
}

// ── Conversational conversion helpers (Phase 02) ────────────────────────────

fn api_chat_message_from(m: agentos_kernel::chat_store::ChatMessage) -> ApiChatMessage {
    ApiChatMessage {
        role: m.role,
        content: m.content,
        timestamp: m.created_at,
        tool_name: m.tool_name,
        tool_intent_type: m.tool_intent_type,
        tool_payload_json: m.tool_payload_json,
        tool_result_json: m.tool_result_json,
        tool_success: m.tool_success,
        tool_duration_ms: m.tool_duration_ms,
    }
}

fn api_convo_summary_from(c: agentos_kernel::convo_store::AgentConvo) -> ApiConvoSummary {
    ApiConvoSummary {
        id: c.id,
        topic: c.topic,
        participants: c.participants,
        status: c.status,
        updated_at: c.updated_at,
        kind: c.kind,
    }
}

fn api_convo_turn_from(t: agentos_kernel::convo_store::ConvoTurn) -> ApiConvoTurn {
    ApiConvoTurn {
        turn_number: t.turn_number,
        agent_name: t.agent_name,
        content: t.content,
        created_at: t.created_at,
    }
}

// ── Implementation ──────────────────────────────────────────────────────────

/// Tell every connected panel that a chat session changed.
///
/// The streaming client renders its own turn locally, but a second tab (or a
/// channel bridge writing into the same session) has no other signal — before
/// this, a message written elsewhere only appeared after a reload.
async fn emit_chat_message_added(kernel: &Kernel, session_id: &str, role: &str) {
    kernel
        .emit_event(
            agentos_types::EventType::ChatMessageAdded,
            agentos_types::EventSource::AgentMessageBus,
            agentos_types::EventSeverity::Info,
            serde_json::json!({ "session_id": session_id, "role": role }),
            0,
        )
        .await;
}

/// Expand a composer message into the turn the model sees.
///
/// Delegates to [`agentos_kernel::chat_ingest::build_user_turn`], which also
/// backs the web chat, so `@mentions` and attachments behave identically on both
/// surfaces. Skipping this on the REST path is what left `@file` inert in the
/// React panel and dropped every attachment it sent.
///
/// Typed entity mentions (`@task:`, `@agent:`) resolve only on the web composer,
/// which is the only one that offers them; here they stay literal text, exactly
/// as an unresolvable entity already does.
///
/// `owner_principal` is the caller's API key id. It must not be empty: the
/// `FileStore` owner clause is `owner = ?N OR owner IS NULL OR owner = ''`, so an
/// empty principal matches *only* unowned rows — channel media and CLI writes —
/// while `POST /api/v1/files` stamps every panel upload with the key id. Passing
/// the real principal sees both, which is exactly the set this caller may read.
async fn expand_chat_user_turn(
    kernel: &Kernel,
    text: &str,
    file_ids: Option<&str>,
    owner_principal: &str,
    agent_name: &str,
    session_id: &str,
) -> (String, Option<Vec<agentos_types::ContentPart>>) {
    let supports_images = kernel
        .agent_supports_images(agent_name)
        .await
        .unwrap_or(false);
    agentos_kernel::chat_ingest::build_user_turn(
        text,
        file_ids,
        &kernel.file_store,
        None,
        owner_principal,
        Some(session_id),
        supports_images,
        Some(&kernel.config.transcription),
    )
    .await
}

#[async_trait]
impl KernelService for Kernel {
    // ── Agents ──────────────────────────────────────────────────────────

    async fn list_agents(&self) -> Result<Vec<ApiAgentSummary>, ApiError> {
        let registry = self.agent_registry.read().await;
        let llms = self.active_llms.read().await;
        // All registered agents, offline included — the `status` field tells
        // them apart and this keeps the count consistent with `/dashboard`.
        Ok(registry
            .list_all()
            .into_iter()
            .map(|p| {
                let supports_images = llms
                    .get(&p.id)
                    .map(|c| c.supports_images())
                    .unwrap_or(false);
                agent_summary(p, supports_images)
            })
            .collect())
    }

    async fn connect_agent(&self, req: ConnectAgentRequest) -> Result<ApiAgentSummary, ApiError> {
        let provider = parse_provider(&req.provider)?;
        self.api_connect_agent(
            req.name.clone(),
            provider,
            req.model.clone(),
            req.base_url.clone(),
            req.roles.clone(),
            req.description.clone(),
            req.thinking_level.clone(),
            req.system_prompt.clone(),
        )
        .await
        .map_err(ApiError::Internal)?;

        // Read back the newly connected agent to return its summary.
        let registry = self.agent_registry.read().await;
        let profile = registry
            .get_by_name(&req.name)
            .ok_or_else(|| ApiError::Internal("Agent registered but not found".into()))?;
        let supports_images = self
            .active_llms
            .read()
            .await
            .get(&profile.id)
            .map(|c| c.supports_images())
            .unwrap_or(false);
        Ok(agent_summary(profile, supports_images))
    }

    async fn disconnect_agent(&self, agent_id: agentos_types::AgentID) -> Result<(), ApiError> {
        self.api_disconnect_agent(agent_id)
            .await
            .map_err(ApiError::Internal)
    }

    async fn remove_agent(
        &self,
        agent_id: agentos_types::AgentID,
    ) -> Result<Option<serde_json::Value>, ApiError> {
        self.api_remove_agent(agent_id)
            .await
            .map_err(ApiError::Internal)
    }

    async fn get_agent_detail(&self, name: &str) -> Result<ApiAgentDetail, ApiError> {
        let registry = self.agent_registry.read().await;
        let profile = registry
            .get_by_name(name)
            .ok_or_else(|| ApiError::NotFound(format!("Agent '{}' not found", name)))?;

        let (summary, adapter) = {
            let llms = self.active_llms.read().await;
            let adapter = llms.get(&profile.id).cloned();
            let supports_images = adapter
                .as_ref()
                .map(|c| c.supports_images())
                .unwrap_or(false);
            (agent_summary(profile, supports_images), adapter)
        };
        // Live provider probe so the panel can say "online but unreachable"
        // (e.g. ollama stopped). Bounded so a dead endpoint can't stall the
        // page, and memoised because the panel polls this endpoint: for a
        // claude-code agent every probe spawns a `claude --version` subprocess.
        let provider_healthy = match adapter {
            Some(llm) => cached_provider_health(profile.id, llm).await,
            None => None,
        };
        let effective = registry.compute_effective_permissions(&profile.id);
        let permissions: Vec<String> = effective
            .entries()
            .iter()
            .filter_map(permission_str)
            .collect();

        let cost_snapshot = self.cost_tracker.get_snapshot(&profile.id).await;

        // Fetch recent tasks assigned to this agent.
        let all_tasks = self.scheduler.list_tasks().await;
        let recent_tasks: Vec<ApiTaskSummary> = all_tasks
            .iter()
            .filter(|t| {
                // Match by agent name via the agent_registry lookup.
                let ag = registry.get_by_id(&t.agent_id);
                ag.is_some_and(|a| a.name == name)
            })
            .take(10)
            .map(|t| {
                let agent_name = registry.get_by_id(&t.agent_id).map(|a| a.name.clone());
                ApiTaskSummary {
                    id: t.id,
                    agent_name,
                    prompt_preview: t.prompt_preview.clone(),
                    status: (&t.state).into(),
                    created_at: t.created_at,
                    completed_at: None,
                    error: t.error.clone(),
                }
            })
            .collect();

        Ok(ApiAgentDetail {
            summary,
            provider_healthy,
            permissions,
            recent_tasks,
            cost_snapshot,
            description: profile.description.clone(),
            thinking_level: thinking_level_str(&profile.default_thinking_level).to_string(),
            system_prompt: profile.system_prompt.clone(),
        })
    }

    async fn update_agent_settings(&self, req: UpdateAgentSettingsRequest) -> Result<(), ApiError> {
        // Caps live here, not in one caller: the HTML form enforced them while the
        // REST path let a client store an unbounded prompt straight into agents.json.
        if req.description.as_ref().is_some_and(|d| d.len() > 2_048) {
            return Err(ApiError::BadRequest(
                "Description too long (max 2,048 chars)".into(),
            ));
        }
        if req.system_prompt.as_ref().is_some_and(|p| p.len() > 16_384) {
            return Err(ApiError::BadRequest(
                "System prompt too long (max 16,384 chars)".into(),
            ));
        }
        // Absent field = leave unchanged. An empty `system_prompt` is the one way
        // to clear the stored prompt, so it maps to `Some(None)`.
        let system_prompt = req
            .system_prompt
            .map(|s| if s.trim().is_empty() { None } else { Some(s) });
        // Same spelling as the prompt: `""` clears.
        let avatar = match req.avatar {
            Some(a) if a.is_empty() => Some(None),
            Some(a) => {
                validate_avatar(&a)?;
                Some(Some(a))
            }
            None => None,
        };
        self.api_update_agent_settings(
            req.agent_name,
            req.description,
            req.thinking_level,
            system_prompt,
            req.working_set_size,
            avatar,
        )
        .await
        // The registry's only failure here is an unknown agent name, which the
        // endpoint documents as a 404 — mapping it to 500 made react-query retry
        // a permanent failure and showed the operator "Internal error".
        .map_err(ApiError::NotFound)
    }

    async fn grant_permission(&self, req: PermissionRequest) -> Result<(), ApiError> {
        self.api_grant_permission(req.agent_name, req.permission)
            .await
            .map_err(permission_cmd_err)
    }

    async fn revoke_permission(&self, req: PermissionRequest) -> Result<(), ApiError> {
        self.api_revoke_permission(req.agent_name, req.permission)
            .await
            .map_err(permission_cmd_err)
    }

    async fn receive_webhook(
        &self,
        endpoint_id: &str,
        headers: std::collections::HashMap<String, String>,
        body: Vec<u8>,
    ) -> Result<(), ApiError> {
        use agentos_kernel::webhook_verify::{ingest_webhook, WebhookIngestError};
        // Generic bodies on purpose: this route is unauthenticated.
        ingest_webhook(
            &self.webhook_registry,
            &self.webhook_throttle,
            &self.webhook_batcher,
            endpoint_id,
            headers,
            &body,
        )
        .await
        .map_err(|e| match e {
            WebhookIngestError::NotFound => ApiError::NotFound("Webhook endpoint not found".into()),
            WebhookIngestError::RateLimited => {
                ApiError::RateLimited("Webhook endpoint rate limit".into())
            }
            WebhookIngestError::UnknownProvider => {
                ApiError::Internal("Webhook endpoint misconfigured".into())
            }
            WebhookIngestError::InvalidSignature => ApiError::Unauthorized,
        })
    }

    // ── Tasks ───────────────────────────────────────────────────────────

    async fn list_tasks(&self, filter: TaskFilter) -> Result<(Vec<ApiTaskSummary>, u64), ApiError> {
        let all_tasks = self.scheduler.list_tasks().await;
        let registry = self.agent_registry.read().await;

        let mut filtered: Vec<_> = all_tasks
            .into_iter()
            .filter(|t| {
                if let Some(status) = filter.status {
                    if ApiTaskStatus::from(&t.state) != status {
                        return false;
                    }
                }
                if let Some(ref agent_name) = filter.agent_name {
                    let matches = registry
                        .get_by_id(&t.agent_id)
                        .is_some_and(|a| a.name == *agent_name);
                    if !matches {
                        return false;
                    }
                }
                true
            })
            .collect();

        let total = filtered.len() as u64;
        let offset = filter.offset.unwrap_or(0) as usize;
        let limit = filter.limit.unwrap_or(50) as usize;

        filtered.sort_by_key(|t| std::cmp::Reverse(t.created_at));

        let page: Vec<ApiTaskSummary> = filtered
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|t| {
                let agent_name = registry.get_by_id(&t.agent_id).map(|a| a.name.clone());
                ApiTaskSummary {
                    id: t.id,
                    agent_name,
                    prompt_preview: t.prompt_preview.clone(),
                    status: (&t.state).into(),
                    created_at: t.created_at,
                    completed_at: None,
                    error: t.error.clone(),
                }
            })
            .collect();

        Ok((page, total))
    }

    async fn get_task(&self, id: TaskID) -> Result<ApiTaskDetail, ApiError> {
        let task = self
            .scheduler
            .get_task(&id)
            .await
            .ok_or_else(|| ApiError::NotFound(format!("Task {} not found", id)))?;

        let registry = self.agent_registry.read().await;
        let agent_name = registry.get_by_id(&task.agent_id).map(|a| a.name.clone());
        drop(registry);
        let (completed_at, result) = self.scheduler.outcome(&id).await.unzip();

        Ok(ApiTaskDetail {
            id: task.id,
            agent_name,
            prompt: task.original_prompt.clone(),
            status: (&task.state).into(),
            created_at: task.created_at,
            completed_at,
            result: result.flatten(),
            trigger_event_type: task
                .trigger_source
                .as_ref()
                .map(|t| t.event_type.to_string()),
            error: self.scheduler.failure_reason(&task.id).await,
        })
    }

    async fn run_task(&self, req: RunTaskRequest) -> Result<TaskID, ApiError> {
        self.api_submit_task(req.agent_name, req.prompt, req.autonomous)
            .await
            .map_err(ApiError::Conflict)
    }

    async fn cancel_task(&self, id: TaskID) -> Result<(), ApiError> {
        // Through the kernel's cancel path, not a bare state flip: that left the
        // task's approval card live (approving it then ran the tool for a
        // cancelled task), its parent waiting out a full timeout, and its
        // children running.
        if self.scheduler.get_task(&id).await.is_none() {
            return Err(ApiError::from(agentos_types::AgentOSError::TaskNotFound(
                id,
            )));
        }
        match self.cmd_cancel_task(id).await {
            agentos_bus::KernelResponse::Error { message } => Err(ApiError::Conflict(message)),
            _ => Ok(()),
        }
    }

    async fn get_task_trace(
        &self,
        id: TaskID,
    ) -> Result<agentos_types::task_trace::TaskTrace, ApiError> {
        let trace = self
            .trace_collector
            .get_trace(&id)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?
            .ok_or_else(|| ApiError::NotFound(format!("Trace for task {} not found", id)))?;
        Ok(trace)
    }

    // ── Tools ───────────────────────────────────────────────────────────

    async fn list_tools(&self) -> Result<Vec<ApiToolSummary>, ApiError> {
        let registry = self.tool_registry.read().await;
        Ok(registry.list_all().into_iter().map(tool_summary).collect())
    }

    async fn install_tool(&self, req: InstallToolRequest) -> Result<ToolID, ApiError> {
        self.api_install_tool(req.manifest_path.clone())
            .await
            .map_err(ApiError::Internal)
    }

    async fn remove_tool(&self, name: &str) -> Result<(), ApiError> {
        self.api_remove_tool(name.to_string())
            .await
            .map_err(ApiError::Internal)
    }

    // ── Secrets ─────────────────────────────────────────────────────────

    async fn list_secrets(&self) -> Result<Vec<SecretMetadata>, ApiError> {
        self.vault
            .list()
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))
    }

    async fn set_secret(&self, req: SetSecretRequest) -> Result<(), ApiError> {
        let scope = parse_scope(&req.scope)?;
        // Only `agent:<name>` / `tool:<name>` need kernel-side resolution: it turns the
        // name into a real AgentID/ToolID against its registries and rejects unknown
        // names, instead of silently widening to Global. `global`/`kernel` are already
        // final — and the kernel's resolver is case-sensitive, so handing it e.g.
        // "Global" would fail a request that parsed fine here.
        let needs_resolution = req.scope.starts_with("agent:") || req.scope.starts_with("tool:");
        self.api_set_secret(
            req.name,
            req.value,
            scope,
            needs_resolution.then_some(req.scope),
        )
        .await
        .map_err(|e| {
            if e.contains("could not be resolved") {
                ApiError::BadRequest(e)
            } else {
                ApiError::Internal(e)
            }
        })
    }

    async fn revoke_secret(&self, name: &str) -> Result<(), ApiError> {
        self.api_revoke_secret(name.to_string())
            .await
            .map_err(ApiError::Internal)
    }

    // ── Chat ────────────────────────────────────────────────────────────

    async fn agent_supports_images(&self, agent_name: &str) -> Result<bool, ApiError> {
        let registry = self.agent_registry.read().await;
        let profile = registry
            .get_by_name(agent_name)
            .ok_or_else(|| ApiError::NotFound(format!("Agent '{}' not found", agent_name)))?;
        let llms = self.active_llms.read().await;
        Ok(llms
            .get(&profile.id)
            .map(|c| c.supports_images())
            .unwrap_or(false))
    }

    async fn chat_send(&self, req: ChatRequest) -> Result<ChatResponse, ApiError> {
        // S1: tools execute INSIDE `chat_infer_with_tools`, where each call is
        // gated by per-turn capability-token validation (HMAC/expiry/scope) in
        // the kernel chat loop. The API never dispatches tools itself, so the
        // REST/OpenAI-compat path inherits that enforcement — no separate gate.
        let history: Vec<(String, String)> = req.history;
        let user_parts = (!req.parts.is_empty()).then_some(req.parts.clone());
        let result = self
            .chat_infer_with_tools(
                &req.agent_name,
                &history,
                &req.message,
                user_parts,
                Some(&req.session_id),
            )
            .await
            .map_err(ApiError::Internal)?;

        let tool_calls: Vec<serde_json::Value> = result
            .tool_calls
            .into_iter()
            .map(|tc| {
                serde_json::json!({
                    "tool_name": tc.tool_name,
                    "intent_type": tc.intent_type,
                    "payload": tc.payload,
                    "result": tc.result,
                })
            })
            .collect();

        Ok(ChatResponse {
            message: result.answer,
            tool_calls,
        })
    }

    async fn chat_stream(
        &self,
        req: ChatRequest,
        tx: mpsc::Sender<ChatStreamEvent>,
    ) -> Result<(), ApiError> {
        // Run the same chat_infer_with_tools path but emit events along the way.
        // For now we perform full inference and emit Thinking → Done events.
        // This unblocks SSE clients while a full token-level streaming implementation
        // is wired in a future iteration.
        // S1: as in chat_send, tools execute inside the kernel chat loop and are
        // capability-validated there; the API adds no independent tool dispatch.
        let _ = tx
            .send(ChatStreamEvent::Thinking {
                iteration: 1,
                text: None,
            })
            .await;

        let history: Vec<(String, String)> = req.history;
        let user_parts = (!req.parts.is_empty()).then_some(req.parts.clone());
        let result = self
            .chat_infer_with_tools(
                &req.agent_name,
                &history,
                &req.message,
                user_parts,
                Some(&req.session_id),
            )
            .await
            .map_err(ApiError::Internal)?;

        let tool_calls: Vec<agentos_kernel::kernel::ChatToolCallRecord> = result.tool_calls;

        // Emit tool events
        for tc in &tool_calls {
            let _ = tx
                .send(ChatStreamEvent::ToolResult {
                    tool_name: tc.tool_name.clone(),
                    result_preview: {
                        let s = tc.result.to_string();
                        s.chars().take(200).collect()
                    },
                    duration_ms: 0,
                    success: true,
                })
                .await;
        }

        let _ = tx
            .send(ChatStreamEvent::Done {
                answer: result.answer,
                tool_calls,
                iterations: result.iterations,
                tokens_used: result.tokens_used,
                cost_usd: result.cost_usd,
            })
            .await;

        Ok(())
    }

    // ── Pipelines ───────────────────────────────────────────────────────

    async fn list_pipelines(&self) -> Result<Vec<ApiPipelineSummary>, ApiError> {
        let store = self.pipeline_engine.store_arc();
        let summaries = tokio::task::spawn_blocking(move || store.list_pipelines())
            .await
            .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
            .map_err(|e| ApiError::Internal(e.to_string()))?;

        Ok(summaries
            .into_iter()
            .map(|s| ApiPipelineSummary {
                name: s.name,
                description: s.description,
                step_count: s.step_count,
            })
            .collect())
    }

    async fn save_pipeline(&self, req: SavePipelineRequest) -> Result<(), ApiError> {
        let yaml = serde_json::to_string_pretty(&req.definition)
            .map_err(|e| ApiError::BadRequest(format!("Invalid pipeline definition: {e}")))?;
        // The definition owns its version; the store used to stamp every pipeline
        // "1.0.0", silently downgrading anything that declared otherwise.
        let version = req
            .definition
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("1.0.0")
            .to_string();
        // Reject a document the engine could never run. Without this the store
        // happily holds a blob that only fails at run time — and `list_pipelines`
        // swallows the parse error as `step_count: 0`, so it looks installed.
        agentos_pipeline::PipelineDefinition::from_yaml(&yaml)
            .map_err(|e| ApiError::BadRequest(format!("Invalid pipeline definition: {e}")))?;
        let store = self.pipeline_engine.store_arc();
        let name = req.name.clone();
        let overwrite = req.overwrite;
        // `create_pipeline` is a plain INSERT, so the name primary key rejects a
        // collision with no window between a check and a write. `install_pipeline`
        // (INSERT OR REPLACE) is reserved for an explicit overwrite.
        tokio::task::spawn_blocking(move || {
            if overwrite {
                return store
                    .install_pipeline(&name, &version, &yaml)
                    .map_err(|e| ApiError::Internal(e.to_string()));
            }
            match store.create_pipeline(&name, &version, &yaml) {
                Ok(true) => Ok(()),
                Ok(false) => Err(ApiError::Conflict(format!(
                    "Pipeline '{name}' already exists — save with overwrite to replace it"
                ))),
                Err(e) => Err(ApiError::Internal(e.to_string())),
            }
        })
        .await
        .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
    }

    async fn run_pipeline(&self, req: RunPipelineRequest) -> Result<serde_json::Value, ApiError> {
        // Use fully qualified syntax to call the inherent Kernel::run_pipeline,
        // not the KernelService trait method (which would recurse).
        Kernel::run_pipeline(self, req.name, req.input, req.detach, req.agent_name)
            .await
            .map_err(ApiError::Internal)
    }

    async fn delete_pipeline(&self, name: &str) -> Result<(), ApiError> {
        let store = self.pipeline_engine.store_arc();
        let name_owned = name.to_string();
        tokio::task::spawn_blocking(move || store.remove_pipeline(&name_owned))
            .await
            .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
            .map_err(|e| ApiError::Internal(e.to_string()))
    }

    // ── Audit ───────────────────────────────────────────────────────────

    async fn query_audit(&self, filter: AuditFilter) -> Result<Vec<AuditEntrySummary>, ApiError> {
        let audit = self.audit.clone();
        let limit = filter.limit.unwrap_or(50).min(1000);

        // Parse optional predicate inputs up-front so we can fail fast on bad input.
        let agent_id = match filter.agent_id.as_deref() {
            Some(s) => Some(
                s.parse::<agentos_types::AgentID>()
                    .map_err(|_| ApiError::BadRequest(format!("Invalid agent_id: {s}")))?,
            ),
            None => None,
        };
        let task_id = match filter.task_id.as_deref() {
            Some(s) => Some(
                s.parse::<agentos_types::TaskID>()
                    .map_err(|_| ApiError::BadRequest(format!("Invalid task_id: {s}")))?,
            ),
            None => None,
        };
        let event_type: Option<AuditEventType> = match filter.event_type.as_deref() {
            Some(s) => Some(
                serde_json::from_value(serde_json::Value::String(s.to_string()))
                    .map_err(|_| ApiError::BadRequest(format!("Invalid event_type: {s}")))?,
            ),
            None => None,
        };
        let from = filter.from;
        let to = filter.to;

        // Choose the most selective backing query, then filter remaining
        // predicates in memory over an over-fetched window.
        let fetch_limit = limit.saturating_mul(5).min(5000).max(limit);
        let evt = event_type;
        let entries = tokio::task::spawn_blocking(move || {
            if let Some(et) = evt {
                audit.query_by_type(et, fetch_limit)
            } else if let (Some(f), Some(t)) = (from, to) {
                audit.query_by_time_range(f, t, fetch_limit)
            } else if let Some(aid) = agent_id {
                audit.query_recent_for_agent(&aid, fetch_limit)
            } else {
                audit.query_recent(fetch_limit)
            }
        })
        .await
        .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
        .map_err(|e| ApiError::Internal(e.to_string()))?;

        let severity_filter = filter.severity.clone();
        let filtered: Vec<AuditEntrySummary> = entries
            .into_iter()
            .filter(|e| agent_id.is_none() || e.agent_id == agent_id)
            .filter(|e| task_id.is_none() || e.task_id == task_id)
            .filter(|e| {
                from.is_none()
                    || to.is_none()
                    || (e.timestamp >= from.unwrap() && e.timestamp <= to.unwrap())
            })
            .filter(|e| match &severity_filter {
                Some(s) => serde_json::to_string(&e.severity)
                    .unwrap_or_default()
                    .trim_matches('"')
                    .eq_ignore_ascii_case(s),
                None => true,
            })
            .take(limit as usize)
            .map(|e| AuditEntrySummary {
                timestamp: e.timestamp,
                event_type: serde_json::to_string(&e.event_type)
                    .unwrap_or_default()
                    .trim_matches('"')
                    .to_string(),
                agent_id: e.agent_id.map(|id| id.to_string()),
                details: e.details.to_string(),
            })
            .collect();

        Ok(filtered)
    }

    async fn get_audit_detail(&self, trace_id: &str) -> Result<AuditEntryDetail, ApiError> {
        let tid = trace_id
            .parse::<agentos_types::TraceID>()
            .map_err(|_| ApiError::BadRequest(format!("Invalid trace ID: {trace_id}")))?;

        let audit = self.audit.clone();
        let entries = tokio::task::spawn_blocking(move || audit.query_by_trace(&tid))
            .await
            .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
            .map_err(|e| ApiError::Internal(e.to_string()))?;

        let entry = entries.into_iter().next().ok_or_else(|| {
            ApiError::NotFound(format!("Audit entry for trace {} not found", trace_id))
        })?;

        Ok(AuditEntryDetail {
            timestamp: entry.timestamp,
            event_type: serde_json::to_string(&entry.event_type)
                .unwrap_or_default()
                .trim_matches('"')
                .to_string(),
            agent_id: entry.agent_id.map(|id| id.to_string()),
            task_id: entry.task_id.map(|id| id.to_string()),
            trace_id: Some(entry.trace_id.to_string()),
            details: entry.details.to_string(),
            metadata: entry.details,
        })
    }

    // ── Costs ───────────────────────────────────────────────────────────

    async fn get_cost_summary(&self) -> Result<Vec<CostSummaryEntry>, ApiError> {
        let snapshots = self.cost_tracker.get_all_snapshots().await;
        Ok(snapshots
            .into_iter()
            .map(cost_entry_from_snapshot)
            .collect())
    }

    async fn get_agent_costs(&self, agent_name: &str) -> Result<CostSummaryEntry, ApiError> {
        let registry = self.agent_registry.read().await;
        let profile = registry
            .get_by_name(agent_name)
            .ok_or_else(|| ApiError::NotFound(format!("Agent '{}' not found", agent_name)))?;
        let agent_id = profile.id;
        drop(registry);

        let snapshot = self
            .cost_tracker
            .get_snapshot(&agent_id)
            .await
            .ok_or_else(|| {
                ApiError::NotFound(format!("No cost data for agent '{}'", agent_name))
            })?;

        Ok(cost_entry_from_snapshot(snapshot))
    }

    // ── Notifications ───────────────────────────────────────────────────

    async fn list_notifications(
        &self,
        filter: NotificationFilter,
    ) -> Result<Vec<NotificationSummary>, ApiError> {
        let inbox = self.notification_router.inbox();
        let unread_only = filter.unread_only.unwrap_or(false);
        let limit = filter.limit.unwrap_or(50) as usize;

        let messages = inbox
            .list(unread_only, limit)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;

        Ok(messages
            .into_iter()
            .map(|m| NotificationSummary {
                id: m.id,
                subject: m.subject.clone(),
                priority: m.priority.to_string(),
                read: m.read,
                timestamp: m.created_at.to_rfc3339(),
                from: match &m.from {
                    agentos_types::NotificationSource::Agent(id) => format!("Agent {}", id),
                    agentos_types::NotificationSource::Kernel => "Kernel".to_string(),
                    agentos_types::NotificationSource::System => "System".to_string(),
                },
                body: m.body.clone(),
                task_id: m.task_id.map(|t| t.to_string()),
                // A question the user can still usefully answer: unanswered AND
                // not past its deadline. Once `expires_at` passes, the waiter
                // has already been released with the question's `auto_action`
                // (notification_router::sweep_expired_waiters) without writing a
                // response back, so `response.is_none()` alone would keep
                // advertising a reply that now goes nowhere.
                needs_response: matches!(m.kind, agentos_types::UserMessageKind::Question { .. })
                    && m.response.is_none()
                    && m.expires_at.is_none_or(|e| e > chrono::Utc::now()),
                // `ChannelBroadcastSink` stamps `thread_id = "escalation:<id>"`
                // on every approval prompt it delivers.
                escalation_id: m
                    .thread_id
                    .as_deref()
                    .and_then(|t| t.strip_prefix("escalation:"))
                    .and_then(|id| id.parse().ok()),
            })
            .collect())
    }

    async fn get_notification(
        &self,
        id: NotificationID,
    ) -> Result<agentos_types::UserMessage, ApiError> {
        let inbox = self.notification_router.inbox();
        inbox
            .get(&id)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?
            .ok_or_else(|| ApiError::NotFound(format!("Notification {} not found", id)))
    }

    async fn respond_to_notification(
        &self,
        id: NotificationID,
        text: String,
    ) -> Result<(), ApiError> {
        let response = UserResponse {
            text,
            responded_at: chrono::Utc::now(),
            channel: DeliveryChannel::web(),
        };
        self.notification_router
            .route_response(id, response)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))
    }

    async fn mark_notification_read(&self, id: NotificationID) -> Result<bool, ApiError> {
        let updated = self
            .notification_router
            .inbox()
            .mark_read(&id)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        if updated {
            self.record_audit(
                AuditEventType::NotificationRead,
                serde_json::json!({ "notification_id": id.to_string() }),
            )
            .await;
        }
        Ok(updated)
    }

    async fn dismiss_notification(&self, id: NotificationID) -> Result<bool, ApiError> {
        let inbox = self.notification_router.inbox();
        inbox
            .delete(&id)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))
    }

    async fn clear_read_notifications(&self) -> Result<usize, ApiError> {
        let inbox = self.notification_router.inbox();
        inbox
            .clear_read()
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))
    }

    async fn clear_all_notifications(&self) -> Result<usize, ApiError> {
        self.notification_router
            .inbox()
            .clear_all()
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))
    }

    async fn mark_all_notifications_read(&self) -> Result<usize, ApiError> {
        self.notification_router
            .inbox()
            .mark_all_read()
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))
    }

    async fn get_unread_count(&self) -> Result<u64, ApiError> {
        let inbox = self.notification_router.inbox();
        Ok(inbox.count_unread().await as u64)
    }

    async fn get_notification_routes(&self) -> Result<ApiNotificationRoutes, ApiError> {
        use agentos_types::NotificationEvent;

        let events = NotificationEvent::ALL
            .iter()
            .map(|e| ApiNotificationEvent {
                key: e.as_str().to_string(),
                label: e.label().to_string(),
                description: e.description().to_string(),
            })
            .collect();

        // Two outbound stacks own the channels between them: the notification
        // router (Telegram/Ntfy/Email + the builtin desktop/cli/web/webhook/
        // slack adapters) and the ChannelManager (Discord/Slack/WhatsApp/
        // Teams/Matrix/…). A column axis built from the router alone would
        // leave every manager-stack channel unmutable from the panel — while
        // the escalation sink still DMs it — so the registry's active channels
        // are unioned in.
        let registered = self
            .channel_registry
            .list_active()
            .await
            .map_err(|e| ApiError::Internal(format!("Channel registry error: {e}")))?;
        let names: std::collections::HashMap<String, String> = registered
            .iter()
            .map(|ch| (ch.id.to_string(), ch.display_name.clone()))
            .collect();

        let mut channels: Vec<ApiRouteChannel> = self
            .notification_router
            .adapter_targets()
            .await
            .into_iter()
            .map(|(key, kind, available)| ApiRouteChannel {
                label: names.get(&key).cloned().unwrap_or_else(|| kind.clone()),
                key,
                kind,
                available,
            })
            .collect();

        let health = self.channel_manager.health().await;
        for ch in &registered {
            let key = ch.id.to_string();
            if channels.iter().any(|c| c.key == key) {
                continue;
            }
            channels.push(ApiRouteChannel {
                // A manager-stack channel is reachable when the manager holds a
                // live adapter for it; an unknown id means it was never built.
                available: health.contains_key(&key),
                key,
                kind: ch.kind.to_string(),
                label: ch.display_name.clone(),
            });
        }
        channels.sort_by(|a, b| a.label.cmp(&b.label));

        let rules = self
            .notification_routes
            .rules()
            .into_iter()
            .map(|(event, channel, mode)| ApiRouteRule {
                event: event.as_str().to_string(),
                channel,
                mode: mode.as_str().to_string(),
            })
            .collect();

        Ok(ApiNotificationRoutes {
            events,
            channels,
            rules,
            panel_connected: self.notification_routes.panel_connected(),
        })
    }

    async fn set_notification_routes(
        &self,
        rules: Vec<ApiRouteRule>,
    ) -> Result<ApiNotificationRoutes, ApiError> {
        use agentos_kernel::RouteMode;
        use agentos_types::NotificationEvent;

        // Bounds before work: this write lands in one transaction on the state
        // DB mutex, which the scheduler, escalation store and cost tracker all
        // share. An unbounded batch would park them for its duration.
        const MAX_RULES: usize = 512;
        const MAX_CHANNEL_LEN: usize = 128;
        if rules.len() > MAX_RULES {
            return Err(ApiError::BadRequest(format!(
                "Too many rules: {} (max {MAX_RULES} per request)",
                rules.len()
            )));
        }

        let mut parsed = Vec::with_capacity(rules.len());
        for rule in rules {
            let event = NotificationEvent::parse(&rule.event).ok_or_else(|| {
                ApiError::BadRequest(format!(
                    "Unknown notification event '{}' — expected one of: {}",
                    rule.event,
                    NotificationEvent::ALL
                        .iter()
                        .map(|e| e.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })?;
            let mode = RouteMode::parse(&rule.mode).ok_or_else(|| {
                ApiError::BadRequest(format!(
                    "Unknown route mode '{}' — expected always, never, or when_away",
                    rule.mode
                ))
            })?;
            let channel = rule.channel.trim();
            if channel.is_empty() {
                return Err(ApiError::BadRequest(
                    "Route rule 'channel' must not be empty".to_string(),
                ));
            }
            if channel.len() > MAX_CHANNEL_LEN {
                return Err(ApiError::BadRequest(format!(
                    "Route rule 'channel' is too long ({} chars, max {MAX_CHANNEL_LEN})",
                    channel.len()
                )));
            }
            let rule = ApiRouteRule {
                channel: channel.to_string(),
                ..rule
            };
            parsed.push((event, rule.channel, mode));
        }

        let count = parsed.len();
        self.notification_routes
            .set_many(parsed)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to save notification routes: {e}")))?;

        let _ = self.audit.append(AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: agentos_types::TraceID::new(),
            event_type: AuditEventType::KernelConfigChanged,
            agent_id: None,
            task_id: None,
            tool_id: None,
            details: serde_json::json!({ "source": "notification_routes", "rules": count }),
            severity: AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        });

        self.get_notification_routes().await
    }

    // ── Dashboard ───────────────────────────────────────────────────────

    async fn get_dashboard_summary(&self) -> Result<DashboardSummary, ApiError> {
        let online_agents: Vec<ApiAgentSummary> = self
            .list_agents()
            .await?
            .into_iter()
            .filter(|a| a.status != "offline")
            .collect();
        let agent_count = {
            let registry = self.agent_registry.read().await;
            registry.list_all().len()
        };

        let all_tasks = self.scheduler.list_tasks().await;
        let running = all_tasks
            .iter()
            .filter(|t| t.state == TaskState::Running)
            .count();
        let completed = all_tasks
            .iter()
            .filter(|t| t.state == TaskState::Complete)
            .count();
        let failed = all_tasks
            .iter()
            .filter(|t| t.state == TaskState::Failed)
            .count();
        let total = all_tasks.len();

        let tool_count = {
            let registry = self.tool_registry.read().await;
            registry.list_all().len()
        };

        let uptime = chrono::Utc::now()
            .signed_duration_since(self.started_at)
            .to_std()
            .unwrap_or_default();

        let audit_filter = AuditFilter {
            limit: Some(10),
            ..Default::default()
        };
        let recent_audit = self.query_audit(audit_filter).await.unwrap_or_default();

        let background_tasks = self.background_pool.list_running().await;

        Ok(DashboardSummary {
            agent_count,
            online_agents,
            task_counts: TaskCounts {
                running,
                completed,
                failed,
                total,
            },
            tool_count,
            uptime_secs: uptime.as_secs(),
            recent_audit,
            background_task_count: background_tasks.len(),
        })
    }

    // ── System ──────────────────────────────────────────────────────────

    async fn get_status(&self) -> Result<SystemStatus, ApiError> {
        let agent_count = {
            let registry = self.agent_registry.read().await;
            registry.list_online().len()
        };
        let task_count = self.scheduler.list_tasks().await.len();
        let tool_count = {
            let registry = self.tool_registry.read().await;
            registry.list_all().len()
        };
        let uptime = chrono::Utc::now()
            .signed_duration_since(self.started_at)
            .to_std()
            .unwrap_or_default();

        Ok(SystemStatus {
            uptime_secs: uptime.as_secs(),
            agent_count,
            task_count,
            tool_count,
            version: env!("CARGO_PKG_VERSION").to_string(),
        })
    }

    async fn get_uptime(&self) -> std::time::Duration {
        chrono::Utc::now()
            .signed_duration_since(self.started_at)
            .to_std()
            .unwrap_or_default()
    }

    async fn verify_webhook_secret(
        &self,
        channel_id: &str,
        secret: &str,
    ) -> Result<bool, ApiError> {
        use subtle::ConstantTimeEq;
        let cid: agentos_types::ChannelInstanceID = channel_id
            .parse()
            .map_err(|_| ApiError::BadRequest(format!("Invalid channel ID: {channel_id}")))?;
        let secrets = self.webhook_secrets.read().await;
        // W13: constant-time compare — the webhook secret (e.g. Telegram's
        // X-Telegram-Bot-Api-Secret-Token) is a shared secret; a plain `==`
        // is a timing oracle. Length may leak; the bytes are compared in
        // constant time only when lengths match.
        let valid = match secrets.get(&cid) {
            Some(expected) => {
                let a = expected.as_bytes();
                let b = secret.as_bytes();
                a.len() == b.len() && bool::from(a.ct_eq(b))
            }
            None => false,
        };
        Ok(valid)
    }

    async fn channel_pinned_external_id(
        &self,
        channel_id: &str,
    ) -> Result<Option<String>, ApiError> {
        let cid: agentos_types::ChannelInstanceID = channel_id
            .parse()
            .map_err(|_| ApiError::BadRequest(format!("Invalid channel ID: {channel_id}")))?;
        let ch = self
            .channel_registry
            .get_by_id(&cid)
            .await
            .map_err(|e| ApiError::Internal(format!("Channel registry error: {e}")))?;
        Ok(ch.map(|c| c.external_id))
    }

    async fn forward_webhook_message(
        &self,
        message: agentos_kernel::notification_router::InboundMessage,
    ) -> Result<(), ApiError> {
        self.inbound_tx
            .send(message)
            .await
            .map_err(|_| ApiError::Internal("Inbound message channel closed".into()))
    }

    async fn verify_whatsapp_signature(
        &self,
        channel_id: &str,
        body: &[u8],
        signature: &str,
    ) -> Result<bool, ApiError> {
        Ok(Kernel::whatsapp_verify_signature(self, channel_id, body, signature).await)
    }

    async fn whatsapp_verify_token(&self, channel_id: &str) -> Result<Option<String>, ApiError> {
        Ok(Kernel::whatsapp_verify_token(self, channel_id).await)
    }

    async fn telegram_ack_callback(&self, channel_id: &str, callback_query_id: &str) {
        Kernel::telegram_ack_callback(self, channel_id, callback_query_id).await;
    }

    // ── Control-plane auth ───────────────────────────────────────────────────

    async fn verify_operator_credential(&self, credential: &str) -> CredentialCheck {
        use subtle::ConstantTimeEq;
        match self.config.api.operator_token.as_deref() {
            None | Some("") => CredentialCheck::NotConfigured,
            Some(tok) => {
                let a = credential.as_bytes();
                let b = tok.as_bytes();
                // Length is allowed to leak; the token bytes are compared in
                // constant time only when lengths match.
                let valid = a.len() == b.len() && bool::from(a.ct_eq(b));
                if valid {
                    CredentialCheck::Valid
                } else {
                    CredentialCheck::Invalid
                }
            }
        }
    }

    async fn record_audit(&self, event_type: AuditEventType, details: serde_json::Value) {
        let severity = match event_type {
            AuditEventType::ApiLoginFailed => AuditSeverity::Warn,
            AuditEventType::ApiLoginSucceeded
            | AuditEventType::ApiKeyIssued
            | AuditEventType::ApiKeyRevoked => AuditSeverity::Security,
            _ => AuditSeverity::Info,
        };
        let entry = AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: agentos_types::TraceID::new(),
            event_type,
            agent_id: None,
            task_id: None,
            tool_id: None,
            details,
            severity,
            reversible: false,
            rollback_ref: None,
        };
        let audit = self.audit.clone();
        if let Err(e) = tokio::task::spawn_blocking(move || audit.append(entry)).await {
            tracing::error!("record_audit join error: {e}");
        }
    }

    // ── Escalations (HITL) ───────────────────────────────────────────────

    async fn list_escalations(&self, pending_only: bool) -> Result<Vec<ApiEscalation>, ApiError> {
        let escalations = if pending_only {
            self.escalation_manager.list_pending().await
        } else {
            self.escalation_manager.list_all().await
        };
        Ok(escalations.into_iter().map(escalation_to_api).collect())
    }

    async fn get_escalation(&self, id: u64) -> Result<ApiEscalation, ApiError> {
        let esc = self
            .escalation_manager
            .get(id)
            .await
            .ok_or_else(|| ApiError::NotFound(format!("Escalation {} not found", id)))?;
        Ok(escalation_to_api(esc))
    }

    async fn resolve_escalation(
        &self,
        id: u64,
        decision: String,
        note: Option<String>,
        remember: bool,
        actor: String,
    ) -> Result<ResolveEscalationResponse, ApiError> {
        // 404 if it does not exist; 409 if already resolved/expired.
        let existing = self
            .escalation_manager
            .get(id)
            .await
            .ok_or_else(|| ApiError::NotFound(format!("Escalation {} not found", id)))?;
        if existing.resolved {
            return Err(ApiError::Conflict(format!(
                "Escalation {} already resolved or expired",
                id
            )));
        }

        // Fold an optional operator note into the recorded decision string.
        let resolution = match &note {
            Some(n) if !n.is_empty() => format!("{decision} ({n})"),
            _ => decision.clone(),
        };

        let resp = self
            .cmd_resolve_escalation(id, resolution, remember, &actor)
            .await;
        match resp {
            agentos_bus::KernelResponse::Success { data } => {
                let data = data.unwrap_or_else(|| serde_json::json!({}));
                let task_resumed = data
                    .get("task_resumed")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let task_id = data
                    .get("task_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let policy_id = data.get("policy_id").and_then(|v| v.as_i64());
                let remember_note = remember.then(|| {
                    data.get("remember_note")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string()
                });
                Ok(ResolveEscalationResponse {
                    status: "resolved".to_string(),
                    escalation_id: id,
                    task_id,
                    task_resumed,
                    policy_id,
                    remember_note,
                })
            }
            agentos_bus::KernelResponse::Error { message } => Err(ApiError::Conflict(message)),
            _ => Err(ApiError::Internal(
                "Unexpected kernel response resolving escalation".into(),
            )),
        }
    }

    // ── Approval policies (standing grants) ──────────────────────────────

    async fn list_approval_policies(&self) -> Result<Vec<ApiApprovalPolicy>, ApiError> {
        let matcher = self.approval_policy_matcher.as_ref().ok_or_else(|| {
            ApiError::ServiceUnavailable("Approval policy store is not configured".into())
        })?;
        let entries = matcher
            .list_all()
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        Ok(entries.into_iter().map(approval_policy_to_api).collect())
    }

    async fn add_approval_policy(
        &self,
        req: AddApprovalPolicyRequest,
    ) -> Result<ApiApprovalPolicy, ApiError> {
        let matcher = self.approval_policy_matcher.as_ref().ok_or_else(|| {
            ApiError::ServiceUnavailable("Approval policy store is not configured".into())
        })?;
        // Validate at the trust boundary: an empty tool or a past expiry are
        // malformed requests (400), not privilege (403) or a dead-row 409.
        if req.tool_name.trim().is_empty() {
            return Err(ApiError::BadRequest("tool_name must not be empty".into()));
        }
        if let Some(exp) = req.expires_at {
            if exp <= chrono::Utc::now() {
                return Err(ApiError::BadRequest(
                    "expires_at must be in the future".into(),
                ));
            }
        }
        // Parse the optional agent scope; a malformed UUID is a client error.
        let agent_id = match req.agent_id.as_deref() {
            Some(s) if !s.is_empty() => Some(
                s.parse::<agentos_types::AgentID>()
                    .map_err(|_| ApiError::BadRequest(format!("Invalid agent_id: {s}")))?,
            ),
            _ => None,
        };
        let entry = matcher
            .add(
                &req.tool_name,
                req.action.as_deref(),
                req.path_glob.as_deref(),
                agent_id,
                "operator-api",
                "api",
                req.expires_at,
            )
            .map_err(|e| match &e {
                // Duplicate active scope → 409, matching the escalation-resolve style.
                agentos_types::AgentOSError::PermissionDenied { resource, .. }
                    if resource
                        == agentos_kernel::approval_policy_store::POLICY_DUPLICATE_RESOURCE =>
                {
                    ApiError::Conflict(e.to_string())
                }
                _ => ApiError::from(e),
            })?;
        Ok(approval_policy_to_api(entry))
    }

    async fn revoke_approval_policy(&self, id: i64) -> Result<(), ApiError> {
        let matcher = self.approval_policy_matcher.as_ref().ok_or_else(|| {
            ApiError::ServiceUnavailable("Approval policy store is not configured".into())
        })?;
        let revoked = matcher
            .revoke(id)
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        if revoked {
            Ok(())
        } else {
            Err(ApiError::NotFound(format!(
                "Approval policy {id} not found"
            )))
        }
    }

    // ── Workspace grants (folder access) ─────────────────────────────────────

    async fn list_workspace_grants(
        &self,
        agent_name: Option<String>,
    ) -> Result<Vec<ApiWorkspaceGrant>, ApiError> {
        let grants = self
            .api_list_workspace_grants(agent_name)
            .await
            .map_err(workspace_cmd_err)?;
        Ok(grants.into_iter().map(workspace_grant_to_api).collect())
    }

    async fn grant_workspace(
        &self,
        req: GrantWorkspaceRequest,
        actor: &str,
    ) -> Result<ApiWorkspaceGrant, ApiError> {
        // Validate at the trust boundary: the store rejects relative and system
        // paths too, but a 400 here is clearer than a PermissionDenied round trip.
        let path = std::path::PathBuf::from(req.path.trim());
        if !path.is_absolute() {
            return Err(ApiError::BadRequest(
                "path must be absolute (`~` is not expanded server-side)".into(),
            ));
        }
        // An empty string is a form field that was left blank, not a caller
        // asking for the empty mode (which the parser rejects) — treat it the
        // same as an omitted field.
        let mode = req
            .mode
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| "rw".to_string());
        let grant = self
            .api_grant_workspace(path, req.agent_name, mode, actor)
            .await
            .map_err(workspace_cmd_err)?;
        Ok(workspace_grant_to_api(grant))
    }

    async fn revoke_workspace(
        &self,
        path: String,
        agent_name: Option<String>,
        actor: &str,
    ) -> Result<u64, ApiError> {
        self.api_revoke_workspace(std::path::PathBuf::from(path.trim()), agent_name, actor)
            .await
            .map_err(workspace_cmd_err)
    }

    async fn browse_agent_memory(
        &self,
        agent_id: String,
        tier: String,
        q: Option<String>,
        limit: Option<usize>,
    ) -> Result<Vec<ApiMemoryItem>, ApiError> {
        let aid = resolve_agent_id(&self.agent_registry, &agent_id).await?;
        let limit = limit.unwrap_or(50).min(200);
        let query = q.unwrap_or_default();
        let has_q = !query.trim().is_empty();
        // Note: episodic is strictly per-agent, but semantic/procedural browse
        // also include global (agent_id IS NULL) entries — mirroring the agent's
        // effective retrieval (own + shared), so the browse matches what the
        // agent can actually recall.
        let items: Vec<ApiMemoryItem> = match tier.as_str() {
            "episodic" => {
                let entries = if has_q {
                    self.episodic_memory
                        .recall_global(&query, Some(&aid), None, limit)
                        .await
                } else {
                    self.episodic_memory.recent(Some(&aid), limit).await
                }
                .map_err(ApiError::from)?;
                entries.into_iter().map(episodic_to_item).collect()
            }
            "semantic" => {
                if has_q {
                    self.semantic_memory
                        .search(&query, Some(&aid), limit, 0.0)
                        .await
                        .map_err(ApiError::from)?
                        .into_iter()
                        .map(|r| semantic_entry_to_item(r.entry, Some(r.rrf_score)))
                        .collect()
                } else {
                    self.semantic_memory
                        .list_recent(Some(&aid), limit)
                        .await
                        .map_err(ApiError::from)?
                        .into_iter()
                        .map(|e| semantic_entry_to_item(e, None))
                        .collect()
                }
            }
            "procedural" => {
                if has_q {
                    self.procedural_memory
                        .search(&query, Some(&aid), limit, 0.0)
                        .await
                        .map_err(ApiError::from)?
                        .into_iter()
                        .map(|r| procedure_to_item(r.procedure, Some(r.rrf_score)))
                        .collect()
                } else {
                    self.procedural_memory
                        .list_by_agent(Some(&aid), limit)
                        .await
                        .map_err(ApiError::from)?
                        .into_iter()
                        .map(|p| procedure_to_item(p, None))
                        .collect()
                }
            }
            other => {
                return Err(ApiError::BadRequest(format!(
                    "unknown memory tier '{other}' (expected episodic | semantic | procedural)"
                )))
            }
        };
        Ok(items)
    }

    async fn list_providers(&self) -> Result<Vec<ApiProvider>, ApiError> {
        // Same source the bus `ListProviders` command serves, so the panel's
        // provider picker never drifts from `config/providers.toml`.
        self.provider_entries()
            .into_iter()
            .map(|v| {
                serde_json::from_value(v)
                    .map_err(|e| ApiError::Internal(format!("provider entry decode failed: {e}")))
            })
            .collect()
    }

    async fn list_skills(&self) -> Result<Vec<ApiSkillSummary>, ApiError> {
        let reg = self.skill_registry.read().await;
        Ok(reg.list().into_iter().map(skill_summary).collect())
    }

    async fn get_skill(&self, name: String) -> Result<ApiSkillDetail, ApiError> {
        let reg = self.skill_registry.read().await;
        reg.get(&name)
            .map(skill_detail)
            .ok_or_else(|| ApiError::NotFound(format!("Skill '{name}' not found")))
    }

    async fn agent_inbox(
        &self,
        agent_id: String,
        limit: Option<usize>,
    ) -> Result<Vec<ApiInboxMessage>, ApiError> {
        let aid = resolve_agent_id(&self.agent_registry, &agent_id).await?;
        let limit = limit.unwrap_or(50).min(200);
        let msgs = self.message_bus.get_history(&aid, limit).await;
        Ok(msgs.into_iter().map(inbox_message_to_api).collect())
    }

    // ── User-preference proposals ────────────────────────────────────────

    async fn list_pref_proposals(
        &self,
        status: String,
        limit: u32,
    ) -> Result<Vec<ApiPrefProposal>, ApiError> {
        use agentos_kernel::user_pref_proposals::ProposalStatus;
        let parsed = match status.to_lowercase().as_str() {
            "pending" => ProposalStatus::Pending,
            "accepted" => ProposalStatus::Accepted,
            "rejected" => ProposalStatus::Rejected,
            "expired" => ProposalStatus::Expired,
            other => {
                return Err(ApiError::BadRequest(format!(
                    "Invalid proposal status '{other}'. Expected pending|accepted|rejected|expired"
                )))
            }
        };
        let rows = self
            .user_pref_proposal_store
            .list_by_status(parsed, limit)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        Ok(rows.into_iter().map(proposal_to_api).collect())
    }

    async fn accept_pref_proposal(&self, id: String) -> Result<(), ApiError> {
        // Replicate cmd_user_prefs_accept: claim first, then apply side effect.
        let p = self
            .user_pref_proposal_store
            .get(&id)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?
            .ok_or_else(|| ApiError::NotFound(format!("Proposal '{id}' not found")))?;

        let claimed = self
            .user_pref_proposal_store
            .accept(&id)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        if !claimed {
            return Err(ApiError::Conflict("Proposal already reviewed".into()));
        }

        self.context_memory_store
            .write(
                &p.agent_id.to_string(),
                &format!("- {}", p.content),
                Some("user_pref_proposal_accept"),
            )
            .await
            .map_err(|e| {
                ApiError::Internal(format!(
                    "proposal accepted but context-memory write failed: {e}"
                ))
            })?;

        let audit = self.audit.clone();
        let entry = AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: agentos_types::TraceID::new(),
            event_type: AuditEventType::ProposalAccepted,
            agent_id: Some(p.agent_id),
            task_id: Some(p.task_id),
            tool_id: None,
            details: serde_json::json!({
                "proposal_id": id,
                "confidence": p.confidence,
                "kind": format!("{:?}", p.kind),
            }),
            severity: AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        };
        let _ = tokio::task::spawn_blocking(move || audit.append(entry)).await;
        Ok(())
    }

    async fn reject_pref_proposal(&self, id: String) -> Result<(), ApiError> {
        let proposal = self
            .user_pref_proposal_store
            .get(&id)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        let rejected = self
            .user_pref_proposal_store
            .reject(&id)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        if !rejected {
            return match proposal {
                Some(_) => Err(ApiError::Conflict("Proposal already reviewed".into())),
                None => Err(ApiError::NotFound(format!("Proposal '{id}' not found"))),
            };
        }
        if let Some(p) = proposal {
            let audit = self.audit.clone();
            let entry = AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: agentos_types::TraceID::new(),
                event_type: AuditEventType::ProposalRejected,
                agent_id: Some(p.agent_id),
                task_id: Some(p.task_id),
                tool_id: None,
                details: serde_json::json!({
                    "proposal_id": id,
                    "confidence": p.confidence,
                }),
                severity: AuditSeverity::Info,
                reversible: false,
                rollback_ref: None,
            };
            let _ = tokio::task::spawn_blocking(move || audit.append(entry)).await;
        }
        Ok(())
    }

    async fn pref_proposal_stats(&self) -> Result<ApiProposalStats, ApiError> {
        let s = self
            .user_pref_proposal_store
            .stats()
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        let proposed = s.pending + s.accepted + s.rejected + s.expired;
        Ok(ApiProposalStats {
            proposed,
            accepted: s.accepted,
            rejected: s.rejected,
            pending: s.pending,
            expired: s.expired,
        })
    }

    // ── Roles ────────────────────────────────────────────────────────────

    async fn list_roles(&self) -> Result<Vec<ApiRole>, ApiError> {
        let registry = self.agent_registry.read().await;
        Ok(registry.list_roles().into_iter().map(role_to_api).collect())
    }

    async fn create_role(&self, req: CreateRoleRequest) -> Result<ApiRole, ApiError> {
        // Build the PermissionSet from "resource:rwxqo" strings via parse_permission.
        let mut perms = agentos_types::PermissionSet::new();
        for p in &req.permissions {
            match Kernel::parse_permission(p) {
                Some((res, r, w, x, q, o)) => {
                    perms.grant(res.clone(), r, w, x, None);
                    if q {
                        perms.grant_op(res.clone(), agentos_types::PermissionOp::Query, None);
                    }
                    if o {
                        perms.grant_op(res, agentos_types::PermissionOp::Observe, None);
                    }
                }
                None => {
                    return Err(ApiError::BadRequest(format!(
                        "Invalid permission '{p}'. Expected resource:BITS (r,w,x,q,o)"
                    )))
                }
            }
        }

        let description = req.description.clone().unwrap_or_default();
        let mut registry = self.agent_registry.write().await;
        if registry.get_role_by_name(&req.name).is_some() {
            return Err(ApiError::Conflict(format!(
                "Role '{}' already exists",
                req.name
            )));
        }
        let mut role = agentos_types::Role::new(req.name.clone(), description);
        role.permissions = perms;
        let id = registry.register_role(role);
        let created = registry
            .get_role_by_id(&id)
            .ok_or_else(|| ApiError::Internal("Role registered but not found".into()))?;
        Ok(role_to_api(created))
    }

    async fn get_role(&self, name: &str) -> Result<ApiRole, ApiError> {
        let registry = self.agent_registry.read().await;
        let role = registry
            .get_role_by_name(name)
            .ok_or_else(|| ApiError::NotFound(format!("Role '{name}' not found")))?;
        Ok(role_to_api(role))
    }

    async fn delete_role(&self, name: &str) -> Result<(), ApiError> {
        let mut registry = self.agent_registry.write().await;
        let id = registry
            .get_role_by_name(name)
            .map(|r| r.id)
            .ok_or_else(|| ApiError::NotFound(format!("Role '{name}' not found")))?;
        registry.unregister_role(&id).map_err(ApiError::Conflict)
    }

    // ── Audit integrity ──────────────────────────────────────────────────

    async fn verify_audit_chain(&self) -> Result<serde_json::Value, ApiError> {
        let audit = self.audit.clone();
        let verification = tokio::task::spawn_blocking(move || audit.verify_chain(None))
            .await
            .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        Ok(serde_json::json!({
            "valid": verification.valid,
            "entries_checked": verification.entries_checked,
            "gaps": verification.gaps,
            "first_invalid_seq": verification.first_invalid_seq,
            "error": verification.error,
        }))
    }

    // ── Config ──────────────────────────────────────────────────────────

    async fn get_config_tree(&self) -> Result<serde_json::Value, ApiError> {
        let mut value = serde_json::to_value(&self.config)
            .map_err(|e| ApiError::Internal(format!("Serialize config: {e}")))?;
        redact_secrets(&mut value);
        Ok(value)
    }

    async fn get_config_key(&self, key: &str) -> Result<serde_json::Value, ApiError> {
        let path = self.config_path().to_path_buf();
        let key = key.to_string();
        tokio::task::spawn_blocking(move || read_config_key(&path, &key))
            .await
            .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
    }

    async fn set_config_key(&self, key: &str, value: serde_json::Value) -> Result<(), ApiError> {
        let path = self.config_path().to_path_buf();
        let key = key.to_string();
        let value_str = match value {
            serde_json::Value::String(s) => s,
            other => other.to_string(),
        };
        tokio::task::spawn_blocking(move || {
            let content = std::fs::read_to_string(&path).map_err(|e| {
                ApiError::Internal(format!("Cannot read config at {}: {e}", path.display()))
            })?;
            let mut doc: toml_edit::DocumentMut = content
                .parse()
                .map_err(|e| ApiError::Internal(format!("Config parse error: {e}")))?;
            set_dotted_key(&mut doc, &key, &value_str)?;
            std::fs::write(&path, doc.to_string())
                .map_err(|e| ApiError::Internal(format!("Cannot write config: {e}")))
        })
        .await
        .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
    }

    fn config_writable(&self) -> bool {
        self.config.api.config_writable
    }

    // ── Doctor ──────────────────────────────────────────────────────────

    async fn run_doctor(&self) -> Result<Vec<DoctorCheck>, ApiError> {
        let config_path = self.config_path().to_path_buf();
        let vault = std::path::PathBuf::from(&self.config.secrets.vault_path);
        let audit = std::path::PathBuf::from(&self.config.audit.log_path);
        let socket = std::path::PathBuf::from(&self.config.bus.socket_path);
        let core_dir = self.config.tools.core_tools_dir.clone();
        tokio::task::spawn_blocking(move || {
            doctor_run_checks(&config_path, &vault, &audit, &socket, &core_dir, false)
        })
        .await
        .map_err(|e| ApiError::Internal(format!("Join error: {e}")))
    }

    async fn apply_doctor_fix(&self, _check: &str) -> Result<(), ApiError> {
        let config_path = self.config_path().to_path_buf();
        let vault = std::path::PathBuf::from(&self.config.secrets.vault_path);
        let audit = std::path::PathBuf::from(&self.config.audit.log_path);
        let socket = std::path::PathBuf::from(&self.config.bus.socket_path);
        let core_dir = self.config.tools.core_tools_dir.clone();
        tokio::task::spawn_blocking(move || {
            let _ = doctor_run_checks(&config_path, &vault, &audit, &socket, &core_dir, true);
        })
        .await
        .map_err(|e| ApiError::Internal(format!("Join error: {e}")))
    }

    // ── Logs ────────────────────────────────────────────────────────────

    async fn query_logs(
        &self,
        level: Option<String>,
        since: Option<String>,
        limit: u32,
    ) -> Result<Vec<LogLine>, ApiError> {
        let dir = self.config.logging.log_dir.clone();
        tokio::task::spawn_blocking(move || query_logs_dir(&dir, level, since, limit))
            .await
            .map_err(|e| ApiError::Internal(format!("Join error: {e}")))
    }

    // ── Resources ───────────────────────────────────────────────────────

    async fn get_resources(&self) -> Result<ResourceInfo, ApiError> {
        let snapshot = agentos_hal::drivers::system::SystemDriver::new()
            .snapshot()
            .map_err(|e| ApiError::Internal(format!("System snapshot: {e}")))?;

        let (disk_total, disk_free) = snapshot
            .disk_usage
            .iter()
            .max_by_key(|d| d.total_space_bytes)
            .map(|d| (d.total_space_bytes, d.available_space_bytes))
            .unwrap_or((0, 0));

        let locks = self
            .resource_arbiter
            .list_locks()
            .await
            .into_iter()
            .map(|l| ResourceLockInfo {
                resource_id: l.resource_id,
                lock_mode: l.lock_mode,
                held_by: l.held_by,
                acquired_at: l.acquired_at,
                ttl_seconds: l.ttl_seconds,
                waiters: l.waiters,
            })
            .collect();
        let contention = self.resource_arbiter.contention_stats().await;

        Ok(ResourceInfo {
            data_dir: self.data_dir().display().to_string(),
            disk_free_bytes: disk_free,
            disk_total_bytes: disk_total,
            mem_used_mb: snapshot.memory_used_mb,
            mem_total_mb: snapshot.memory_total_mb,
            locks,
            contention,
        })
    }

    // ── HAL ─────────────────────────────────────────────────────────────

    async fn get_hal_info(&self) -> Result<HalInfo, ApiError> {
        let devices = self
            .hardware_registry
            .list_devices()
            .into_iter()
            .map(|d| HalDevice {
                id: d.id,
                device_type: d.device_type,
                status: match d.status {
                    agentos_hal::DeviceStatus::Pending => "pending",
                    agentos_hal::DeviceStatus::Approved => "approved",
                    agentos_hal::DeviceStatus::Quarantined => "quarantined",
                }
                .to_string(),
                granted_to: d.granted_to.iter().map(|a| a.to_string()).collect(),
                denied_to: d.denied_to.iter().map(|a| a.to_string()).collect(),
            })
            .collect();

        let system = agentos_hal::drivers::system::SystemDriver::new()
            .snapshot()
            .ok()
            .and_then(|s| serde_json::to_value(s).ok())
            .unwrap_or(serde_json::Value::Null);

        Ok(HalInfo { devices, system })
    }

    // ── Automation: task resume / checkpoints ────────────────────────────

    async fn resume_task(&self, id: TaskID) -> Result<serde_json::Value, ApiError> {
        match self.cmd_resume_task(id).await {
            agentos_bus::KernelResponse::Success { data } => {
                Ok(data.unwrap_or_else(|| serde_json::json!({ "resumed": id.to_string() })))
            }
            agentos_bus::KernelResponse::Error { message } => {
                if message.contains("no checkpoint") || message.contains("not found") {
                    Err(ApiError::NotFound(message))
                } else {
                    Err(ApiError::Conflict(message))
                }
            }
            _ => Err(ApiError::Internal(
                "Unexpected kernel response resuming task".into(),
            )),
        }
    }

    async fn list_task_checkpoints(
        &self,
        id: TaskID,
    ) -> Result<Vec<ApiCheckpointSummary>, ApiError> {
        let record = self
            .checkpoint_store
            .get_latest(&id)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        let Some(rec) = record else {
            return Ok(Vec::new());
        };
        let tool_calls = serde_json::from_slice::<
            agentos_kernel::checkpoint_store::CheckpointPayload,
        >(&rec.state_blob)
        .map(|p| p.tool_call_history.len() as u32)
        .unwrap_or(0);
        Ok(vec![ApiCheckpointSummary {
            task_id: rec.task_id.to_string(),
            created_at: rec.created_at,
            iteration: rec.step_num,
            tool_calls,
        }])
    }

    // ── Automation: pipelines ────────────────────────────────────────────

    async fn import_pipeline(&self, yaml: String) -> Result<String, ApiError> {
        let def: serde_json::Value = serde_yaml::from_str(&yaml)
            .map_err(|e| ApiError::BadRequest(format!("Invalid pipeline YAML: {e}")))?;
        let name = def
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ApiError::BadRequest("Pipeline YAML missing 'name'".into()))?
            .to_string();
        let version = def
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("1.0.0")
            .to_string();
        agentos_pipeline::PipelineDefinition::from_yaml(&yaml)
            .map_err(|e| ApiError::BadRequest(format!("Invalid pipeline definition: {e}")))?;
        let store = self.pipeline_engine.store_arc();
        let name_c = name.clone();
        // An import is always a create — it must not silently replace a pipeline
        // that happens to share the imported document's name.
        tokio::task::spawn_blocking(move || store.create_pipeline(&name_c, &version, &yaml))
            .await
            .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
            .map_err(|e| ApiError::Internal(e.to_string()))
            .and_then(|created| {
                if created {
                    Ok(())
                } else {
                    Err(ApiError::Conflict(format!(
                        "Pipeline '{name}' already exists — delete it first, or rename it in the YAML"
                    )))
                }
            })?;
        Ok(name)
    }

    async fn export_pipeline(&self, name: &str) -> Result<String, ApiError> {
        let store = self.pipeline_engine.store_arc();
        let name = name.to_string();
        tokio::task::spawn_blocking(move || store.get_pipeline_yaml(&name))
            .await
            .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
            .map_err(|_| ApiError::NotFound("Pipeline not found".into()))
    }

    async fn get_pipeline_definition(&self, name: &str) -> Result<serde_json::Value, ApiError> {
        let store = self.pipeline_engine.store_arc();
        let name = name.to_string();
        let yaml = tokio::task::spawn_blocking(move || store.get_pipeline_yaml(&name))
            .await
            .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
            .map_err(|_| ApiError::NotFound("Pipeline not found".into()))?;
        serde_yaml::from_str::<serde_json::Value>(&yaml)
            .map_err(|e| ApiError::Internal(format!("Pipeline YAML parse error: {e}")))
    }

    async fn get_pipeline_run(&self, run_id: String) -> Result<serde_json::Value, ApiError> {
        let rid: agentos_types::RunID = run_id
            .parse()
            .map_err(|_| ApiError::BadRequest(format!("Invalid run ID: {run_id}")))?;
        let store = self.pipeline_engine.store_arc();
        let run = tokio::task::spawn_blocking(move || store.get_run(&rid))
            .await
            .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
            .map_err(|_| ApiError::NotFound(format!("Pipeline run '{run_id}' not found")))?;
        serde_json::to_value(run).map_err(|e| ApiError::Internal(e.to_string()))
    }

    // ── Automation: schedules ────────────────────────────────────────────

    async fn list_schedules(&self) -> Result<Vec<ApiScheduleSummary>, ApiError> {
        // Unified view across all three schedule kinds so agent-created
        // once-jobs and timers are visible alongside cron schedules.
        let mut entries: Vec<ApiScheduleSummary> = self
            .schedule_manager
            .list_jobs()
            .await
            .iter()
            .map(schedule_to_api)
            .collect();
        // `run_count` on the job lags the run history after restarts (panel
        // showed 1 against 13 recorded runs); the history is the durable count.
        if let Some(store) = self.schedule_manager.store() {
            let counts = store.count_runs_by_parent().await.unwrap_or_default();
            for e in entries.iter_mut() {
                if let Some(n) = counts.get(&e.id) {
                    e.run_count = e.run_count.max(*n);
                }
            }
        }
        entries.extend(
            self.schedule_manager
                .list_once_jobs()
                .await
                .iter()
                .map(once_job_to_api),
        );
        entries.extend(
            self.schedule_manager
                .list_timers()
                .await
                .iter()
                .map(timer_to_api),
        );
        // Soonest-firing first; entries with no upcoming fire time sort last.
        entries.sort_by(|a, b| match (a.next_run_at, b.next_run_at) {
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.name.cmp(&b.name),
        });
        Ok(entries)
    }

    async fn create_schedule(
        &self,
        req: CreateScheduleRequest,
    ) -> Result<ApiScheduleSummary, ApiError> {
        let id = self
            .schedule_manager
            .create_job(
                req.name.clone(),
                req.cron.clone(),
                req.agent_name.clone(),
                req.prompt.clone(),
                Vec::new(),
            )
            .await
            .map_err(|e| ApiError::BadRequest(e.to_string()))?;
        let job = self
            .schedule_manager
            .get_job(&id)
            .await
            .ok_or_else(|| ApiError::Internal("Schedule created but not found".into()))?;
        Ok(schedule_to_api(&job))
    }

    async fn pause_schedule(&self, id: &str) -> Result<(), ApiError> {
        let sid: agentos_types::ScheduleID = id
            .parse()
            .map_err(|_| ApiError::BadRequest(format!("Invalid schedule ID: {id}")))?;
        self.schedule_manager
            .pause(&sid)
            .await
            .map_err(|e| ApiError::NotFound(e.to_string()))
    }

    async fn resume_schedule(&self, id: &str) -> Result<(), ApiError> {
        let sid: agentos_types::ScheduleID = id
            .parse()
            .map_err(|_| ApiError::BadRequest(format!("Invalid schedule ID: {id}")))?;
        self.schedule_manager
            .resume(&sid)
            .await
            .map_err(|e| ApiError::NotFound(e.to_string()))
    }

    async fn delete_schedule(&self, id: &str) -> Result<(), ApiError> {
        let sid: agentos_types::ScheduleID = id
            .parse()
            .map_err(|_| ApiError::BadRequest(format!("Invalid schedule ID: {id}")))?;
        // The id may belong to any schedule kind; fall through cron → once → timer.
        if self.schedule_manager.delete(&sid).await.is_ok() {
            return Ok(());
        }
        if self.schedule_manager.cancel_once_job(&sid).await.is_ok() {
            return Ok(());
        }
        self.schedule_manager
            .cancel_timer(&sid)
            .await
            .map(|_| ())
            .map_err(|_| ApiError::NotFound(format!("Schedule {id} not found")))
    }

    async fn get_schedule_runs(
        &self,
        id: &str,
        limit: u32,
    ) -> Result<Vec<ApiScheduleRun>, ApiError> {
        let sid: agentos_types::ScheduleID = id
            .parse()
            .map_err(|_| ApiError::BadRequest(format!("Invalid schedule ID: {id}")))?;
        let Some(store) = self.schedule_manager.store() else {
            return Ok(Vec::new());
        };
        let runs = store
            .list_runs_for_schedule(sid, limit)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        Ok(runs
            .into_iter()
            .map(|r| ApiScheduleRun {
                run_id: r.run_id.to_string(),
                fired_at: Some(r.started_at),
                status: r.state.as_str().to_string(),
                task_id: r.task_id.map(|t| t.to_string()),
            })
            .collect())
    }

    // ── Automation: workflows (JSON file store) ──────────────────────────

    async fn list_workflows(&self) -> Result<Vec<ApiWorkflowSummary>, ApiError> {
        let dir = self.data_dir().join("workflows");
        tokio::task::spawn_blocking(move || -> Result<Vec<ApiWorkflowSummary>, ApiError> {
            let mut out = Vec::new();
            let rd = match std::fs::read_dir(&dir) {
                Ok(rd) => rd,
                Err(_) => return Ok(out),
            };
            for entry in rd.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.extension().map(|x| x == "json").unwrap_or(false) {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
                            let id = v
                                .get("id")
                                .and_then(|x| x.as_str())
                                .map(|s| s.to_string())
                                .or_else(|| {
                                    path.file_stem().map(|s| s.to_string_lossy().into_owned())
                                })
                                .unwrap_or_default();
                            let name = v
                                .get("name")
                                .and_then(|x| x.as_str())
                                .unwrap_or(&id)
                                .to_string();
                            let version = v
                                .get("version")
                                .and_then(|x| x.as_str())
                                .unwrap_or("1.0.0")
                                .to_string();
                            let node_count = v
                                .get("nodes")
                                .and_then(|x| x.as_array())
                                .map(|a| a.len())
                                .unwrap_or(0);
                            out.push(ApiWorkflowSummary {
                                id,
                                name,
                                version,
                                node_count,
                                status: "saved".to_string(),
                            });
                        }
                    }
                }
            }
            out.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(out)
        })
        .await
        .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
    }

    async fn get_workflow(&self, id: &str) -> Result<serde_json::Value, ApiError> {
        validate_workflow_id(id)?;
        let path = self.data_dir().join("workflows").join(format!("{id}.json"));
        tokio::task::spawn_blocking(move || -> Result<serde_json::Value, ApiError> {
            let content = std::fs::read_to_string(&path)
                .map_err(|_| ApiError::NotFound("Workflow not found".into()))?;
            serde_json::from_str(&content)
                .map_err(|e| ApiError::Internal(format!("Workflow parse error: {e}")))
        })
        .await
        .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
    }

    async fn save_workflow(&self, req: SaveWorkflowRequest) -> Result<String, ApiError> {
        let id = req
            .definition
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        validate_workflow_id(&id)?;

        let mut doc = req.definition.clone();
        if let serde_json::Value::Object(map) = &mut doc {
            map.insert("id".to_string(), serde_json::Value::String(id.clone()));
            map.entry("name".to_string())
                .or_insert(serde_json::Value::String(req.name.clone()));
            map.entry("version".to_string())
                .or_insert(serde_json::Value::String("1.0.0".to_string()));
        } else {
            return Err(ApiError::BadRequest(
                "Workflow definition must be a JSON object".into(),
            ));
        }

        let dir = self.data_dir().join("workflows");
        let path = dir.join(format!("{id}.json"));
        let id_ret = id.clone();
        tokio::task::spawn_blocking(move || -> Result<(), ApiError> {
            std::fs::create_dir_all(&dir)
                .map_err(|e| ApiError::Internal(format!("Cannot create workflows dir: {e}")))?;
            let body = serde_json::to_string_pretty(&doc)
                .map_err(|e| ApiError::Internal(e.to_string()))?;
            std::fs::write(&path, body)
                .map_err(|e| ApiError::Internal(format!("Cannot write workflow: {e}")))
        })
        .await
        .map_err(|e| ApiError::Internal(format!("Join error: {e}")))??;
        Ok(id_ret)
    }

    async fn delete_workflow(&self, id: &str) -> Result<(), ApiError> {
        validate_workflow_id(id)?;
        let path = self.data_dir().join("workflows").join(format!("{id}.json"));
        tokio::task::spawn_blocking(move || -> Result<(), ApiError> {
            std::fs::remove_file(&path).map_err(|_| ApiError::NotFound("Workflow not found".into()))
        })
        .await
        .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
    }

    // ── Extensibility (Phase 05) ─────────────────────────────────────────

    async fn list_plugins(&self) -> Result<Vec<ApiPluginSummary>, ApiError> {
        let user_dir = self.user_plugins_dir();
        Ok(self
            .plugin_registry
            .list()
            .await
            .into_iter()
            .map(|p| plugin_to_summary(p, &user_dir))
            .collect())
    }

    async fn get_plugin(&self, id: &str) -> Result<ApiPluginDetail, ApiError> {
        let entry = self
            .plugin_registry
            .list()
            .await
            .into_iter()
            .find(|p| p.manifest.id == id)
            .ok_or_else(|| ApiError::NotFound(format!("Plugin '{id}' not found")))?;
        Ok(plugin_to_detail(entry, &self.user_plugins_dir()))
    }

    async fn discover_plugins(&self) -> Result<DiscoverPluginsResponse, ApiError> {
        let data_dir = std::path::PathBuf::from(&self.config.tools.data_dir);
        let base = data_dir.parent().unwrap_or(&data_dir).to_path_buf();
        let dirs = vec![base.join("plugins/core"), base.join("plugins/user")];
        let discovered = self.plugin_registry.discover(&dirs).await as u64;
        let user_dir = self.user_plugins_dir();
        let plugins = self
            .plugin_registry
            .list()
            .await
            .into_iter()
            .map(|p| plugin_to_summary(p, &user_dir))
            .collect();
        Ok(DiscoverPluginsResponse {
            discovered,
            plugins,
        })
    }

    async fn set_plugin_enabled(&self, id: &str, enabled: bool) -> Result<(), ApiError> {
        let result = if enabled {
            self.plugin_registry.activate(id).await
        } else {
            self.plugin_registry.deactivate(id).await
        };
        result.map_err(|e| ApiError::Conflict(e.to_string()))
    }

    async fn list_channels(&self) -> Result<Vec<ApiChannelSummary>, ApiError> {
        let rows = self
            .channel_registry
            .list_active()
            .await
            .map_err(|e| ApiError::Internal(format!("Channel registry error: {e}")))?;
        let health: std::collections::HashMap<String, String> = self
            .channel_manager
            .health()
            .await
            .into_iter()
            .map(|(id, status)| (id, format!("{status:?}")))
            .collect();
        Ok(rows
            .into_iter()
            .map(|ch| channel_to_summary(ch, &health))
            .collect())
    }

    async fn get_channel(&self, id: &str) -> Result<ApiChannelSummary, ApiError> {
        let cid: agentos_types::ChannelInstanceID = id
            .parse()
            .map_err(|_| ApiError::BadRequest(format!("Invalid channel ID: {id}")))?;
        let ch = self
            .channel_registry
            .get_by_id(&cid)
            .await
            .map_err(|e| ApiError::Internal(format!("Channel registry error: {e}")))?
            .ok_or_else(|| ApiError::NotFound(format!("Channel '{id}' not found")))?;
        let health: std::collections::HashMap<String, String> = self
            .channel_manager
            .health()
            .await
            .into_iter()
            .map(|(hid, status)| (hid, format!("{status:?}")))
            .collect();
        Ok(channel_to_summary(ch, &health))
    }

    async fn disconnect_channel(&self, id: &str) -> Result<(), ApiError> {
        let cid: agentos_types::ChannelInstanceID = id
            .parse()
            .map_err(|_| ApiError::BadRequest(format!("Invalid channel ID: {id}")))?;
        if !matches!(self.channel_registry.get_by_id(&cid).await, Ok(Some(_))) {
            return Err(ApiError::NotFound(format!("Channel '{id}' not found")));
        }
        // The kernel command stops the listener, deletes a Telegram webhook and
        // its secret, and drops routes. Deregistering only the registry row left
        // the webhook verifying — and, with the pinned chat id gone, accepting
        // messages from ANY chat.
        match self.cmd_disconnect_channel(id.to_string()).await {
            agentos_bus::KernelResponse::Error { message } => Err(ApiError::Internal(message)),
            _ => Ok(()),
        }
    }

    async fn list_mcp_servers(&self) -> Result<Vec<ApiMcpServer>, ApiError> {
        let live = self.mcp_supervisor.server_statuses().await;
        let attachments = self
            .mcp_attachment_store
            .list_all()
            .await
            .unwrap_or_default();

        let mut by_name: std::collections::HashMap<String, ApiMcpServer> =
            std::collections::HashMap::new();

        for (name, state, tool_count, stats, note) in live {
            by_name.insert(
                name.clone(),
                ApiMcpServer {
                    permission: mcp_server_permission(&name),
                    name,
                    state: Some(format!("{state:?}")),
                    tool_count,
                    stats: Some(ApiMcpStats {
                        total_calls: stats.total_calls,
                        failure_count: stats.failure_count,
                        avg_latency_ms: stats.avg_latency_ms,
                    }),
                    note,
                    transport: None,
                    command: None,
                    args: Vec::new(),
                    url: None,
                    timeout_secs: None,
                    oauth_connector_id: None,
                    has_auth_token: false,
                    env_keys: Vec::new(),
                    created_at: None,
                },
            );
        }

        for a in attachments {
            let transport = if a.command.is_some() {
                "stdio"
            } else if a.url.is_some() {
                "http"
            } else {
                "unknown"
            };
            let entry = by_name
                .entry(a.name.clone())
                .or_insert_with(|| ApiMcpServer {
                    permission: mcp_server_permission(&a.name),
                    name: a.name.clone(),
                    state: None,
                    tool_count: 0,
                    stats: None,
                    note: None,
                    transport: None,
                    command: None,
                    args: Vec::new(),
                    url: None,
                    timeout_secs: None,
                    oauth_connector_id: None,
                    has_auth_token: false,
                    env_keys: Vec::new(),
                    created_at: None,
                });
            entry.transport = Some(transport.to_string());
            entry.command = a.command;
            entry.args = a.args;
            entry.url = a.url;
            entry.timeout_secs = a.timeout_secs;
            entry.oauth_connector_id = a.oauth_connector_id;
            entry.has_auth_token = a.auth_token.is_some();
            let mut keys: Vec<String> = a.env.into_keys().collect();
            keys.sort();
            entry.env_keys = keys;
            entry.created_at = Some(a.created_at);
        }

        let mut servers: Vec<ApiMcpServer> = by_name.into_values().collect();
        servers.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(servers)
    }

    async fn detach_mcp_server(&self, name: &str) -> Result<(), ApiError> {
        // Delegate to the kernel command rather than poking the supervisor:
        // it also unregisters the server's tools from the ToolRegistry and the
        // ToolRunner and revokes the auto-vaulted token. Removing the server
        // alone leaves those tools behind, where they shadow a later re-attach
        // of the same server (every tool is skipped as a name conflict).
        if matches!(
            self.cmd_mcp_detach(name.to_string()).await,
            agentos_bus::KernelResponse::McpDetached
        ) {
            return Ok(());
        }
        // Not live — it may exist only as a persisted attachment. Revoke its
        // auto-vaulted token too, or it lingers invisibly after the row is gone.
        if self
            .mcp_attachment_store
            .delete(name)
            .await
            .unwrap_or(false)
        {
            let _ = self.vault.revoke(&format!("mcp.{name}.auth_token")).await;
            Ok(())
        } else {
            Err(ApiError::NotFound(format!("MCP server '{name}' not found")))
        }
    }

    async fn list_connectors(&self) -> Result<Vec<ApiConnectorSummary>, ApiError> {
        let registered = self.connector_registry.list().await;
        let creds = self.vault.oauth_store().list().await.unwrap_or_default();
        Ok(registered
            .into_iter()
            .map(|m| {
                let cred = creds.iter().find(|c| c.connector_id == m.connector.id);
                ApiConnectorSummary {
                    id: m.connector.id.clone(),
                    name: m.connector.name.clone(),
                    connected: cred.is_some(),
                    provider: cred.map(|c| c.provider.clone()),
                    scopes: cred.map(|c| c.scopes.clone()).unwrap_or_default(),
                    expires_at: cred.and_then(|c| c.expires_at),
                    // Rows come from the registry, so `registered` is by
                    // construction; `oauth_available` awaits the connector REST work.
                    registered: true,
                    oauth_available: false,
                }
            })
            .collect())
    }

    async fn get_connector(&self, id: &str) -> Result<ApiConnectorDetail, ApiError> {
        let manifest = self
            .connector_registry
            .list()
            .await
            .into_iter()
            .find(|m| m.connector.id == id)
            .ok_or_else(|| ApiError::NotFound(format!("Connector '{id}' not found")))?;
        let creds = self.vault.oauth_store().list().await.unwrap_or_default();
        let cred = creds.iter().find(|c| c.connector_id == id);
        let tools = manifest
            .tools
            .iter()
            .map(|t| format!("{}.{}", manifest.connector.id, t.name))
            .collect();
        Ok(ApiConnectorDetail {
            id: manifest.connector.id.clone(),
            name: manifest.connector.name.clone(),
            version: manifest.connector.version.clone(),
            description: manifest.connector.description.clone(),
            base_url: manifest.connector.base_url.clone(),
            connected: cred.is_some(),
            provider: cred.map(|c| c.provider.clone()),
            scopes: cred.map(|c| c.scopes.clone()).unwrap_or_default(),
            expires_at: cred.and_then(|c| c.expires_at),
            tools,
            manifest_toml: std::fs::read_to_string(
                self.connectors_dir().join(format!("{id}.toml")),
            )
            .ok(),
        })
    }

    async fn disconnect_connector(&self, id: &str) -> Result<(), ApiError> {
        // Disconnect is idempotent (a missing credential/registration is a
        // success — the desired end-state is "gone"), but a genuine failure must
        // not be silently swallowed: otherwise the operator believes the OAuth
        // token was revoked while it is still stored. Log failures loudly.
        if let Err(e) = self.vault.oauth_store().delete(id).await {
            tracing::warn!(
                connector_id = %id,
                error = %e,
                "disconnect_connector: OAuth credential delete did not succeed (may already be absent)"
            );
        }
        if let Err(e) = self.connector_registry.deregister(id).await {
            tracing::warn!(
                connector_id = %id,
                error = %e,
                "disconnect_connector: connector deregister did not succeed (may already be absent)"
            );
        }
        Ok(())
    }

    async fn attach_mcp_server(
        &self,
        req: AttachMcpRequest,
    ) -> Result<McpAttachedResponse, ApiError> {
        let name = req.name.trim().to_string();
        validate_attach_request(&req)?;

        let resp = self
            .cmd_mcp_attach(
                name.clone(),
                req.command.filter(|c| !c.trim().is_empty()),
                req.args,
                req.url.filter(|u| !u.trim().is_empty()),
                req.auth_token.map(|t| t.to_string()),
                req.oauth_connector_id,
                req.timeout_secs,
                req.env.unwrap_or_default(),
            )
            .await;
        match resp {
            agentos_bus::KernelResponse::McpAttached { tools, .. } => {
                Ok(McpAttachedResponse { name, tools })
            }
            agentos_bus::KernelResponse::Error { message } => Err(ApiError::Conflict(message)),
            _ => Err(ApiError::Internal(
                "Unexpected kernel response attaching MCP server".into(),
            )),
        }
    }

    async fn update_mcp_server(
        &self,
        name: &str,
        req: AttachMcpRequest,
    ) -> Result<McpAttachedResponse, ApiError> {
        // An MCP attachment is its transport — there is nothing to mutate in
        // place, so an edit is a detach followed by a fresh attach under the
        // same name. Validate the new config *before* tearing the old one down.
        let name = name.to_string();
        // Connectors and plugins both 400 on an id mismatch; be consistent
        // rather than silently renaming what the caller asked for.
        let body_name = req.name.trim().to_string();
        if !body_name.is_empty() && body_name != name {
            return Err(ApiError::BadRequest(format!(
                "Body name '{body_name}' does not match '{name}' — renaming is detach + attach"
            )));
        }
        if !self
            .list_mcp_servers()
            .await?
            .iter()
            .any(|srv| srv.name == name)
        {
            return Err(ApiError::NotFound(format!(
                "MCP server '{name}' is not attached"
            )));
        }
        // Neither the env values nor the bearer token are ever returned to the
        // panel, so an edit that leaves them out must carry the stored ones
        // forward rather than silently dropping them.
        let stored = self
            .mcp_attachment_store
            .list_all()
            .await
            .unwrap_or_default()
            .into_iter()
            .find(|a| a.name == name);
        let carried_token = match (&req.auth_token, &req.oauth_connector_id, &stored) {
            (None, None, Some(a)) => match a.auth_token.as_deref() {
                // Stored as a `vault:` reference — re-attach needs the plaintext,
                // which it re-vaults under the same key. Fail closed if it
                // cannot be read: proceeding would detach (revoking the key) and
                // re-attach with no auth at all, destroying the token.
                Some(v) => match v.strip_prefix("vault:") {
                    Some(key) => Some(zeroize::Zeroizing::new(
                        self.vault
                            .get(key)
                            .await
                            .map_err(|e| {
                                ApiError::Internal(format!(
                                    "Cannot read the stored token for '{name}': {e} — the edit was not applied"
                                ))
                            })?
                            .as_str()
                            .to_string(),
                    )),
                    None => Some(zeroize::Zeroizing::new(v.to_string())),
                },
                None => None,
            },
            _ => None,
        };
        let mut req = req;
        req.name = name.clone();
        req.auth_token = req.auth_token.or_else(|| carried_token.clone());
        req.env = req.env.or_else(|| stored.as_ref().map(|a| a.env.clone()));
        // W4's siblings: `args` and `timeout_secs` deserve the same carry-forward
        // as `env`, or a scripted PUT that omits them re-attaches a bare command.
        if req.args.is_empty() {
            req.args = stored.as_ref().map(|a| a.args.clone()).unwrap_or_default();
        }
        req.timeout_secs = req
            .timeout_secs
            .or_else(|| stored.as_ref().and_then(|a| a.timeout_secs));
        validate_attach_request(&req)?;
        self.detach_mcp_server(&name).await?;
        match self.attach_mcp_server(req).await {
            Ok(resp) => Ok(resp),
            Err(ApiError::Conflict(m)) | Err(ApiError::BadRequest(m)) => {
                // The detach revoked the token's vault key. Put it back, or a
                // typo'd command costs the operator a secret they may not have
                // a second copy of.
                let restored = match &carried_token {
                    Some(token) => self
                        .vault
                        .set(
                            &format!("mcp.{name}.auth_token"),
                            token.as_str(),
                            agentos_types::SecretOwner::Kernel,
                            SecretScope::Global,
                        )
                        .await
                        .is_ok(),
                    None => true,
                };
                Err(ApiError::Conflict(format!(
                    "{m} — '{name}' was detached and the new configuration did not attach; fix it and attach again{}",
                    if restored { "" } else { " (its stored token could not be restored)" }
                )))
            }
            Err(other) => Err(other),
        }
    }

    async fn list_mcp_catalog(&self, q: Option<&str>) -> Result<Vec<ApiMcpCatalogEntry>, ApiError> {
        // "installed" means a server is attached under the catalog id — that is
        // the name `cmd_mcp_install` attaches with, so the check must use ids.
        let attached: std::collections::HashSet<String> = self
            .list_mcp_servers()
            .await?
            .into_iter()
            .map(|s| s.name)
            .collect();
        let entries = match q.map(str::trim).filter(|s| !s.is_empty()) {
            Some(query) => self.mcp_catalog.search(query),
            None => self.mcp_catalog.list(),
        };
        Ok(entries
            .into_iter()
            .map(|e| ApiMcpCatalogEntry {
                installed: attached.contains(&e.id),
                id: e.id.clone(),
                display_name: e.display_name.clone(),
                description: e.description.clone(),
                trust_tier: e.trust_tier.clone(),
                transport: e.mcp.transport.clone(),
                runtime: e.install.runtime.clone(),
                homepage: e.homepage.clone(),
            })
            .collect())
    }

    async fn get_mcp_catalog_entry(&self, id: &str) -> Result<serde_json::Value, ApiError> {
        match self.cmd_mcp_catalog_info(id.to_string()).await {
            agentos_bus::KernelResponse::McpCatalogInfo(v) => Ok(v),
            agentos_bus::KernelResponse::Error { message } => Err(ApiError::NotFound(message)),
            _ => Err(ApiError::Internal(
                "Unexpected kernel response reading MCP catalog".into(),
            )),
        }
    }

    async fn install_mcp_server(
        &self,
        id: &str,
        req: InstallMcpRequest,
    ) -> Result<McpAttachedResponse, ApiError> {
        if self.mcp_catalog.lookup(id).is_none() {
            return Err(ApiError::NotFound(format!("No catalog entry '{id}'")));
        }
        let resp = self
            .cmd_mcp_install(
                id.to_string(),
                true,
                req.allow_community,
                req.runtime_binary,
                req.no_auth,
            )
            .await;
        match resp {
            agentos_bus::KernelResponse::McpAttached { tools, .. } => Ok(McpAttachedResponse {
                name: id.to_string(),
                tools,
            }),
            agentos_bus::KernelResponse::Error { message } => Err(ApiError::Conflict(message)),
            _ => Err(ApiError::Internal(
                "Unexpected kernel response installing MCP server".into(),
            )),
        }
    }

    async fn connect_channel(
        &self,
        req: ConnectChannelRequest,
    ) -> Result<ApiChannelSummary, ApiError> {
        use std::str::FromStr;
        let display_name = req.display_name.trim().to_string();
        if display_name.is_empty() {
            return Err(ApiError::BadRequest("`display_name` is required".into()));
        }
        // `ChannelKind::from_str` is infallible (unknown → Custom), so an
        // unsupported kind must be rejected here or it registers a dead channel
        // with no adapter that still reports "connected".
        let kind = agentos_types::ChannelKind::from_str(req.kind.trim())
            .map_err(|e| ApiError::BadRequest(e.to_string()))?;
        if matches!(kind, agentos_types::ChannelKind::Custom(_)) {
            return Err(ApiError::BadRequest(format!(
                "Unsupported channel kind '{}' (telegram, ntfy, email, discord, slack, whatsapp, webhook)",
                req.kind
            )));
        }

        // An inline credential is stored in the vault first; only the key is
        // persisted with the channel row.
        let mut credential_key = req.credential_key.unwrap_or_default().trim().to_string();
        if !credential_key.is_empty() {
            validate_channel_credential_key(&credential_key)?;
        }
        if let Some(secret) = req.credential {
            if credential_key.is_empty() {
                let slug: String = display_name
                    .to_lowercase()
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                    .collect();
                let base = format!("channel.{kind}.{}", slug.trim_matches('-'));
                // Two channels named alike would derive the same key, and the
                // second token would silently overwrite the first — breaking a
                // working channel. Take the next free suffix instead.
                credential_key = base.clone();
                let mut n = 2;
                while self.vault.get(&credential_key).await.is_ok() {
                    credential_key = format!("{base}-{n}");
                    n += 1;
                }
            }
            self.vault
                .set(
                    &credential_key,
                    secret.as_str(),
                    agentos_types::SecretOwner::Kernel,
                    SecretScope::Global,
                )
                .await
                .map_err(|e| {
                    ApiError::Internal(format!("Failed to store channel credential: {e}"))
                })?;
        }

        let resp = self
            .cmd_connect_channel(
                kind,
                req.external_id.unwrap_or_default(),
                display_name,
                credential_key,
                req.reply_topic,
                req.server_url,
                req.webhook_url,
                req.active_agent_name,
            )
            .await;
        let channel_id = match resp {
            agentos_bus::KernelResponse::Success { data } => data
                .as_ref()
                .and_then(|d| d.get("channel_id"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .ok_or_else(|| {
                    ApiError::Internal("Kernel did not return a channel id".to_string())
                })?,
            agentos_bus::KernelResponse::Error { message } => {
                return Err(if message.starts_with("Unknown agent") {
                    ApiError::BadRequest(message)
                } else {
                    ApiError::Conflict(message)
                })
            }
            _ => {
                return Err(ApiError::Internal(
                    "Unexpected kernel response connecting channel".into(),
                ))
            }
        };
        self.get_channel(&channel_id).await
    }

    async fn update_channel(
        &self,
        id: &str,
        req: UpdateChannelRequest,
    ) -> Result<ApiChannelSummary, ApiError> {
        // The kernel owns the vault write: it happens after the old adapter is
        // torn down (so a Telegram webhook is deleted with the token that
        // registered it) and only once the channel is known to exist.
        let credential_key = match req.credential_key.map(|k| k.trim().to_string()) {
            Some(k) if k.is_empty() => None,
            Some(k) => {
                validate_channel_credential_key(&k)?;
                Some(k)
            }
            None => None,
        };
        match self
            .cmd_update_channel(
                id.to_string(),
                req.display_name,
                req.external_id,
                credential_key,
                req.credential,
                req.reply_topic,
                req.server_url,
                req.webhook_url,
                req.active_agent_name,
            )
            .await
        {
            agentos_bus::KernelResponse::Success { .. } => self.get_channel(id).await,
            agentos_bus::KernelResponse::Error { message } => Err(channel_err(message)),
            _ => Err(ApiError::Internal(
                "Unexpected kernel response updating channel".into(),
            )),
        }
    }

    async fn test_channel(&self, id: &str) -> Result<(), ApiError> {
        match self.cmd_test_channel(id.to_string()).await {
            agentos_bus::KernelResponse::Success { .. } => Ok(()),
            agentos_bus::KernelResponse::Error { message } => Err(channel_err(message)),
            _ => Err(ApiError::Internal(
                "Unexpected kernel response testing channel".into(),
            )),
        }
    }

    async fn set_channel_agent(
        &self,
        id: &str,
        agent_name: Option<String>,
    ) -> Result<(), ApiError> {
        match self
            .cmd_set_channel_active_agent(id.to_string(), agent_name)
            .await
        {
            agentos_bus::KernelResponse::Success { .. } => Ok(()),
            agentos_bus::KernelResponse::Error { message } => Err(channel_err(message)),
            _ => Err(ApiError::Internal(
                "Unexpected kernel response setting channel agent".into(),
            )),
        }
    }

    async fn list_pairings(&self) -> Result<ApiPairings, ApiError> {
        match self.cmd_list_pairings().await {
            agentos_bus::KernelResponse::PairingList { approved, pending } => Ok(ApiPairings {
                approved: approved
                    .into_iter()
                    .map(|p| ApiPairingEntry {
                        channel_id: p.channel_id,
                        sender_id: p.sender_id,
                        approved_at: p.approved_at,
                        label: p.label,
                    })
                    .collect(),
                pending: pending
                    .into_iter()
                    .map(|p| ApiPendingPairing {
                        channel_id: p.channel_id,
                        sender_id: p.sender_id,
                        expires_at: p.expires_at,
                    })
                    .collect(),
            }),
            agentos_bus::KernelResponse::Error { message } => Err(ApiError::Internal(message)),
            _ => Err(ApiError::Internal(
                "Unexpected kernel response listing pairings".into(),
            )),
        }
    }

    async fn approve_pairing(&self, code: &str) -> Result<ApiPairingEntry, ApiError> {
        match self.cmd_approve_pairing(code.to_string()).await {
            agentos_bus::KernelResponse::Success { data } => {
                let d = data.unwrap_or_else(|| serde_json::json!({}));
                let field = |k: &str| {
                    d.get(k)
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string()
                };
                Ok(ApiPairingEntry {
                    channel_id: field("channel_id"),
                    sender_id: field("sender_id"),
                    approved_at: chrono::Utc::now().to_rfc3339(),
                    label: None,
                })
            }
            agentos_bus::KernelResponse::Error { message } => Err(ApiError::NotFound(message)),
            _ => Err(ApiError::Internal(
                "Unexpected kernel response approving pairing".into(),
            )),
        }
    }

    async fn approve_pending_pairing(
        &self,
        channel_id: &str,
        sender_id: &str,
    ) -> Result<ApiPairingEntry, ApiError> {
        match self
            .cmd_approve_pending_pairing(channel_id.to_string(), sender_id.to_string())
            .await
        {
            agentos_bus::KernelResponse::Success { data } => {
                let d = data.unwrap_or_else(|| serde_json::json!({}));
                let field = |k: &str| {
                    d.get(k)
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string()
                };
                Ok(ApiPairingEntry {
                    channel_id: field("channel_id"),
                    sender_id: field("sender_id"),
                    approved_at: chrono::Utc::now().to_rfc3339(),
                    label: None,
                })
            }
            agentos_bus::KernelResponse::Error { message } => Err(ApiError::NotFound(message)),
            _ => Err(ApiError::Internal(
                "Unexpected kernel response approving pairing".into(),
            )),
        }
    }

    async fn revoke_pairing(&self, channel_id: &str, sender_id: &str) -> Result<(), ApiError> {
        match self
            .cmd_revoke_pairing(channel_id.to_string(), sender_id.to_string())
            .await
        {
            agentos_bus::KernelResponse::Success { .. } => Ok(()),
            agentos_bus::KernelResponse::Error { message } => Err(ApiError::NotFound(message)),
            _ => Err(ApiError::Internal(
                "Unexpected kernel response revoking pairing".into(),
            )),
        }
    }

    async fn install_plugin(&self, manifest_toml: &str) -> Result<ApiPluginSummary, ApiError> {
        let manifest: agentos_types::PluginManifest = toml::from_str(manifest_toml)
            .map_err(|e| ApiError::BadRequest(format!("Invalid plugin manifest: {e}")))?;
        let id = manifest.id.clone();
        if !agentos_kernel::plugin_registry::valid_plugin_id(&id) {
            return Err(ApiError::BadRequest(format!(
                "Invalid plugin id '{id}': use letters, digits, '-' or '_' (max 64)"
            )));
        }
        if self.plugin_registry.status(&id).await.is_some() {
            return Err(ApiError::Conflict(format!(
                "Plugin '{id}' already exists — remove it first"
            )));
        }

        let dir = self.user_plugins_dir().join(&id);
        let path = dir.join("plugin.toml");
        let body = manifest_toml.to_string();
        let write_dir = dir.clone();
        let write_path = path.clone();
        tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&write_dir)?;
            std::fs::write(&write_path, body)
        })
        .await
        .map_err(|e| ApiError::Internal(format!("write task failed: {e}")))?
        .map_err(|e| ApiError::Internal(format!("Failed to write plugin manifest: {e}")))?;

        self.plugin_registry
            .discover(&[self.user_plugins_dir()])
            .await;
        let entry = self
            .plugin_registry
            .list()
            .await
            .into_iter()
            .find(|p| p.manifest.id == id)
            .ok_or_else(|| {
                // Discovery rejected it (bad id, duplicate). Don't leave the file behind.
                ApiError::BadRequest(format!(
                    "Manifest for '{id}' was rejected by plugin discovery"
                ))
            });
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(&dir)).await;
                return Err(e);
            }
        };

        let _ = self.audit.append(AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: agentos_types::TraceID::new(),
            event_type: AuditEventType::PluginInstalled,
            agent_id: None,
            task_id: None,
            tool_id: None,
            details: serde_json::json!({
                "plugin_id": id,
                "path": path.display().to_string(),
                "trust_tier": format!("{:?}", entry.manifest.trust_tier),
                "status": format!("{:?}", entry.status),
            }),
            severity: AuditSeverity::Info,
            reversible: true,
            rollback_ref: None,
        });
        Ok(plugin_to_summary(entry, &self.user_plugins_dir()))
    }

    async fn update_plugin(
        &self,
        id: &str,
        manifest_toml: &str,
    ) -> Result<ApiPluginSummary, ApiError> {
        let manifest: agentos_types::PluginManifest = toml::from_str(manifest_toml)
            .map_err(|e| ApiError::BadRequest(format!("Invalid plugin manifest: {e}")))?;
        if manifest.id != id {
            return Err(ApiError::BadRequest(format!(
                "Manifest id '{}' does not match plugin '{id}' — remove it and add the new one instead",
                manifest.id
            )));
        }
        let entry = self
            .plugin_registry
            .list()
            .await
            .into_iter()
            .find(|p| p.manifest.id == id)
            .ok_or_else(|| ApiError::NotFound(format!("Plugin '{id}' not found")))?;
        let user_dir = self.user_plugins_dir();
        if !is_user_plugin(&entry, &user_dir) {
            return Err(ApiError::Conflict(format!(
                "Plugin '{id}' ships with AgentOS — it cannot be edited"
            )));
        }
        // Discovery keeps the first manifest it sees for an id, so the old entry
        // has to leave the registry before the new file is scanned.
        let was_active = matches!(
            entry.status,
            agentos_kernel::plugin_registry::PluginStatus::Active
        );
        if was_active {
            self.plugin_registry
                .deactivate(id)
                .await
                .map_err(|e| ApiError::Conflict(e.to_string()))?;
        }
        let path = self
            .plugin_registry
            .remove(id)
            .await
            .map_err(|e| ApiError::Conflict(e.to_string()))?;

        // Keep the bytes that were working: discovery can reject the new
        // manifest for reasons `toml::from_str` accepts (an id that fails
        // `valid_plugin_id`, say), and by then the entry is out of the registry
        // and the old text is overwritten — leaving the plugin unreachable by
        // PUT, DELETE *and* rediscovery, with nothing left to restore it from.
        let previous = {
            let p = path.clone();
            tokio::task::spawn_blocking(move || std::fs::read(&p))
                .await
                .ok()
                .and_then(|r| r.ok())
        };
        let body = manifest_toml.to_string();
        let write_path = path.clone();
        tokio::task::spawn_blocking(move || std::fs::write(&write_path, body))
            .await
            .map_err(|e| ApiError::Internal(format!("write task failed: {e}")))?
            .map_err(|e| ApiError::Internal(format!("Failed to write plugin manifest: {e}")))?;

        self.plugin_registry
            .discover(std::slice::from_ref(&user_dir))
            .await;
        if self.plugin_registry.status(id).await.is_none() {
            let mut restored = false;
            if let Some(bytes) = previous {
                let p = path.clone();
                if tokio::task::spawn_blocking(move || std::fs::write(&p, bytes))
                    .await
                    .is_ok_and(|r| r.is_ok())
                {
                    self.plugin_registry
                        .discover(std::slice::from_ref(&user_dir))
                        .await;
                    restored = self.plugin_registry.status(id).await.is_some();
                }
            }
            return Err(ApiError::BadRequest(format!(
                "Manifest for '{id}' was rejected by plugin discovery{}",
                if restored {
                    " — the previous manifest was restored"
                } else {
                    " and the previous manifest could not be restored"
                }
            )));
        }
        // Restore the prior state: an edit should not silently disable a
        // running plugin. A now-blocked manifest simply fails to activate.
        if was_active {
            if let Err(e) = self.plugin_registry.activate(id).await {
                tracing::warn!(plugin_id = %id, error = %e, "Edited plugin could not be reactivated");
            }
        }
        let entry = self
            .plugin_registry
            .list()
            .await
            .into_iter()
            .find(|p| p.manifest.id == id)
            .ok_or_else(|| ApiError::Internal(format!("Plugin '{id}' vanished after the edit")))?;
        Ok(plugin_to_summary(entry, &user_dir))
    }

    async fn remove_plugin(&self, id: &str) -> Result<(), ApiError> {
        let entry = self
            .plugin_registry
            .list()
            .await
            .into_iter()
            .find(|p| p.manifest.id == id)
            .ok_or_else(|| ApiError::NotFound(format!("Plugin '{id}' not found")))?;
        if !is_user_plugin(&entry, &self.user_plugins_dir()) {
            return Err(ApiError::Conflict(format!(
                "Plugin '{id}' ships with AgentOS — disable it instead of removing it"
            )));
        }
        // Deactivate first so its tools are unregistered; `remove` refuses while active.
        if matches!(
            entry.status,
            agentos_kernel::plugin_registry::PluginStatus::Active
        ) {
            self.plugin_registry
                .deactivate(id)
                .await
                .map_err(|e| ApiError::Conflict(e.to_string()))?;
        }
        // Discovery also accepts a bare `plugins/user/plugin.toml`, whose parent
        // is the user directory itself — deleting that would take every other
        // user plugin with it. Delete the file alone in that case.
        let user_dir = self.user_plugins_dir();
        let manifest_path = self
            .plugin_registry
            .remove(id)
            .await
            .map_err(|e| ApiError::Conflict(e.to_string()))?;
        let target = match manifest_path.parent() {
            Some(parent) if parent != user_dir => parent.to_path_buf(),
            _ => manifest_path.clone(),
        };
        let removed = target.clone();
        tokio::task::spawn_blocking(move || {
            if removed.is_dir() {
                std::fs::remove_dir_all(&removed)
            } else {
                std::fs::remove_file(&removed)
            }
        })
        .await
        .map_err(|e| ApiError::Internal(format!("remove task failed: {e}")))?
        .map_err(|e| ApiError::Internal(format!("Failed to delete plugin files: {e}")))?;
        let dir = target;

        let _ = self.audit.append(AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: agentos_types::TraceID::new(),
            event_type: AuditEventType::PluginRemoved,
            agent_id: None,
            task_id: None,
            tool_id: None,
            details: serde_json::json!({ "plugin_id": id, "path": dir.display().to_string() }),
            severity: AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        });
        Ok(())
    }

    async fn add_connector(&self, manifest_toml: &str) -> Result<ApiConnectorDetail, ApiError> {
        let manifest = self
            .install_connector_manifest(manifest_toml, None)
            .await
            .map_err(|m| {
                if m.starts_with("Invalid") {
                    ApiError::BadRequest(m)
                } else if m.contains("already registered") {
                    ApiError::Conflict(m)
                } else {
                    ApiError::Internal(m)
                }
            })?;
        self.get_connector(&manifest.connector.id).await
    }

    async fn update_connector(
        &self,
        id: &str,
        manifest_toml: &str,
    ) -> Result<ApiConnectorDetail, ApiError> {
        let manifest = self
            .install_connector_manifest(manifest_toml, Some(id))
            .await
            .map_err(|m| {
                if m.starts_with("Invalid") || m.contains("does not match") {
                    ApiError::BadRequest(m)
                } else if m.contains("not registered") {
                    ApiError::NotFound(m)
                } else {
                    ApiError::Internal(m)
                }
            })?;
        self.get_connector(&manifest.connector.id).await
    }

    async fn remove_connector(&self, id: &str) -> Result<(), ApiError> {
        self.remove_connector_manifest(id).await.map_err(|m| {
            if m.contains("not found") {
                ApiError::NotFound(m)
            } else if m.starts_with("Invalid") {
                ApiError::BadRequest(m)
            } else {
                ApiError::Internal(m)
            }
        })
    }

    async fn start_connector_oauth(
        &self,
        id: &str,
        redirect_uri: &str,
    ) -> Result<String, ApiError> {
        use agentos_kernel::oauth_flow::OAuthFlowError;
        self.oauth_begin(id, redirect_uri)
            .await
            .map_err(|e| match e {
                OAuthFlowError::NoProvider(_) => ApiError::NotFound(e.to_string()),
                OAuthFlowError::Config(_) => ApiError::Conflict(e.to_string()),
                other => ApiError::Internal(other.to_string()),
            })
    }

    async fn complete_connector_oauth(
        &self,
        id: &str,
        code: &str,
        state: &str,
    ) -> Result<(), ApiError> {
        use agentos_kernel::oauth_flow::OAuthFlowError;
        self.oauth_complete(id, code, state)
            .await
            .map_err(|e| match e {
                OAuthFlowError::NoProvider(_) => ApiError::NotFound(e.to_string()),
                OAuthFlowError::InvalidState => ApiError::Forbidden(e.to_string()),
                OAuthFlowError::Exchange(m) => ApiError::ServiceUnavailable(m),
                other => ApiError::Internal(other.to_string()),
            })
    }

    async fn store_connector_credential(
        &self,
        id: &str,
        req: StoreCredentialRequest,
    ) -> Result<(), ApiError> {
        if req.access_token.trim().is_empty() {
            return Err(ApiError::BadRequest("`access_token` is required".into()));
        }
        // The vault refuses to store a credential whose token endpoint is not
        // HTTPS (SSRF guard), so validate here where the message can name the
        // field instead of surfacing a vault error from three layers down.
        let token_endpoint = req.token_endpoint.unwrap_or_default();
        if !token_endpoint.starts_with("https://") {
            return Err(ApiError::BadRequest(
                "`token_endpoint` is required and must be an https:// URL".into(),
            ));
        }
        let provider = req.provider.unwrap_or_else(|| id.to_string());
        let resp = self
            .cmd_mcp_oauth_store(
                id.to_string(),
                provider,
                zeroize::Zeroizing::new(req.access_token.to_string()),
                req.refresh_token
                    .map(|t| zeroize::Zeroizing::new(t.to_string())),
                token_endpoint,
                req.client_id.unwrap_or_default(),
                req.client_secret
                    .map(|s| zeroize::Zeroizing::new(s.to_string())),
                req.scopes,
                req.expires_in_secs,
            )
            .await;
        match resp {
            agentos_bus::KernelResponse::McpOAuthStored { .. } => Ok(()),
            agentos_bus::KernelResponse::Error { message } => Err(ApiError::Internal(message)),
            _ => Err(ApiError::Internal(
                "Unexpected kernel response storing credential".into(),
            )),
        }
    }

    async fn list_event_subscriptions(&self) -> Result<Vec<ApiEventSubscription>, ApiError> {
        Ok(self
            .event_bus
            .list_subscriptions()
            .await
            .into_iter()
            .map(subscription_to_api)
            .collect())
    }

    async fn create_event_subscription(
        &self,
        req: CreateSubscriptionRequest,
    ) -> Result<ApiEventSubscription, ApiError> {
        use agentos_kernel::event_bus::{parse_event_type_filter, parse_subscription_priority};

        let agent_id = {
            let registry = self.agent_registry.read().await;
            registry
                .get_by_name(&req.agent_name)
                .map(|a| a.id)
                .ok_or_else(|| {
                    ApiError::BadRequest(format!("Agent '{}' not found", req.agent_name))
                })?
        };

        let event_type_filter = parse_event_type_filter(&req.event_filter).ok_or_else(|| {
            ApiError::BadRequest(format!(
                "Invalid event filter '{}'. Use 'all', 'category:<name>', or an exact event type",
                req.event_filter
            ))
        })?;

        // Fail closed: unspecified throttle gets the bounded default, matching
        // the CLI and agent-tool subscribe paths. Explicit "none" opts out.
        let throttle = match req.throttle.as_deref() {
            None | Some("") => agentos_types::ThrottlePolicy::MaxCountPerDuration(
                30,
                std::time::Duration::from_secs(60),
            ),
            Some("none") => agentos_types::ThrottlePolicy::None,
            Some(s) => parse_throttle_str(s).ok_or_else(|| {
                ApiError::BadRequest(format!(
                    "Invalid throttle '{s}'. Use 'none', 'once_per:<dur>', or 'max:<count>/<dur>'"
                ))
            })?,
        };

        let priority = parse_subscription_priority(req.priority.as_deref()).ok_or_else(|| {
            ApiError::BadRequest(format!(
                "Invalid priority '{}'. Use 'critical', 'high', 'normal', or 'low'",
                req.priority.as_deref().unwrap_or_default()
            ))
        })?;

        let payload_filter = req.payload_filter.and_then(|raw| {
            let t = raw.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        });
        if let Some(f) = payload_filter.as_deref() {
            agentos_kernel::event_bus::validate_filter(f).map_err(ApiError::BadRequest)?;
        }

        let sub = agentos_types::EventSubscription {
            id: agentos_types::SubscriptionID::new(),
            agent_id,
            event_type_filter,
            filter: payload_filter,
            priority,
            throttle,
            enabled: true,
            created_at: chrono::Utc::now(),
        };

        let sub_id = self.event_bus.subscribe(sub.clone()).await;
        let mut api = subscription_to_api(sub);
        api.id = sub_id.to_string();
        Ok(api)
    }

    async fn delete_event_subscription(&self, id: &str) -> Result<(), ApiError> {
        let sid: agentos_types::SubscriptionID = id
            .parse()
            .map_err(|_| ApiError::BadRequest(format!("Invalid subscription ID: {id}")))?;
        if self.event_bus.unsubscribe(&sid).await {
            Ok(())
        } else {
            Err(ApiError::NotFound(format!("Subscription '{id}' not found")))
        }
    }

    async fn enable_event_subscription(&self, id: &str) -> Result<(), ApiError> {
        let sid: agentos_types::SubscriptionID = id
            .parse()
            .map_err(|_| ApiError::BadRequest(format!("Invalid subscription ID: {id}")))?;
        if self.event_bus.enable_subscription(&sid).await {
            Ok(())
        } else {
            Err(ApiError::NotFound(format!("Subscription '{id}' not found")))
        }
    }

    async fn disable_event_subscription(&self, id: &str) -> Result<(), ApiError> {
        let sid: agentos_types::SubscriptionID = id
            .parse()
            .map_err(|_| ApiError::BadRequest(format!("Invalid subscription ID: {id}")))?;
        if Kernel::disable_event_subscription(self, &sid).await {
            Ok(())
        } else {
            Err(ApiError::NotFound(format!("Subscription '{id}' not found")))
        }
    }

    async fn emit_event(&self, req: EmitEventRequest) -> Result<(), ApiError> {
        let event_type =
            agentos_kernel::event_bus::parse_event_type(&req.event_type).ok_or_else(|| {
                ApiError::BadRequest(format!("Unknown event type '{}'", req.event_type))
            })?;
        let severity = match req.severity.as_deref().map(|s| s.to_lowercase()) {
            Some(ref s) if s == "warning" || s == "warn" => agentos_types::EventSeverity::Warning,
            Some(ref s) if s == "critical" => agentos_types::EventSeverity::Critical,
            _ => agentos_types::EventSeverity::Info,
        };
        Kernel::emit_event(
            self,
            event_type,
            agentos_types::EventSource::ExternalBridge,
            severity,
            req.payload,
            0,
        )
        .await;
        Ok(())
    }

    async fn list_webhooks(&self) -> Result<Vec<ApiWebhookEndpoint>, ApiError> {
        Ok(self
            .webhook_registry
            .list_endpoints(None)
            .await
            .into_iter()
            .map(webhook_to_api)
            .collect())
    }

    async fn create_webhook(
        &self,
        req: CreateWebhookRequest,
    ) -> Result<WebhookSecretResponse, ApiError> {
        let provider = parse_webhook_provider(&req.provider).ok_or_else(|| {
            ApiError::BadRequest(
                "Invalid provider. Allowed: github, stripe, slack, pagerduty, generic".into(),
            )
        })?;
        let agent_id = {
            let reg = self.agent_registry.read().await;
            reg.get_by_name(req.agent_name.trim())
                .map(|a| a.id)
                .ok_or_else(|| {
                    ApiError::BadRequest(format!("Unknown agent '{}'", req.agent_name))
                })?
        };
        let (meta, secret) = self
            .webhook_registry
            .create_endpoint(agent_id, provider, req.debounce_seconds.unwrap_or(0))
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        Ok(WebhookSecretResponse {
            id: meta.id.to_string(),
            inbound_url: format!("/api/v1/webhooks/incoming/{}", meta.id),
            secret,
        })
    }

    async fn rotate_webhook(&self, id: &str) -> Result<WebhookSecretResponse, ApiError> {
        let eid: agentos_types::WebhookEndpointID = id
            .parse()
            .map_err(|_| ApiError::BadRequest(format!("Invalid endpoint ID: {id}")))?;
        if self.webhook_registry.get_endpoint(&eid).await.is_none() {
            return Err(ApiError::NotFound(format!(
                "Webhook endpoint '{id}' not found"
            )));
        }
        let secret = self
            .webhook_registry
            .rotate_secret(&eid)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        Ok(WebhookSecretResponse {
            id: eid.to_string(),
            inbound_url: format!("/api/v1/webhooks/incoming/{}", eid),
            secret,
        })
    }

    async fn delete_webhook(&self, id: &str) -> Result<(), ApiError> {
        let eid: agentos_types::WebhookEndpointID = id
            .parse()
            .map_err(|_| ApiError::BadRequest(format!("Invalid endpoint ID: {id}")))?;
        self.webhook_registry
            .delete_endpoint(&eid)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))
    }

    async fn get_agent_identity(&self, name: &str) -> Result<ApiAgentIdentity, ApiError> {
        let registry = self.agent_registry.read().await;
        let profile = registry
            .get_by_name(name)
            .ok_or_else(|| ApiError::NotFound(format!("Agent '{name}' not found")))?;
        let public_key_hex = profile.public_key_hex.clone();
        let fingerprint = public_key_hex
            .as_ref()
            .map(|pk| pk.chars().take(16).collect::<String>());
        Ok(ApiAgentIdentity {
            id: profile.id.to_string(),
            name: profile.name.clone(),
            public_key_hex,
            fingerprint,
            status: format!("{:?}", profile.status),
            created_at: profile.created_at,
            last_active: profile.last_active,
        })
    }

    // ── Files (Phase 06) ──────────────────────────────────────────────────

    async fn upload_file(
        &self,
        owner: &str,
        original_name: &str,
        mime: &str,
        scope: &str,
        tags: &[String],
        bytes: Vec<u8>,
    ) -> Result<ApiFileMeta, ApiError> {
        use agentos_llm::media::{is_supported_image_mime, MAX_INLINE_IMAGE_BYTES};

        // Image-MIME 5 MiB cap (mirrors the web upload path).
        let mime_lc = mime.to_ascii_lowercase();
        if mime_lc.starts_with("image/")
            && is_supported_image_mime(&mime_lc)
            && bytes.len() > MAX_INLINE_IMAGE_BYTES
        {
            return Err(ApiError::BadRequest(
                "Image uploads are limited to 5 MiB".into(),
            ));
        }

        let store = self.file_store.clone();
        let file_id = uuid::Uuid::new_v4().to_string();
        let safe_part = agentos_kernel::file_store::sanitize_storage_name(original_name);
        let stored_name = format!("{file_id}_{safe_part}");
        let disk_path = store.uploads_dir.join(&stored_name);
        let disk_path_str = disk_path.to_string_lossy().to_string();
        let size = bytes.len() as u64;

        let fid = file_id.clone();
        let original = original_name.to_string();
        let mime_owned = mime.to_string();
        let owner_owned = owner.to_string();
        let scope_owned = scope.to_string();
        let tags_csv = tags.join(",");

        tokio::task::spawn_blocking(move || -> Result<(), String> {
            std::fs::write(&disk_path, &bytes).map_err(|e| format!("write to disk: {e}"))?;
            if let Err(e) = store.register_file(
                &fid,
                &original,
                &mime_owned,
                size,
                &disk_path_str,
                &tags_csv,
                &owner_owned,
                &scope_owned,
            ) {
                let _ = std::fs::remove_file(&disk_path_str);
                return Err(format!("register in db: {e}"));
            }
            Ok(())
        })
        .await
        .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
        .map_err(ApiError::Internal)?;

        // Read back the registered row so the meta matches the DB exactly.
        self.get_file(owner, &file_id).await
    }

    async fn list_files(
        &self,
        owner: &str,
        scope: Option<&str>,
        tag: Option<&str>,
        q: Option<&str>,
    ) -> Result<Vec<ApiFileMeta>, ApiError> {
        let store = self.file_store.clone();
        let owner_owned = owner.to_string();
        let scope_owned = scope.map(|s| s.to_string());
        let q_owned = q.map(|s| s.to_string());
        // When searching within a session scope, pass the session id to search_files
        // so session-scoped files are searchable (otherwise it restricts to global
        // and the post-filter below would drop every session-scoped hit).
        let search_session = scope_owned
            .as_deref()
            .and_then(|s| s.strip_prefix("session:"))
            .map(|s| s.to_string());

        let files = tokio::task::spawn_blocking(move || match q_owned {
            Some(query) if !query.trim().is_empty() => {
                store.search_files(&query, &owner_owned, search_session.as_deref(), 200)
            }
            _ => store.list_files(&owner_owned, scope_owned.as_deref()),
        })
        .await
        .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
        .map_err(|e| ApiError::Internal(e.to_string()))?;

        let tag = tag.map(|t| t.to_string());
        let scope_post = scope.map(|s| s.to_string());
        Ok(files
            .into_iter()
            .map(file_meta_from)
            .filter(|m| scope_post.as_ref().is_none_or(|s| &m.scope == s))
            .filter(|m| tag.as_ref().is_none_or(|t| m.tags.iter().any(|x| x == t)))
            .collect())
    }

    async fn get_file(&self, owner: &str, id: &str) -> Result<ApiFileMeta, ApiError> {
        let store = self.file_store.clone();
        let owner_owned = owner.to_string();
        let id_owned = id.to_string();
        let rec = tokio::task::spawn_blocking(move || store.get_file(&id_owned, &owner_owned))
            .await
            .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
            .map_err(|e| ApiError::Internal(e.to_string()))?
            .ok_or_else(|| ApiError::NotFound(format!("File {id} not found")))?;
        Ok(file_meta_from(rec))
    }

    async fn download_file(
        &self,
        owner: &str,
        id: &str,
    ) -> Result<(String, String, Vec<u8>), ApiError> {
        let store = self.file_store.clone();
        let owner_owned = owner.to_string();
        let id_owned = id.to_string();
        let rec = tokio::task::spawn_blocking(move || store.get_file(&id_owned, &owner_owned))
            .await
            .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
            .map_err(|e| ApiError::Internal(e.to_string()))?
            .ok_or_else(|| ApiError::NotFound(format!("File {id} not found")))?;

        let uploads_dir = self.file_store.uploads_dir.clone();
        let path = rec.path.clone();
        let original_name = rec.original_name.clone();
        let safe_mime = safe_download_mime(&rec.mime);

        let bytes = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
            let disk_path = std::path::PathBuf::from(&path);
            let canonical = disk_path
                .canonicalize()
                .map_err(|_| "file not found on disk".to_string())?;
            let canonical_uploads = uploads_dir
                .canonicalize()
                .map_err(|e| format!("canonicalize uploads_dir: {e}"))?;
            if !canonical.starts_with(&canonical_uploads) {
                return Err("path escapes uploads directory".into());
            }
            std::fs::read(&canonical).map_err(|_| "file not found on disk".to_string())
        })
        .await
        .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
        .map_err(ApiError::NotFound)?;

        Ok((safe_mime, original_name, bytes))
    }

    async fn delete_file(&self, owner: &str, id: &str) -> Result<(), ApiError> {
        let store = self.file_store.clone();
        let owner_owned = owner.to_string();
        let id_owned = id.to_string();

        let path = tokio::task::spawn_blocking(move || store.delete_file(&id_owned, &owner_owned))
            .await
            .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
            .map_err(|e| ApiError::Internal(e.to_string()))?
            .ok_or_else(|| ApiError::NotFound(format!("File {id} not found")))?;

        // Same sweep the TTL prunes use: containment-checked unlink of the bytes,
        // plus any copies agents materialized under their own homes. Those are
        // hard links, so unlinking the upload alone would leave the agent holding
        // a fully readable copy of a file the user just deleted.
        let store = self.file_store.clone();
        let id_for_sweep = id.to_string();
        tokio::task::spawn_blocking(move || {
            agentos_kernel::file_bindings::unlink_pruned(
                &store,
                vec![(id_for_sweep, path)],
                "delete_file",
            )
        })
        .await
        .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?;
        Ok(())
    }

    // ── Scratchpad (Phase 06) ─────────────────────────────────────────────

    async fn get_scratchpad(&self, agent_id: &str) -> Result<Vec<ApiPageSummary>, ApiError> {
        let pages = self
            .scratchpad_store
            .list_pages(agent_id)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        Ok(pages.into_iter().map(scratch_summary_to_api).collect())
    }

    async fn get_scratchpad_page(
        &self,
        agent_id: &str,
        title: &str,
    ) -> Result<ApiScratchPage, ApiError> {
        let page = self
            .scratchpad_store
            .read_page(agent_id, title)
            .await
            .map_err(|e| match e {
                agentos_scratch::ScratchError::PageNotFound { .. } => {
                    ApiError::NotFound(format!("Page '{title}' not found"))
                }
                other => ApiError::Internal(other.to_string()),
            })?;
        let links = self
            .scratchpad_store
            .get_all_links(agent_id, title)
            .await
            .map(|l| l.backlinks)
            .unwrap_or_default();
        Ok(scratch_page_to_api(page, links))
    }

    async fn save_scratchpad_page(
        &self,
        agent_id: &str,
        title: &str,
        content: String,
        tags: Vec<String>,
    ) -> Result<ApiScratchPage, ApiError> {
        let page = self
            .scratchpad_store
            .write_page(agent_id, title, &content, &tags)
            .await
            .map_err(|e| match e {
                agentos_scratch::ScratchError::ContentTooLarge { .. }
                | agentos_scratch::ScratchError::TitleTooLong { .. }
                | agentos_scratch::ScratchError::EmptyTitle
                | agentos_scratch::ScratchError::InvalidTitle
                | agentos_scratch::ScratchError::TooManyPages { .. } => {
                    ApiError::BadRequest(e.to_string())
                }
                other => ApiError::Internal(other.to_string()),
            })?;
        let links = self
            .scratchpad_store
            .get_all_links(agent_id, title)
            .await
            .map(|l| l.backlinks)
            .unwrap_or_default();
        Ok(scratch_page_to_api(page, links))
    }

    async fn delete_scratchpad_page(&self, agent_id: &str, title: &str) -> Result<(), ApiError> {
        self.scratchpad_store
            .delete_page(agent_id, title)
            .await
            .map_err(|e| match e {
                agentos_scratch::ScratchError::PageNotFound { .. } => {
                    ApiError::NotFound(format!("Page '{title}' not found"))
                }
                other => ApiError::Internal(other.to_string()),
            })
    }

    // ── Chat sessions (Phase 02 Conversational) ──────────────────────────────

    async fn list_chat_sessions(&self) -> Result<Vec<ApiChatSessionSummary>, ApiError> {
        let store = self.chat_store.clone();
        let sessions = tokio::task::spawn_blocking(move || store.list_sessions())
            .await
            .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        Ok(sessions
            .into_iter()
            .map(|s| ApiChatSessionSummary {
                id: s.id,
                agent_name: s.agent_name,
                title: s.title,
                preview: s.last_preview,
                message_count: s.message_count.max(0) as u64,
                updated_at: s.updated_at,
            })
            .collect())
    }

    async fn create_chat_session(
        &self,
        req: CreateChatSessionRequest,
    ) -> Result<ApiChatSessionDetail, ApiError> {
        let store = self.chat_store.clone();
        let agent_name = req.agent_name.clone();
        // No first message => an empty session. A client that opens the chat
        // lazily (panel: create on first send, then stream) has nothing to seed
        // it with, and the send path persists the user turn itself — seeding a
        // blank placeholder row here would put an empty user turn at the head of
        // the transcript and replay it to the LLM as history.
        let first = req
            .first_message
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let id = tokio::task::spawn_blocking(move || match first {
            Some(first) => store.create_session_with_first_message(&agent_name, &first, None),
            None => store.create_session(&agent_name),
        })
        .await
        .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
        .map_err(|e| ApiError::Internal(e.to_string()))?;

        if let Some(title) = req
            .title
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let store = self.chat_store.clone();
            let id_c = id.clone();
            let title_c = title.to_string();
            let _ =
                tokio::task::spawn_blocking(move || store.rename_session(&id_c, Some(&title_c)))
                    .await;
        }

        self.get_chat_session(&id).await
    }

    async fn get_chat_session(&self, id: &str) -> Result<ApiChatSessionDetail, ApiError> {
        let store = self.chat_store.clone();
        let id_owned = id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id_owned))
            .await
            .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
            .map_err(|e| ApiError::Internal(e.to_string()))?
            .ok_or_else(|| ApiError::NotFound(format!("Chat session {id} not found")))?;

        let store = self.chat_store.clone();
        let id_owned = id.to_string();
        let msgs = tokio::task::spawn_blocking(move || store.get_messages(&id_owned))
            .await
            .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
            .map_err(|e| ApiError::Internal(e.to_string()))?;

        Ok(ApiChatSessionDetail {
            id: session.id,
            agent_name: session.agent_name,
            title: session.title,
            messages: msgs.into_iter().map(api_chat_message_from).collect(),
        })
    }

    async fn rename_chat_session(&self, id: &str, title: Option<String>) -> Result<(), ApiError> {
        let store = self.chat_store.clone();
        let id_owned = id.to_string();
        let id_err = id.to_string();
        tokio::task::spawn_blocking(move || store.rename_session(&id_owned, title.as_deref()))
            .await
            .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    ApiError::NotFound(format!("Chat session {id_err} not found"))
                }
                other => ApiError::Internal(other.to_string()),
            })
    }

    async fn delete_chat_session(&self, id: &str) -> Result<(), ApiError> {
        let store = self.chat_store.clone();
        let id_owned = id.to_string();
        let id_err = id.to_string();
        tokio::task::spawn_blocking(move || store.delete_session(&id_owned))
            .await
            .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    ApiError::NotFound(format!("Chat session {id_err} not found"))
                }
                other => ApiError::Internal(other.to_string()),
            })?;
        self.forget_chat_session_dedup(id).await;
        Ok(())
    }

    async fn fork_chat_session(&self, id: &str, title: Option<String>) -> Result<String, ApiError> {
        let store = self.chat_store.clone();
        let id_owned = id.to_string();
        let id_err = id.to_string();
        tokio::task::spawn_blocking(move || store.fork_session(&id_owned, title.as_deref()))
            .await
            .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    ApiError::NotFound(format!("Chat session {id_err} not found"))
                }
                other => ApiError::Internal(other.to_string()),
            })
    }

    async fn export_chat_session(
        &self,
        id: &str,
        format: &str,
    ) -> Result<(Vec<u8>, String, String), ApiError> {
        let detail = self.get_chat_session(id).await?;
        let short = id.chars().take(8).collect::<String>();

        match format {
            "markdown" | "md" => {
                let mut out = String::new();
                let title = detail
                    .title
                    .clone()
                    .unwrap_or_else(|| format!("Chat with {}", detail.agent_name));
                out.push_str(&format!("# {title}\n\n"));
                for msg in &detail.messages {
                    match msg.role.as_str() {
                        "user" => out.push_str("## You\n\n"),
                        "assistant" => out.push_str(&format!("## {}\n\n", detail.agent_name)),
                        "tool" => {
                            let tool_name =
                                msg.tool_name.clone().unwrap_or_else(|| "tool".to_string());
                            out.push_str(&format!("### Tool: {tool_name}\n\n"));
                            if let Some(payload) = &msg.tool_payload_json {
                                out.push_str("#### Input\n\n```json\n");
                                out.push_str(payload);
                                out.push_str("\n```\n\n");
                            }
                            if let Some(result) = &msg.tool_result_json {
                                out.push_str("#### Result\n\n```json\n");
                                out.push_str(result);
                                out.push_str("\n```\n\n");
                            }
                        }
                        _ => out.push_str("## Message\n\n"),
                    }
                    if msg.role != "tool" {
                        out.push_str(&msg.content);
                        out.push_str("\n\n");
                    }
                }
                Ok((
                    out.into_bytes(),
                    "text/markdown; charset=utf-8".to_string(),
                    format!("chat-{short}.md"),
                ))
            }
            _ => {
                let json = serde_json::to_vec_pretty(&detail)
                    .map_err(|e| ApiError::Internal(e.to_string()))?;
                Ok((
                    json,
                    "application/json".to_string(),
                    format!("chat-{short}.json"),
                ))
            }
        }
    }

    async fn get_chat_messages(&self, id: &str) -> Result<Vec<ApiChatMessage>, ApiError> {
        let store = self.chat_store.clone();
        let id_check = id.to_string();
        let exists = tokio::task::spawn_blocking(move || store.get_session(&id_check))
            .await
            .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        if exists.is_none() {
            return Err(ApiError::NotFound(format!("Chat session {id} not found")));
        }

        let store = self.chat_store.clone();
        let id_owned = id.to_string();
        let msgs = tokio::task::spawn_blocking(move || store.get_messages(&id_owned))
            .await
            .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        Ok(msgs.into_iter().map(api_chat_message_from).collect())
    }

    async fn send_chat_message(
        &self,
        session_id: &str,
        text: String,
        file_ids: Option<String>,
        owner_principal: &str,
    ) -> Result<ApiChatMessage, ApiError> {
        // Load the session (agent_name + 404 if missing) and prior history.
        let store = self.chat_store.clone();
        let sid = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&sid))
            .await
            .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
            .map_err(|e| ApiError::Internal(e.to_string()))?
            .ok_or_else(|| ApiError::NotFound(format!("Chat session {session_id} not found")))?;
        let agent_name = session.agent_name;

        let store = self.chat_store.clone();
        let sid = session_id.to_string();
        let prior = tokio::task::spawn_blocking(move || store.get_messages(&sid))
            .await
            .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        // Blank turns are never worth replaying, and sessions created before
        // empty-session support carry a placeholder empty user row at the head —
        // an empty user message confuses (or is rejected by) several providers.
        //
        // User turns are re-expanded from the `file_ids` stored on the row rather
        // than replayed verbatim. That is what the web chat does, and it is the
        // reason the transcript can hold the message the user actually typed: the
        // file's content is re-read at send time, so a file that was deleted or
        // pruned stops being replayed instead of being frozen into the session
        // forever at up to 1 MiB per attachment per turn.
        let mut history: Vec<(String, String)> = Vec::with_capacity(prior.len());
        for m in prior {
            if m.content.trim().is_empty() {
                continue;
            }
            match m.role.as_str() {
                "user" => {
                    let (expanded, _) = expand_chat_user_turn(
                        self,
                        &m.content,
                        m.file_ids.as_deref(),
                        owner_principal,
                        &agent_name,
                        session_id,
                    )
                    .await;
                    history.push(("user".to_string(), expanded));
                }
                "assistant" if !agentos_kernel::is_unreplayable_assistant_turn(&m.content) => {
                    history.push((m.role, m.content))
                }
                _ => {}
            }
        }

        // Resolve `@mentions` and attached uploads into the turn the model sees.
        // The same call backs the web chat, so a message means the same thing on
        // both surfaces; skipping it here is what made `@file` inert in the panel.
        let (display, parts) = expand_chat_user_turn(
            self,
            &text,
            file_ids.as_deref(),
            owner_principal,
            &agent_name,
            session_id,
        )
        .await;

        // Persist the message the user actually typed, plus the attachment ids.
        // NOT the expansion: the ids are what let every later turn rebuild the
        // context from the files as they are *then*, and a transcript holding the
        // fenced `<user_data>` blob would render it in the user's own chat bubble
        // and replay a deleted file's contents for the life of the session.
        let store = self.chat_store.clone();
        let sid = session_id.to_string();
        let text_user = text.clone();
        let fids = file_ids.clone();
        tokio::task::spawn_blocking(move || {
            store.add_message(&sid, "user", &text_user, fids.as_deref())
        })
        .await
        .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
        .map_err(|e| ApiError::Internal(e.to_string()))?;
        emit_chat_message_added(self, session_id, "user").await;

        // Run inference directly (not via `chat_send`, which lossily converts the
        // typed tool-call records to JSON) so we can persist the tool rows. Tool
        // execution is included.
        let result = self
            .chat_infer_with_tools(&agent_name, &history, &display, parts, Some(session_id))
            .await
            .map_err(ApiError::Internal)?;

        // Persist tool-call rows before the assistant turn so the timeline orders
        // user → tool… → assistant (mirrors the web UI + streaming path).
        if !result.tool_calls.is_empty() {
            let store = self.chat_store.clone();
            let sid = session_id.to_string();
            let calls = result.tool_calls.clone();
            match tokio::task::spawn_blocking(move || store.add_tool_calls(&sid, &calls)).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::error!("Failed to save chat tool calls: {e}"),
                Err(e) => tracing::error!("spawn_blocking panicked saving tool calls: {e}"),
            }
        }

        // Persist the assistant turn (with token/cost accounting).
        let store = self.chat_store.clone();
        let sid = session_id.to_string();
        let answer = result.answer.clone();
        let tokens = result.tokens_used;
        let cost = result.cost_usd;
        tokio::task::spawn_blocking(move || {
            store.add_assistant_message(
                &sid,
                &answer,
                Some(tokens),
                if cost.is_finite() && cost > 0.0 {
                    Some(cost)
                } else {
                    None
                },
            )
        })
        .await
        .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
        .map_err(|e| ApiError::Internal(e.to_string()))?;

        emit_chat_message_added(self, session_id, "assistant").await;

        Ok(ApiChatMessage {
            role: "assistant".to_string(),
            content: result.answer,
            timestamp: chrono::Utc::now().to_rfc3339(),
            tool_name: None,
            tool_intent_type: None,
            tool_payload_json: None,
            tool_result_json: None,
            tool_success: None,
            tool_duration_ms: None,
        })
    }

    async fn stream_chat_message(
        &self,
        session_id: &str,
        text: String,
        file_ids: Option<String>,
        owner_principal: &str,
        out_tx: mpsc::Sender<ChatStreamEvent>,
    ) -> Result<(), ApiError> {
        // Load session (agent_name + 404) and prior history.
        let store = self.chat_store.clone();
        let sid = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&sid))
            .await
            .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
            .map_err(|e| ApiError::Internal(e.to_string()))?
            .ok_or_else(|| ApiError::NotFound(format!("Chat session {session_id} not found")))?;
        let agent_name = session.agent_name;

        let store = self.chat_store.clone();
        let sid = session_id.to_string();
        let prior = tokio::task::spawn_blocking(move || store.get_messages(&sid))
            .await
            .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        // Blank turns are never worth replaying, and sessions created before
        // empty-session support carry a placeholder empty user row at the head —
        // an empty user message confuses (or is rejected by) several providers.
        //
        // User turns are re-expanded from the `file_ids` stored on the row rather
        // than replayed verbatim. That is what the web chat does, and it is the
        // reason the transcript can hold the message the user actually typed: the
        // file's content is re-read at send time, so a file that was deleted or
        // pruned stops being replayed instead of being frozen into the session
        // forever at up to 1 MiB per attachment per turn.
        let mut history: Vec<(String, String)> = Vec::with_capacity(prior.len());
        for m in prior {
            if m.content.trim().is_empty() {
                continue;
            }
            match m.role.as_str() {
                "user" => {
                    let (expanded, _) = expand_chat_user_turn(
                        self,
                        &m.content,
                        m.file_ids.as_deref(),
                        owner_principal,
                        &agent_name,
                        session_id,
                    )
                    .await;
                    history.push(("user".to_string(), expanded));
                }
                "assistant" if !agentos_kernel::is_unreplayable_assistant_turn(&m.content) => {
                    history.push((m.role, m.content))
                }
                _ => {}
            }
        }

        // Resolve `@mentions` and attached uploads into the turn the model sees.
        // The same call backs the web chat, so a message means the same thing on
        // both surfaces; skipping it here is what made `@file` inert in the panel.
        let (display, parts) = expand_chat_user_turn(
            self,
            &text,
            file_ids.as_deref(),
            owner_principal,
            &agent_name,
            session_id,
        )
        .await;

        // Persist the message the user actually typed, plus the attachment ids.
        // NOT the expansion: the ids are what let every later turn rebuild the
        // context from the files as they are *then*, and a transcript holding the
        // fenced `<user_data>` blob would render it in the user's own chat bubble
        // and replay a deleted file's contents for the life of the session.
        let store = self.chat_store.clone();
        let sid = session_id.to_string();
        let text_user = text.clone();
        let fids = file_ids.clone();
        tokio::task::spawn_blocking(move || {
            store.add_message(&sid, "user", &text_user, fids.as_deref())
        })
        .await
        .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
        .map_err(|e| ApiError::Internal(e.to_string()))?;
        emit_chat_message_added(self, session_id, "user").await;

        // Real token streaming: forward events to the caller while capturing the
        // final answer. Producer + consumer run concurrently so the bounded
        // channel applies natural backpressure.
        let (in_tx, mut in_rx) = mpsc::channel::<ChatStreamEvent>(64);
        let producer = self.chat_infer_streaming(
            &agent_name,
            &history,
            &display,
            parts,
            in_tx,
            Some(session_id),
        );
        // `async move`, so an early return DROPS `in_rx`. Without that the
        // receiver outlives the consumer (it is only borrowed) and the producer
        // never learns the client left: its bounded sends wait out the full
        // 30s timeout instead of failing at once, and every unbounded send in
        // between simply parks against a channel nothing will ever drain.
        let out_tx_stream = out_tx.clone();
        let consumer = async move {
            // `done` is held back rather than forwarded: a client reads it as
            // "the reply is complete, the transcript is now authoritative" and
            // refetches the session. The tool and assistant rows below are
            // written AFTER the producer finishes, so forwarding `done` here
            // races those writes — the refetch usually wins and comes back
            // without the reply, which then only appears on a later reload.
            let mut done = None;
            while let Some(ev) = in_rx.recv().await {
                if matches!(ev, ChatStreamEvent::Done { .. }) {
                    done = Some(ev);
                    continue;
                }
                if out_tx_stream.send(ev).await.is_err() {
                    return None; // client disconnected
                }
            }
            done
        };
        let (res, done_event) = tokio::join!(producer, consumer);
        let result = res.map_err(ApiError::Internal)?;
        let streamed_answer = match &done_event {
            Some(ChatStreamEvent::Done { answer, .. }) => answer.as_str(),
            _ => "",
        };
        let final_answer = if streamed_answer.is_empty() {
            result.answer.clone()
        } else {
            streamed_answer.to_string()
        };

        // Persist tool-call rows before the assistant turn so the timeline orders
        // user → tool… → assistant (mirrors the web UI + non-streaming path).
        if !result.tool_calls.is_empty() {
            let store = self.chat_store.clone();
            let sid = session_id.to_string();
            let calls = result.tool_calls.clone();
            match tokio::task::spawn_blocking(move || store.add_tool_calls(&sid, &calls)).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::error!("Failed to save chat tool calls: {e}"),
                Err(e) => tracing::error!("spawn_blocking panicked saving tool calls: {e}"),
            }
        }

        // Persist the assistant turn (with token/cost accounting).
        let store = self.chat_store.clone();
        let sid = session_id.to_string();
        let tokens = result.tokens_used;
        let cost = result.cost_usd;
        let _ = tokio::task::spawn_blocking(move || {
            store.add_assistant_message(&sid, &final_answer, Some(tokens), Some(cost))
        })
        .await;

        // Everything is persisted — now the client may refetch.
        emit_chat_message_added(self, session_id, "assistant").await;
        if let Some(ev) = done_event {
            let _ = out_tx.send(ev).await;
        }
        Ok(())
    }

    // ── Agent conversations (read-only) ──────────────────────────────────────

    async fn list_convos(&self, kind: Option<&str>) -> Result<Vec<ApiConvoSummary>, ApiError> {
        let store = self.convo_store.clone();
        let kind = kind.map(str::to_string);
        let convos = tokio::task::spawn_blocking(move || store.list_convos(kind.as_deref()))
            .await
            .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        Ok(convos.into_iter().map(api_convo_summary_from).collect())
    }

    async fn get_convo(&self, id: &str) -> Result<ApiConvoDetail, ApiError> {
        let store = self.convo_store.clone();
        let id_owned = id.to_string();
        let convo = tokio::task::spawn_blocking(move || store.get_convo(&id_owned))
            .await
            .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
            .map_err(|e| ApiError::Internal(e.to_string()))?
            .ok_or_else(|| ApiError::NotFound(format!("Conversation {id} not found")))?;

        let store = self.convo_store.clone();
        let id_owned = id.to_string();
        let turns = tokio::task::spawn_blocking(move || store.get_turns(&id_owned))
            .await
            .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
            .map_err(|e| ApiError::Internal(e.to_string()))?;

        Ok(ApiConvoDetail {
            id: convo.id,
            topic: convo.topic,
            participants: convo.participants,
            status: convo.status,
            max_turns: convo.max_turns,
            created_at: convo.created_at,
            updated_at: convo.updated_at,
            messages: turns.into_iter().map(api_convo_turn_from).collect(),
        })
    }

    async fn create_agent_chat(
        &self,
        topic: String,
        participants: Vec<String>,
        max_turns: u32,
    ) -> Result<ApiConvoSummary, ApiError> {
        let trimmed = topic.trim();
        if trimmed.is_empty() || trimmed.chars().count() > 1000 {
            return Err(ApiError::BadRequest(
                "Topic is required (max 1000 chars)".into(),
            ));
        }
        let topic = trimmed.to_string();
        if !(2..=8).contains(&participants.len()) {
            return Err(ApiError::BadRequest(
                "A conversation needs between 2 and 8 participants".into(),
            ));
        }
        check_convo_participants(self, &participants).await?;
        let store = self.convo_store.clone();
        let t = topic.clone();
        let p = participants.clone();
        let id = tokio::task::spawn_blocking(move || store.create_convo(&t, &p, max_turns))
            .await
            .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        Ok(ApiConvoSummary {
            id,
            topic,
            participants,
            // Matches the value persisted by `ConvoStore::create_convo` and the
            // documented status enum (running|complete|stopped|error).
            status: "running".to_string(),
            updated_at: chrono::Utc::now().to_rfc3339(),
            // This endpoint only ever creates operator-started conversations;
            // `dm` rows are opened by `agent-message` inside the kernel.
            kind: "operator".to_string(),
        })
    }

    async fn run_agent_chat(
        &self,
        id: &str,
        topic: String,
        participants: Vec<String>,
        max_turns: u32,
    ) {
        // The loop itself lives in `agentos_kernel::convo_runner` — shared with
        // the web UI orchestrator so a convo fix lands once. Progress is relayed
        // to the `agent-chat:<id>` realtime channel (scope `chat:r`, same as
        // `GET {id}`) so the panel can show who is speaking and their words as
        // they arrive. Every turn is still persisted; a missed frame costs a
        // repaint, never a turn.
        let (tx, rx) = mpsc::channel(64);
        let relay = tokio::spawn(agentos_kernel::convo_runner::relay_convo_events(
            rx,
            self.realtime_event_sender.clone(),
            format!("agent-chat:{id}"),
        ));
        agentos_kernel::convo_runner::run_convo(
            self,
            id,
            &topic,
            &participants,
            max_turns,
            Some(tx),
        )
        .await;
        // The runner dropped its sender, so the relay drains and exits.
        let _ = relay.await;
    }

    async fn stop_agent_chat(&self, id: &str) -> Result<(), ApiError> {
        let store = self.convo_store.clone();
        let sid = id.to_string();
        tokio::task::spawn_blocking(move || store.set_status(&sid, "stopped"))
            .await
            .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    ApiError::NotFound(format!("Agent conversation {id} not found"))
                }
                other => ApiError::Internal(other.to_string()),
            })
    }

    async fn continue_agent_chat(
        &self,
        id: &str,
        turns: u32,
    ) -> Result<(ApiConvoSummary, u32), ApiError> {
        let convo = load_convo(self, id).await?;
        check_convo_participants(self, &convo.participants).await?;
        let ceiling = claim_convo_resume(self, id, turns).await?.ok_or_else(|| {
            ApiError::Conflict(
                "Conversation is still running — stop it or wait for the current turn".into(),
            )
        })?;
        Ok((convo_summary_with_status(convo, "running"), ceiling))
    }

    async fn post_agent_chat_message(
        &self,
        id: &str,
        content: String,
    ) -> Result<(ApiConvoSummary, Option<u32>), ApiError> {
        let content = content.trim().to_string();
        if content.is_empty() || content.chars().count() > 4000 {
            return Err(ApiError::BadRequest(
                "Message is required (max 4000 chars)".into(),
            ));
        }
        let convo = load_convo(self, id).await?;
        // Only a reopen needs every agent online; a live run can still take a
        // message after one drops (its turn fails visibly).
        if convo.status != "running" {
            check_convo_participants(self, &convo.participants).await?;
        }

        let store = self.convo_store.clone();
        let cid = id.to_string();
        tokio::task::spawn_blocking(move || {
            store.add_turn(&cid, agentos_kernel::convo_store::USER_SPEAKER, &content, 0)
        })
        .await
        .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
        .map_err(|e| ApiError::Internal(e.to_string()))?;

        // Stored first, so a live run reads it on its next turn (the runner grants
        // an extra round if its budget is spent); a finished one reopens for a
        // round so every participant answers once.
        // ponytail: a stopped run still finishing its last turn is Busy here, so
        // the message waits for Continue.
        let round = convo.participants.len() as u32;
        let ceiling = claim_convo_resume(self, id, round).await?;
        let status = if ceiling.is_some() {
            "running".to_string()
        } else {
            convo.status.clone()
        };
        Ok((convo_summary_with_status(convo, &status), ceiling))
    }

    // ── Realtime (Phase 08) ───────────────────────────────────────────────

    fn subscribe_realtime(&self) -> tokio::sync::broadcast::Receiver<agentos_types::RealtimeEvent> {
        self.realtime_event_sender.subscribe()
    }
}

#[cfg(test)]
mod convo_relay_tests {
    use agentos_kernel::convo_runner::relay_convo_events;
    use agentos_kernel::convo_runner::ConvoEvent;
    use agentos_kernel::ChatStreamEvent;

    #[tokio::test]
    async fn relays_speaker_coalesced_text_and_lifecycle() {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let (rt_tx, mut rt_rx) = tokio::sync::broadcast::channel(16);
        let relay = tokio::spawn(relay_convo_events(rx, rt_tx, "agent-chat:c1".into()));

        let chat = |event| ConvoEvent::Chat {
            agent: "alice".into(),
            turn: 1,
            event,
        };
        tx.send(ConvoEvent::TurnStart {
            agent: "alice".into(),
            turn: 1,
        })
        .await
        .unwrap();
        tx.send(chat(ChatStreamEvent::TextChunk { text: "Hel".into() }))
            .await
            .unwrap();
        tx.send(chat(ChatStreamEvent::TextChunk { text: "lo".into() }))
            .await
            .unwrap();
        tx.send(ConvoEvent::TurnEnd {
            agent: "alice".into(),
            turn: 1,
            answer: "Hello".into(),
        })
        .await
        .unwrap();
        tx.send(ConvoEvent::Done { total_turns: 1 }).await.unwrap();
        drop(tx);
        relay.await.unwrap();

        let mut frames = Vec::new();
        while let Ok(ev) = rt_rx.try_recv() {
            assert_eq!(ev.channel, "agent-chat:c1");
            frames.push((ev.event, ev.data));
        }
        let names: Vec<&str> = frames.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["turn.start", "turn.text", "turn.end", "convo.done"]);
        assert_eq!(frames[0].1["agent"], "alice");
        // Both chunks land in one frame, attributed to the speaker.
        assert_eq!(frames[1].1["text"], "Hello");
        assert_eq!(frames[1].1["agent"], "alice");
        assert_eq!(frames[1].1["turn"], 1);
    }

    /// Text is flushed by the timer while the model is quiet, before a tool
    /// frame, and on a speaker change — always attributed to whoever wrote it.
    #[tokio::test(start_paused = true)]
    async fn flushes_on_timer_tool_and_speaker_change() {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let (rt_tx, mut rt_rx) = tokio::sync::broadcast::channel(16);
        let relay = tokio::spawn(relay_convo_events(rx, rt_tx, "agent-chat:c1".into()));
        let chat = |agent: &str, turn, event| ConvoEvent::Chat {
            agent: agent.into(),
            turn,
            event,
        };
        let text = |t: &str| ChatStreamEvent::TextChunk { text: t.into() };
        let drain = |rx: &mut tokio::sync::broadcast::Receiver<agentos_types::RealtimeEvent>| {
            let mut frames = Vec::new();
            while let Ok(ev) = rx.try_recv() {
                frames.push((ev.event, ev.data));
            }
            frames
        };

        // Quiet model: the timer flushes without further input.
        tx.send(chat("a", 1, text("one"))).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let got = drain(&mut rt_rx);
        assert_eq!(got.len(), 1);
        assert_eq!(
            (got[0].0.as_str(), &got[0].1["text"]),
            ("turn.text", &"one".into())
        );

        // Pending text lands before the tool frame; the result clears the tool.
        tx.send(chat("a", 1, text("two"))).await.unwrap();
        tx.send(chat(
            "a",
            1,
            ChatStreamEvent::ToolStart {
                tool_name: "web-search".into(),
                iteration: 1,
                task_id: None,
            },
        ))
        .await
        .unwrap();
        tx.send(chat(
            "a",
            1,
            ChatStreamEvent::ToolResult {
                tool_name: "web-search".into(),
                result_preview: String::new(),
                duration_ms: 1,
                success: true,
            },
        ))
        .await
        .unwrap();

        // Speaker change with text pending: each chunk keeps its author. The
        // last chunk is only flushed by the channel closing.
        tx.send(chat("a", 1, text("three"))).await.unwrap();
        tx.send(chat("b", 2, text("four"))).await.unwrap();
        drop(tx);
        relay.await.unwrap();

        let got = drain(&mut rt_rx);
        let rest: Vec<_> = got
            .iter()
            .map(|(e, d)| {
                (
                    e.as_str(),
                    d["agent"].clone(),
                    d["text"].clone(),
                    d["tool_name"].clone(),
                )
            })
            .collect();
        use serde_json::Value::Null;
        assert_eq!(
            rest,
            [
                ("turn.text", "a".into(), "two".into(), Null),
                ("turn.tool", "a".into(), Null, "web-search".into()),
                ("turn.tool", "a".into(), Null, Null),
                ("turn.text", "a".into(), "three".into(), Null),
                ("turn.text", "b".into(), "four".into(), Null),
            ]
        );
    }
}

#[cfg(test)]
mod parse_scope_tests {
    use super::parse_scope;
    use agentos_types::SecretScope;

    #[test]
    fn accepts_the_four_documented_forms() {
        assert!(matches!(parse_scope("global"), Ok(SecretScope::Global)));
        assert!(matches!(parse_scope("Kernel"), Ok(SecretScope::Kernel)));
        // agent:/tool: return a placeholder the kernel replaces via `scope_raw`;
        // what matters is that it is NOT Global (readable by every agent).
        for s in ["agent:billing-bot", "tool:file-reader"] {
            assert!(
                !matches!(parse_scope(s), Ok(SecretScope::Global)),
                "{s} must not silently widen to Global"
            );
            assert!(parse_scope(s).is_ok(), "{s} must be accepted");
        }
    }

    #[test]
    fn rejects_unrecognised_scopes_instead_of_defaulting_to_global() {
        for s in ["", "nonsense", "agent:", "tool:", "agent", "user:bob"] {
            let err = parse_scope(s).expect_err("must be rejected, not defaulted to Global");
            let msg = err.to_string();
            assert!(
                msg.contains("agent:<name>"),
                "message must list the accepted forms: {msg}"
            );
        }
    }
}

// ── Agent conversation helpers ────────────────────────────────────────────────

/// Every participant must be a registered, online agent with a well-formed name
/// (names are interpolated into turn prompts unwrapped). Shared by create and
/// by resume, where an agent may have gone offline since.
async fn check_convo_participants(
    kernel: &Kernel,
    participants: &[String],
) -> Result<(), ApiError> {
    let registry = kernel.agent_registry.read().await;
    for (i, name) in participants.iter().enumerate() {
        if !agentos_kernel::commands::agent::is_valid_agent_name(name) {
            return Err(ApiError::BadRequest(format!(
                "Invalid agent name: '{name}'"
            )));
        }
        if participants[..i].contains(name) {
            return Err(ApiError::BadRequest(
                "Duplicate participants are not allowed".into(),
            ));
        }
        match registry.get_by_name(name) {
            None => return Err(ApiError::BadRequest(format!("Agent '{name}' not found"))),
            Some(a) if a.status == agentos_types::AgentStatus::Offline => {
                return Err(ApiError::BadRequest(format!("Agent '{name}' is offline")))
            }
            Some(_) => {}
        }
    }
    Ok(())
}

async fn load_convo(
    kernel: &Kernel,
    id: &str,
) -> Result<agentos_kernel::convo_store::AgentConvo, ApiError> {
    let store = kernel.convo_store.clone();
    let id_owned = id.to_string();
    tokio::task::spawn_blocking(move || store.get_convo(&id_owned))
        .await
        .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("Conversation {id} not found")))
}

/// `Some(new ceiling)` when reopened, `None` when a run is already live.
async fn claim_convo_resume(
    kernel: &Kernel,
    id: &str,
    turns: u32,
) -> Result<Option<u32>, ApiError> {
    use agentos_kernel::convo_store::ResumeError;
    let store = kernel.convo_store.clone();
    let id_owned = id.to_string();
    match tokio::task::spawn_blocking(move || store.claim_resume(&id_owned, turns))
        .await
        .map_err(|e| ApiError::Internal(format!("Join error: {e}")))?
    {
        Ok(ceiling) => Ok(Some(ceiling)),
        Err(ResumeError::Busy) => Ok(None),
        Err(ResumeError::NotFound) => {
            Err(ApiError::NotFound(format!("Conversation {id} not found")))
        }
        Err(ResumeError::Db(e)) => Err(ApiError::Internal(e.to_string())),
    }
}

fn convo_summary_with_status(
    convo: agentos_kernel::convo_store::AgentConvo,
    status: &str,
) -> ApiConvoSummary {
    ApiConvoSummary {
        id: convo.id,
        topic: convo.topic,
        participants: convo.participants,
        status: status.to_string(),
        updated_at: chrono::Utc::now().to_rfc3339(),
        kind: convo.kind,
    }
}

#[cfg(test)]
mod avatar_tests {
    use super::validate_avatar;

    #[test]
    fn accepts_raster_data_urls_only() {
        assert!(validate_avatar("data:image/png;base64,iVBORw0KGgo=").is_ok());
        assert!(validate_avatar("data:image/webp;base64,UklGRgAAAABXRUJQVlA4IA==").is_ok());
        // Declared type must match the bytes (PNG bytes labelled webp).
        assert!(validate_avatar("data:image/webp;base64,iVBORw0KGgo=").is_err());
        // SVG can carry script; non-image and non-base64 payloads are refused.
        assert!(validate_avatar("data:image/svg+xml;base64,PHN2Zz4=").is_err());
        assert!(validate_avatar("https://example.com/a.png").is_err());
        assert!(validate_avatar("data:image/png;base64,\"><script>").is_err());
        let huge = format!("data:image/png;base64,{}", "A".repeat(64 * 1024));
        assert!(validate_avatar(&huge).is_err());
    }
}

fn mcp_server_permission(server: &str) -> String {
    format!(
        "{}:x",
        agentos_mcp::adapter::server_permission_resource(server)
    )
}
