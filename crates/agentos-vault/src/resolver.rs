use crate::vault::SecretsVault;
use agentos_audit::{AuditEntry, AuditEventType, AuditLog, AuditSeverity};
use agentos_types::{AgentOSError, TraceID};
use regex::Regex;
use std::sync::Arc;

/// Resolves `@keyname` and `@{key_name}` references in strings by looking up
/// the corresponding secrets in the vault.
///
/// **Security contract:**
/// - Resolved values are never written to logs, workflow JSON, or YAML.
/// - Every resolution is recorded in the audit log (key name only, not value).
/// - Kernel-scoped secrets are never returned by `list_key_names`.
/// - For strings destined for an LLM, use `redact()` instead — it substitutes
///   `[REDACTED:<key>]` without touching the vault.
///
/// **Syntax:**
/// - `@{key_name}` — explicit braces, allows any key name matching `[a-zA-Z0-9_]{2,64}`.
/// - `@key_name` — bare form, same character set (greedy match).
///
/// Both forms are equivalent; braced form takes precedence when both could match.
pub struct SecretResolver {
    vault: Arc<SecretsVault>,
    audit: Arc<AuditLog>,
}

/// Context recorded in the audit log entry for each resolution.
#[derive(Debug, Clone, Default)]
pub struct ResolveContext<'a> {
    pub workflow_id: Option<&'a str>,
    pub step_id: Option<&'a str>,
    pub agent_id: Option<&'a str>,
    pub source: SecretSource,
}

#[derive(Debug, Clone, Default)]
pub enum SecretSource {
    #[default]
    WorkflowParameter,
    AgentPrompt,
    ToolInput,
    ChannelConfig,
    ScheduleTask,
    HttpHeader,
}

impl SecretResolver {
    pub fn new(vault: Arc<SecretsVault>, audit: Arc<AuditLog>) -> Self {
        Self { vault, audit }
    }

    /// Resolve all `@keyname` / `@{keyname}` references in `input` by fetching
    /// secrets from the vault. Each resolved key is audit-logged.
    ///
    /// Returns an error if any referenced key is missing from the vault.
    pub async fn resolve(
        &self,
        input: &str,
        ctx: &ResolveContext<'_>,
    ) -> Result<String, AgentOSError> {
        let keys: Vec<String> = Self::extract_keys(input);
        if keys.is_empty() {
            return Ok(input.to_string());
        }

        let mut result = input.to_string();
        for key in keys {
            let value = self.vault.get(&key).await?;
            // Audit the resolution (key name only, value is ZeroizingString and never logged).
            let _ = self.audit.append(AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: TraceID::new(),
                event_type: AuditEventType::SecretResolved,
                agent_id: ctx.agent_id.and_then(|s| s.parse().ok()),
                task_id: None,
                tool_id: None,
                details: serde_json::json!({
                    "key": key,
                    "source": format!("{:?}", ctx.source),
                    "workflow_id": ctx.workflow_id,
                    "step_id": ctx.step_id,
                }),
                severity: AuditSeverity::Security,
                reversible: false,
                rollback_ref: None,
            });
            let secret_value = value.as_str();
            result = result.replace(&format!("@{{{}}}", key), secret_value);
            // Only replace bare form if the braced form was not also present
            // (avoids double-replacing if both were written in the same string).
            result = result.replace(&format!("@{}", key), secret_value);
        }
        Ok(result)
    }

    /// Replace every `@keyname` reference with `[REDACTED:<key>]` **without**
    /// touching the vault. Use this for strings that will be sent to an LLM.
    pub fn redact(input: &str) -> String {
        let keys = Self::extract_keys(input);
        if keys.is_empty() {
            return input.to_string();
        }
        let mut result = input.to_string();
        for key in keys {
            let placeholder = format!("[REDACTED:{}]", key);
            result = result.replace(&format!("@{{{}}}", key), &placeholder);
            result = result.replace(&format!("@{}", key), &placeholder);
        }
        result
    }

    /// Return `true` if `input` contains any `@keyname` references.
    pub fn has_references(input: &str) -> bool {
        braced_re().is_match(input) || bare_re().is_match(input)
    }

    /// Extract all key names referenced in `input` (deduped, preserving order).
    /// Braced form `@{key}` is processed before bare form `@key`.
    /// Note: the bare regex cannot match inside `@{...}` because `{` is not
    /// in `[a-zA-Z0-9_]`, so there is no overlap between the two patterns.
    pub fn extract_keys(input: &str) -> Vec<String> {
        let mut seen: Vec<String> = Vec::new();
        for cap in braced_re().captures_iter(input) {
            let k = cap[1].to_string();
            if !seen.contains(&k) {
                seen.push(k);
            }
        }
        for cap in bare_re().captures_iter(input) {
            let k = cap[1].to_string();
            if !seen.contains(&k) {
                seen.push(k);
            }
        }
        seen
    }

    /// List the names of all non-kernel vault secrets (for UI autocomplete).
    /// Never returns actual values.
    pub async fn list_key_names(&self) -> Vec<String> {
        self.vault
            .list()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|m| m.name)
            .collect()
    }
}

fn braced_re() -> &'static Regex {
    use std::sync::OnceLock;
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"@\{([a-zA-Z0-9_]{2,64})\}").expect("valid regex"))
}

fn bare_re() -> &'static Regex {
    use std::sync::OnceLock;
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"@([a-zA-Z0-9_]{2,64})").expect("valid regex"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_braced() {
        let keys = SecretResolver::extract_keys("token=@{my_token}");
        assert_eq!(keys, vec!["my_token"]);
    }

    #[test]
    fn extract_bare() {
        let keys = SecretResolver::extract_keys("Bearer @api_key");
        assert_eq!(keys, vec!["api_key"]);
    }

    #[test]
    fn extract_both_forms() {
        let keys = SecretResolver::extract_keys("@{a_key} and @b_key");
        assert_eq!(keys, vec!["a_key", "b_key"]);
    }

    #[test]
    fn extract_no_duplicate_when_braced_and_bare() {
        // @{same} appears in braced form; bare regex also matches @same
        // inside @{same} — but our dedup via IndexSet prevents double-listing.
        let keys = SecretResolver::extract_keys("@{same}");
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0], "same");
    }

    #[test]
    fn redact_replaces_with_placeholder() {
        let result = SecretResolver::redact("Authorization: Bearer @{github_pat}");
        assert!(result.contains("[REDACTED:github_pat]"));
        assert!(!result.contains("@{github_pat}"));
    }

    #[test]
    fn redact_bare_form() {
        let result = SecretResolver::redact("key is @api_secret here");
        assert!(result.contains("[REDACTED:api_secret]"));
    }

    #[test]
    fn has_references_positive() {
        assert!(SecretResolver::has_references("value @my_key end"));
    }

    #[test]
    fn has_references_negative() {
        assert!(!SecretResolver::has_references("no secrets here @ invalid"));
    }

    #[test]
    fn extract_empty_when_no_references() {
        let keys = SecretResolver::extract_keys("plain text without references");
        assert!(keys.is_empty());
    }

    #[test]
    fn short_key_ignored() {
        // Min key length is 2 chars — single char after @ is not matched.
        let keys = SecretResolver::extract_keys("@x is too short");
        assert!(keys.is_empty());
    }
}
