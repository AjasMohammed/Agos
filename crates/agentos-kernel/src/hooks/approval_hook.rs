use super::Hook;
use crate::agent_registry::AgentRegistry;
use crate::approval_policy_store::ApprovalPolicyMatcher;
use crate::config::ApprovalConfig;
use crate::escalation::{AutoAction, EscalationManager};
use crate::kernel_action::EscalationReason;
use crate::tool_registry::ToolRegistry;
use agentos_connectors::ConnectorRegistry;
use agentos_types::{
    AgentID, ApprovalDecision, ApprovalMode, HookEvent, HookResult, RiskClass, TraceID,
};
use async_trait::async_trait;
use std::sync::{Arc, RwLock as StdRwLock};
use tokio::sync::RwLock;

/// Hot-reloadable per-agent + global approval mode lookup.
///
/// The resolver is consulted on every `ToolPre` event, so the read path uses a
/// `std::sync::RwLock` (sync, no `.await`) wrapped around the latest
/// [`ApprovalConfig`]. The kernel's `ConfigWatcher` calls [`Self::reload`]
/// when the config file changes.
///
/// Resolution order: `agent_overrides[<agent name>]` → `mode` (global).
pub struct ApprovalModeResolver {
    config: StdRwLock<ApprovalConfig>,
    /// Agent registry used to resolve `AgentID -> display name` so per-agent
    /// overrides (which are name-keyed for human readability in TOML) can
    /// be looked up given only the `AgentID` from the hook event.
    agent_registry: Arc<RwLock<AgentRegistry>>,
}

impl ApprovalModeResolver {
    pub fn new(config: ApprovalConfig, agent_registry: Arc<RwLock<AgentRegistry>>) -> Arc<Self> {
        Arc::new(Self {
            config: StdRwLock::new(config),
            agent_registry,
        })
    }

    /// Resolve the active mode for `agent_id`. Falls back to the global mode
    /// when the agent name can't be looked up or has no override. Uses the
    /// registry's O(1) lookup by ID, not a scan.
    pub async fn mode_for(&self, agent_id: &AgentID) -> ApprovalMode {
        let agent_name = {
            let reg = self.agent_registry.read().await;
            reg.get_by_id(agent_id).map(|a| a.name.clone())
        };
        let cfg = match self.config.read() {
            Ok(g) => g,
            Err(_) => {
                tracing::error!("approval mode RwLock poisoned; falling back to ask_edit default");
                return ApprovalMode::default();
            }
        };
        if let Some(name) = agent_name {
            if let Some(m) = cfg.agent_overrides.get(&name) {
                return *m;
            }
        }
        cfg.mode
    }

    /// Sync variant for callers that already know the agent display name
    /// (e.g. CLI tooling). Avoids the async agent registry lookup.
    pub fn mode_for_name(&self, agent_name: Option<&str>) -> ApprovalMode {
        let cfg = match self.config.read() {
            Ok(g) => g,
            Err(_) => return ApprovalMode::default(),
        };
        if let Some(name) = agent_name {
            if let Some(m) = cfg.agent_overrides.get(name) {
                return *m;
            }
        }
        cfg.mode
    }

    /// Replace the current config snapshot. Called by `ConfigWatcher` on
    /// successful reload. On poison the write still succeeds (the poison is
    /// only meaningful if some prior writer left invalid state mid-write;
    /// here we're unconditionally overwriting).
    pub fn reload(&self, new_config: ApprovalConfig) {
        let mut guard = self.config.write().unwrap_or_else(|poisoned| {
            tracing::error!("approval mode RwLock poisoned; overwriting through poison guard");
            poisoned.into_inner()
        });
        *guard = new_config;
    }

    /// Snapshot the current config. Used by the CLI to render
    /// `agentos approval mode get`.
    pub fn snapshot(&self) -> ApprovalConfig {
        match self.config.read() {
            Ok(g) => g.clone(),
            Err(_) => ApprovalConfig::default(),
        }
    }
}

/// Auto-approve rule: matches a specific risk class (and optional path prefix).
#[derive(Debug, Clone)]
pub struct AutoApproveRule {
    /// Risk classes this rule covers.
    pub risk_classes: Vec<RiskClass>,
    /// If set, the tool input's `path` field must start with this prefix.
    /// The prefix is checked against the actual parsed JSON value, not a raw substring.
    pub path_prefix: Option<String>,
    /// Human-readable description for `agentos doctor` output.
    pub description: String,
}

/// Policy that determines which tool calls should be auto-approved vs. escalated.
pub struct AutoApprovePolicy {
    rules: Vec<AutoApproveRule>,
}

impl AutoApprovePolicy {
    /// Default policy: auto-approve all read operations, and writes strictly within /tmp.
    pub fn default_rules() -> Self {
        Self {
            rules: vec![
                // ReadonlyScoped only. `ReadonlyExternal` is deliberately NOT
                // blanket-approved here: `ApprovalMode::decide` already returns
                // `Allow` for it under `auto`/`ask_edit` (so it never reaches this
                // policy), and returns `Prompt` under `ask_always` — where the
                // operator has explicitly opted to gate network reads. Lifting it
                // here would silently override that stricter mode (e.g. a
                // prompt-injected agent exfiltrating context via web-fetch).
                AutoApproveRule {
                    risk_classes: vec![RiskClass::ReadonlyScoped],
                    path_prefix: None,
                    description: "Auto-approve local read operations".to_string(),
                },
                AutoApproveRule {
                    risk_classes: vec![RiskClass::WriteScoped],
                    path_prefix: Some("/tmp/".to_string()),
                    description: "Auto-approve writes strictly under /tmp/".to_string(),
                },
            ],
        }
    }

    /// Returns `true` if this tool call should be auto-approved without human review.
    ///
    /// This is only consulted after `ApprovalMode::decide` has already returned
    /// `Prompt` (never `Allow`), so it must lift a call *only* via an explicit
    /// matching rule. It deliberately does NOT short-circuit on the risk class
    /// alone: re-deriving approval-necessity from the class would override the
    /// operator's chosen mode (the `ask_always` + `ReadonlyExternal` bypass).
    ///
    /// Path-prefix rules parse the JSON to extract the actual `path` field value,
    /// preventing bypass via crafted JSON strings that merely *contain* the prefix.
    pub fn should_auto_approve(&self, risk_class: &RiskClass, input_json: &str) -> bool {
        for rule in &self.rules {
            if !rule.risk_classes.contains(risk_class) {
                continue;
            }
            match &rule.path_prefix {
                None => return true, // class match with no path constraint
                Some(prefix) => {
                    // Parse JSON and check the actual `path` field — never do a raw
                    // substring search, which is trivially bypassable via JSON injection.
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(input_json) {
                        if let Some(path) = parsed.get("path").and_then(|v| v.as_str()) {
                            // Reject any path with traversal components before prefix check.
                            // A path like `/tmp/../etc/passwd` starts with `/tmp/` as a string
                            // but resolves outside it — defense-in-depth even if the file tool
                            // also canonicalizes.
                            if path.contains("..") {
                                // Traversal component present — never auto-approve.
                            } else if path.starts_with(prefix.as_str()) {
                                return true;
                            }
                        }
                    }
                    // Fall through to check remaining rules.
                }
            }
        }
        false
    }
}

/// Pre-hook that decides whether a tool call may run, surfaces approval
/// prompts, or hard-denies the call.
///
/// Decision flow on every `ToolPre` event:
/// 1. Look up the tool's `risk_class`. Tools absent from the registry are
///    fast-aborted (never escalated — nothing a human could approve, and
///    execution would fail with ToolNotFound anyway).
/// 2. Look up the active [`ApprovalMode`] for the agent via the resolver
///    (agent-specific override → global default).
/// 3. Apply the mode-vs-risk-class matrix: `ApprovalDecision::{Allow, Prompt, Deny}`.
/// 4. If `Prompt`, ask the legacy `AutoApprovePolicy` for an upward override
///    (e.g. a learned "allow always" entry) that can lift the prompt to Allow.
/// 5. `Allow` → `HookResult::Continue`. `Deny` → `HookResult::Abort`.
///    `Prompt` → create a blocking `PendingEscalation` and abort.
///
/// `ControlPlane` operations always prompt under non-`Deny` modes — kernel
/// admin actions surface even when the operator opted into auto-approval.
pub struct ApprovalHook {
    policy: AutoApprovePolicy,
    escalations: Arc<EscalationManager>,
    tool_registry: Arc<RwLock<ToolRegistry>>,
    mode_resolver: Arc<ApprovalModeResolver>,
    /// Operator-curated persistent policy. Lifts `Prompt → Allow` for
    /// specific `(tool, payload, agent)` matches. `None` if the kernel
    /// chose not to wire a policy matcher (e.g. early tests).
    policy_matcher: Option<Arc<ApprovalPolicyMatcher>>,
    /// Connector namespace lookup. Connector calls (`github.create_issue`)
    /// are NEVER in `tool_registry` — `install_connector_manifest` only
    /// registers them here — so without this handle every connector call
    /// hits the unknown-tool fast-abort below. `None` disables connector
    /// resolution (tests, early boot).
    connector_registry: Option<Arc<ConnectorRegistry>>,
}

impl ApprovalHook {
    /// Construct without connector resolution. Prefer
    /// [`Self::with_connectors`] in the kernel: a hook built here aborts
    /// every `connector_id.operation` call as an unknown tool.
    pub fn new(
        policy: AutoApprovePolicy,
        escalations: Arc<EscalationManager>,
        tool_registry: Arc<RwLock<ToolRegistry>>,
        mode_resolver: Arc<ApprovalModeResolver>,
        policy_matcher: Option<Arc<ApprovalPolicyMatcher>>,
    ) -> Arc<Self> {
        Self::with_connectors(
            policy,
            escalations,
            tool_registry,
            mode_resolver,
            policy_matcher,
            None,
        )
    }

    pub fn with_connectors(
        policy: AutoApprovePolicy,
        escalations: Arc<EscalationManager>,
        tool_registry: Arc<RwLock<ToolRegistry>>,
        mode_resolver: Arc<ApprovalModeResolver>,
        policy_matcher: Option<Arc<ApprovalPolicyMatcher>>,
        connector_registry: Option<Arc<ConnectorRegistry>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            policy,
            escalations,
            tool_registry,
            mode_resolver,
            policy_matcher,
            connector_registry,
        })
    }

    /// Risk class for a name absent from `tool_registry`.
    ///
    /// `Some(ExecCapable)` when the name is a well-formed
    /// `connector_id.operation` that a registered connector actually exposes:
    /// connectors carry no manifest (so no declared risk class) and perform
    /// external writes, so the fail-closed class is the right floor. `None` for
    /// anything else — the caller then fast-aborts as before.
    ///
    /// The whole `connector.operation` is checked, not just the namespace.
    /// `ConnectorRegistry::route` forwards any operation to the proxy, which
    /// answers `ToolNotFound`, so matching on the connector alone would re-open
    /// the hallucinated-name escalation park inside every registered namespace
    /// (`github.list_my_issues` → ExecCapable → Prompt under the default
    /// `ask_edit` → the task parks until the ~5-min auto-deny).
    async fn connector_risk_class(&self, tool_name: &str) -> Option<RiskClass> {
        let (connector_id, operation) = tool_name.split_once('.')?;
        if connector_id.is_empty() || operation.is_empty() {
            return None;
        }
        // `all_tool_names` returns fully-qualified `connector.op` strings, so a
        // match implies the connector exists — one lock, no `has_connector`.
        // ponytail: allocates a Vec per unregistered-tool call; add
        // `ConnectorRegistry::has_tool` if this ever shows up in a profile.
        self.connector_registry
            .as_ref()?
            .all_tool_names()
            .await
            .iter()
            .any(|name| name == tool_name)
            .then_some(RiskClass::ExecCapable)
    }
}

#[async_trait]
impl Hook for ApprovalHook {
    fn name(&self) -> &'static str {
        "approval"
    }

    fn handles(&self, event: &HookEvent) -> bool {
        matches!(event, HookEvent::ToolPre { .. })
    }

    async fn on_event(&self, event: &HookEvent) -> HookResult {
        let HookEvent::ToolPre {
            task_id,
            agent_id,
            tool_name,
            input_json,
        } = event
        else {
            return HookResult::Continue;
        };

        // Look up the tool's risk class by name. The registry guard is
        // dropped before the connector lookup below — that awaits another
        // lock, and holding a read guard across it invites a deadlock with a
        // concurrent `tool_registry.write()`.
        let registered_risk_class = {
            let registry = self.tool_registry.read().await;
            registry
                .get_by_name(tool_name)
                .map(|t| t.manifest.risk_class.clone())
        };
        let risk_class = match registered_risk_class {
            Some(rc) => rc,
            // Not a registered tool. A connector call routes through the
            // connector registry instead and is legitimately absent here, so
            // resolve it fail-closed rather than aborting.
            None => match self.connector_risk_class(tool_name).await {
                Some(rc) => rc,
                // Genuinely unknown: execution would fail with ToolNotFound
                // anyway, so there is nothing a human could approve.
                // Escalating here parks the caller on the pending escalation
                // until the ~5-min auto-deny sweep — per hallucinated tool
                // name — which stalls chat and task loops for no gain.
                // Fast-abort instead: still fail-closed (the call is
                // blocked), the error lands in the LLM's context immediately
                // and it can self-correct on the next turn.
                None => {
                    return HookResult::Abort(format!(
                        "Tool '{tool_name}' not found — nothing to approve. \
                         Use list-tools or search-tools to discover available tools."
                    ));
                }
            },
        };

        // Mode-driven base decision (auto / ask_edit / ask_always / deny).
        let mode = self.mode_resolver.mode_for(agent_id).await;
        let base_decision = mode.decide(risk_class.clone());

        // Allow: nothing to do.
        if matches!(base_decision, ApprovalDecision::Allow) {
            return HookResult::Continue;
        }

        // Deny: hard-reject. No escalation, no waiting. Audited via the
        // ToolPre/ToolPost hook chain; the Abort string flows through to the
        // tool result.
        if matches!(base_decision, ApprovalDecision::Deny) {
            tracing::warn!(
                tool = %tool_name,
                agent_id = %agent_id,
                ?risk_class,
                %mode,
                "Tool call denied by approval mode"
            );
            return HookResult::Abort(format!(
                "Tool '{tool_name}' denied by approval mode `{mode}` \
                 (risk class: {risk_class:?}). Change the mode with \
                 `agentos approval mode set <auto|ask_edit|ask_always>` or \
                 grant a per-agent override."
            ));
        }

        // Prompt: legacy AutoApprovePolicy + learned-policy entries can still
        // lift `Prompt → Allow` for specific (tool, payload) combinations.
        //
        // EXCEPTION: ControlPlane is the non-overridable floor (see
        // `ApprovalMode::decide` docs). A learned "allow always" entry must
        // NOT be able to silently let kernel-admin operations bypass human
        // review — that defeats the whole point of marking a tool as
        // ControlPlane. We skip both lift-to-allow paths for ControlPlane
        // and proceed directly to the escalation create.
        let is_control_plane = matches!(risk_class, RiskClass::ControlPlane);
        if !is_control_plane {
            if self.policy.should_auto_approve(&risk_class, input_json) {
                return HookResult::Continue;
            }
            if let Some(matcher) = &self.policy_matcher {
                let payload_path = serde_json::from_str::<serde_json::Value>(input_json)
                    .ok()
                    .as_ref()
                    .and_then(|v| v.get("path"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                // Path-traversal guard, matching `should_auto_approve` above: a
                // learned "allow" entry must never auto-approve a path containing
                // `..` (e.g. `/tmp/../etc/passwd`). Fall through to escalation.
                let has_traversal = payload_path
                    .as_deref()
                    .map(|p| p.contains(".."))
                    .unwrap_or(false);
                if !has_traversal && matcher.allows(tool_name, agent_id, payload_path.as_deref()) {
                    tracing::info!(
                        tool = %tool_name,
                        agent_id = %agent_id,
                        "Tool call allowed by learned approval policy"
                    );
                    return HookResult::Continue;
                }
            }
        }

        // Create a blocking escalation for human review.
        let summary = format!(
            "Tool '{}' requires approval. Risk class: {:?}. Input preview: {}",
            tool_name,
            risk_class,
            if input_json.chars().count() > 200 {
                format!(
                    "{}…",
                    input_json
                        .char_indices()
                        .nth(200)
                        .map(|(i, _)| &input_json[..i])
                        .unwrap_or(input_json)
                )
            } else {
                input_json.clone()
            }
        );

        // Structured copy of what the prompt says, for "approve & remember":
        // the resolver mints a standing grant from `tool_name` (+ the payload
        // `path`, when there is one) instead of parsing the prose above.
        let payload_path = serde_json::from_str::<serde_json::Value>(input_json)
            .ok()
            .and_then(|v| v.get("path").and_then(|p| p.as_str()).map(str::to_string));
        let metadata = serde_json::json!({
            "kind": crate::approval_policy_store::TOOL_APPROVAL_KIND,
            "tool_name": tool_name,
            "risk_class": format!("{risk_class:?}"),
            "path": payload_path,
        });
        let escalation_id = self
            .escalations
            .create_escalation_with_metadata(
                *task_id,
                *agent_id,
                EscalationReason::AuthorizationRequired,
                summary,
                format!(
                    "Tool '{}' (risk: {:?}) awaiting approval",
                    tool_name, risk_class
                ),
                vec!["approve".to_string(), "deny".to_string()],
                "high".to_string(),
                true, // blocking
                TraceID::new(),
                Some(AutoAction::Deny),
                metadata,
            )
            .await;

        // Escalation cap reached: DENY the call (fail-closed).
        // Fail-open here would allow an attacker to bypass approval by flooding the queue.
        if escalation_id == u64::MAX {
            tracing::error!(
                task_id = %task_id,
                tool = %tool_name,
                "Escalation cap reached — BLOCKING tool call for safety"
            );
            return HookResult::Abort(
                "Escalation cap reached — tool call denied for safety. \
                 Resolve pending escalations with `agentos escalation resolve`."
                    .to_string(),
            );
        }

        // Install a oneshot resolution channel so the awaiting
        // `task_executor` can be woken when the human resolves the
        // escalation. Without this, the abort would always surface as a
        // tool failure — see ACF Phase 4 plan.
        self.escalations.prepare_resolution(escalation_id).await;

        // The reason string is a structured tag — `task_executor`
        // greps for the `approval_pending:<id>` prefix so it can
        // decide to park on the resolution channel rather than
        // surfacing a hard tool failure. Anything before the colon
        // matters; the trailing human prose is for logs only.
        HookResult::Abort(format!(
            "approval_pending:{escalation_id}: tool '{tool_name}' requires human \
             approval (escalation ID {escalation_id}). \
             Reply `/approve {escalation_id}` on a paired channel or run \
             `agentos escalation resolve {escalation_id} --decision approve`."
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auto_approve_readonly() {
        let policy = AutoApprovePolicy::default_rules();
        // Local reads are auto-approved by the policy.
        assert!(policy.should_auto_approve(&RiskClass::ReadonlyScoped, "{}"));
        // ReadonlyExternal is NOT blanket-approved: reaching this policy means the
        // mode decided `Prompt` (ask_always), which must be honored — not lifted.
        assert!(!policy.should_auto_approve(&RiskClass::ReadonlyExternal, "{}"));
    }

    #[test]
    fn test_no_auto_approve_exec() {
        let policy = AutoApprovePolicy::default_rules();
        assert!(!policy.should_auto_approve(&RiskClass::ExecCapable, "{}"));
        assert!(!policy.should_auto_approve(&RiskClass::ControlPlane, "{}"));
        assert!(!policy.should_auto_approve(&RiskClass::Interactive, "{}"));
    }

    #[test]
    fn test_auto_approve_write_strictly_in_tmp() {
        let policy = AutoApprovePolicy::default_rules();
        // Actual path starts with /tmp/ → approved
        assert!(policy.should_auto_approve(&RiskClass::WriteScoped, r#"{"path": "/tmp/foo.txt"}"#));
        // Actual path is outside /tmp/ → denied
        assert!(!policy.should_auto_approve(
            &RiskClass::WriteScoped,
            r#"{"path": "/home/user/secret.txt"}"#
        ));
    }

    #[test]
    fn test_path_prefix_not_bypassable_via_json_injection() {
        let policy = AutoApprovePolicy::default_rules();
        // The string "/tmp/" appears in the JSON but NOT in the "path" field.
        // A raw substring check would incorrectly approve this.
        assert!(!policy.should_auto_approve(
            &RiskClass::WriteScoped,
            r#"{"path": "/home/evil.sh", "note": "copying from /tmp/ to here"}"#
        ));
        // Nested path injection attempt
        assert!(!policy.should_auto_approve(
            &RiskClass::WriteScoped,
            r#"{"target": "/home/evil.sh", "source": "/tmp/innocent"}"#
        ));
    }

    #[test]
    fn test_malformed_json_is_denied() {
        let policy = AutoApprovePolicy::default_rules();
        // Malformed JSON for a write: can't parse path → denied
        assert!(!policy.should_auto_approve(&RiskClass::WriteScoped, "not-json"));
    }

    // ── connector namespace resolution ────────────────────────────────────
    //
    // Connectors are never registered in `tool_registry` — only in the
    // connector registry — so without the handle every `github.create_issue`
    // call hit the unknown-tool fast-abort and no connector could ever run.

    async fn make_connector_registry() -> Arc<ConnectorRegistry> {
        let tmp = tempfile::TempDir::new().unwrap();
        let audit = Arc::new(agentos_audit::AuditLog::open(&tmp.path().join("audit.db")).unwrap());
        let passphrase = agentos_vault::ZeroizingString::new("test".into());
        let vault = agentos_vault::SecretsVault::initialize(
            &tmp.path().join("vault.db"),
            &passphrase,
            audit,
        )
        .unwrap();
        std::mem::forget(tmp);
        let registry = Arc::new(ConnectorRegistry::new(Arc::new(vault)));
        let manifest = agentos_connectors::ConnectorManifest {
            connector: agentos_connectors::ConnectorInfo {
                id: "github".into(),
                name: "GitHub".into(),
                version: "1.0.0".into(),
                description: "GitHub API".into(),
                base_url: "https://api.github.com".into(),
                auth: agentos_connectors::AuthConfig::None,
                rate_limit: None,
                max_response_bytes: 32768,
            },
            tools: vec![agentos_connectors::ConnectorToolDef {
                name: "create_issue".into(),
                description: "Create issue".into(),
                method: agentos_connectors::HttpMethod::Post,
                path: "/issues".into(),
                input_schema: None,
                response_map: None,
                query_params: vec![],
                body_fields: vec![],
            }],
        };
        registry.register(manifest).await.unwrap();
        registry
    }

    fn make_hook(
        mode: ApprovalMode,
        connectors: Option<Arc<ConnectorRegistry>>,
    ) -> Arc<ApprovalHook> {
        let resolver = ApprovalModeResolver::new(
            ApprovalConfig {
                mode,
                agent_overrides: Default::default(),
            },
            Arc::new(RwLock::new(AgentRegistry::new())),
        );
        ApprovalHook::with_connectors(
            AutoApprovePolicy::default_rules(),
            Arc::new(EscalationManager::new()),
            Arc::new(RwLock::new(ToolRegistry::new())),
            resolver,
            None,
            connectors,
        )
    }

    fn tool_pre(tool_name: &str) -> HookEvent {
        HookEvent::ToolPre {
            task_id: agentos_types::TaskID::new(),
            agent_id: AgentID::new(),
            tool_name: tool_name.to_string(),
            input_json: "{}".to_string(),
        }
    }

    #[tokio::test]
    async fn registered_connector_call_resolves_instead_of_aborting() {
        let hook = make_hook(ApprovalMode::Auto, Some(make_connector_registry().await));
        // Resolves to ExecCapable, which `auto` allows — the point is that it
        // reaches the mode matrix at all instead of fast-aborting.
        assert_eq!(
            hook.on_event(&tool_pre("github.create_issue")).await,
            HookResult::Continue
        );
    }

    #[tokio::test]
    async fn connector_class_is_fail_closed_exec_capable() {
        // Under `ask_always` a ReadonlyScoped class would be allowed; an
        // ExecCapable one must escalate. The escalation tag proves the class.
        let hook = make_hook(
            ApprovalMode::AskAlways,
            Some(make_connector_registry().await),
        );
        match hook.on_event(&tool_pre("github.create_issue")).await {
            HookResult::Abort(reason) => assert!(
                reason.starts_with("approval_pending:"),
                "expected escalation, got {reason}"
            ),
            other => panic!("expected escalation abort, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_names_still_fast_abort() {
        let hook = make_hook(ApprovalMode::Auto, Some(make_connector_registry().await));
        for name in [
            "ghost.create_issue",
            "hallucinated-tool",
            ".leading",
            "trailing.",
            // Registered connector, operation the connector does not expose.
            // `route` would forward it to the proxy and get ToolNotFound, so
            // escalating it parks the task for ~5 min per hallucinated name.
            "github.list_my_issues",
        ] {
            match hook.on_event(&tool_pre(name)).await {
                HookResult::Abort(reason) => {
                    assert!(reason.contains("not found"), "{name}: {reason}")
                }
                other => panic!("{name}: expected fast-abort, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn unknown_operation_on_known_connector_does_not_park_an_escalation() {
        // The failure this guards: under a prompting mode a hallucinated
        // operation inside a registered namespace used to resolve to
        // ExecCapable and create a blocking escalation, parking the task until
        // the ~5-min auto-deny. It must fast-abort like any unknown tool.
        let hook = make_hook(ApprovalMode::AskEdit, Some(make_connector_registry().await));
        match hook.on_event(&tool_pre("github.list_my_issues")).await {
            HookResult::Abort(reason) => {
                assert!(reason.contains("not found"), "{reason}");
                assert!(!reason.starts_with("approval_pending:"), "{reason}");
            }
            other => panic!("expected fast-abort, got {other:?}"),
        }
        // The real operation still resolves and escalates under the same mode.
        match hook.on_event(&tool_pre("github.create_issue")).await {
            HookResult::Abort(reason) => assert!(
                reason.starts_with("approval_pending:"),
                "expected escalation, got {reason}"
            ),
            other => panic!("expected escalation abort, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn connector_call_without_registry_handle_aborts() {
        let hook = make_hook(ApprovalMode::Auto, None);
        assert!(matches!(
            hook.on_event(&tool_pre("github.create_issue")).await,
            HookResult::Abort(_)
        ));
    }

    #[test]
    fn test_path_traversal_not_auto_approved() {
        let policy = AutoApprovePolicy::default_rules();
        // Path traversal via ".." should never be auto-approved even if it starts with /tmp/
        assert!(!policy
            .should_auto_approve(&RiskClass::WriteScoped, r#"{"path": "/tmp/../etc/passwd"}"#));
        // Embedded traversal
        assert!(!policy.should_auto_approve(
            &RiskClass::WriteScoped,
            r#"{"path": "/tmp/./../../root/.ssh/authorized_keys"}"#
        ));
    }
}
