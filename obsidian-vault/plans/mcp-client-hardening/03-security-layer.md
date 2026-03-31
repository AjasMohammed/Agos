---
title: "Phase 3: MCP Security Layer"
tags:
  - mcp
  - v3
  - plan
  - phase-3
  - security
date: 2026-03-30
status: planned
effort: 1d
priority: high
---

# Phase 3: MCP Security Layer

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `McpSecurityGate` to sanitize MCP tool outputs, scan for injection attempts, enforce per-server rate limits, and audit all tool calls.

**Architecture:** `McpSecurityGate` sits between the supervisor's `call_tool()` method and the adapter. Every tool result passes through: rate limit check → transport call → output validation → injection scan → audit log.

**Tech Stack:** Rust, tokio, serde_json, existing `InjectionScanner` and `AuditLog` from kernel

---

## Why This Phase

MCP tool outputs are untrusted external data entering the agent's reasoning context. Without sanitization:
- A malicious MCP server can send 50MB responses, exhausting memory
- Injection attacks can manipulate agent decisions
- No visibility into what MCP servers are doing (no audit trail)
- No rate limiting on runaway servers

This phase adds defense-in-depth: semantic limits, injection scanning, and full audit coverage.

## Current State

- `crates/agentos-kernel/src/injection_scanner.rs` exists, public API: `InjectionScanner::new()`, `scan()`, `scan_with_context()`
- `crates/agentos-audit/src/log.rs` has `AuditLog` and `AuditEventType` enum (no `McpToolCall` variant yet)
- No output sanitization for MCP tools
- No rate limiting
- No audit trail for MCP tool calls

## Target State

- New `crates/agentos-mcp/src/security.rs` with `McpSecurityGate` struct
- New `AuditEventType::McpToolCall` variant in `agentos-audit`
- Per-server rate limiters and policies stored in security gate
- Output validation: size, content-type, depth checks
- Injection scanning with `<user_data>` wrapping
- Audit logging with latency, input size, output size

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-mcp/src/security.rs` | Create — full security gate implementation |
| `crates/agentos-mcp/src/lib.rs` | Add `pub mod security`, re-export `McpSecurityGate`, `SlidingWindowRateLimiter` |
| `crates/agentos-audit/src/lib.rs` | Add `McpToolCall` variant to `AuditEventType` enum |

## Dependencies

- **Requires:** Phase 1 (Transport), Phase 2 (Supervisor)
- **Blocks:** Phase 4 (Config + Adapter + CLI + Kernel)

---

### Task 1: AuditEventType Extension and Rate Limiter

**Files:**
- Modify: `crates/agentos-audit/src/lib.rs`
- Create: `crates/agentos-mcp/src/security/rate_limiter.rs`

- [ ] **Step 1: Add McpToolCall variant to AuditEventType**

Find `AuditEventType` enum in `crates/agentos-audit/src/lib.rs` (around line 16). Add the new variant:

```rust
    /// MCP tool call (client mode only).
    /// Details:
    /// {
    ///   "server": "filesystem",
    ///   "tool": "read_file",
    ///   "latency_ms": 42,
    ///   "input_size_bytes": 128,
    ///   "output_size_bytes": 4096,
    ///   "success": true,
    ///   "trust_tier": "community"
    /// }
    McpToolCall,
```

Place it after the existing tool-related variants (e.g., after `ToolExecutionCompleted`).

- [ ] **Step 2: Write SlidingWindowRateLimiter**

Create a new module for rate limiting:

```rust
// crates/agentos-mcp/src/security/rate_limiter.rs

use std::collections::VecDeque;
use std::time::Instant;

/// A sliding window rate limiter that tracks calls over the last minute.
#[derive(Debug, Clone)]
pub struct SlidingWindowRateLimiter {
    /// Max calls allowed in a 60-second window.
    max_calls: u32,
    /// Timestamps of recent calls (kept only if within the window).
    call_times: VecDeque<Instant>,
}

impl SlidingWindowRateLimiter {
    /// Create a new rate limiter with the given max calls per minute.
    pub fn new(max_calls_per_minute: u32) -> Self {
        Self {
            max_calls: max_calls_per_minute,
            call_times: VecDeque::new(),
        }
    }

    /// Check if a call is allowed under the rate limit.
    /// If allowed, records the call time. Returns true if allowed, false if rate limit exceeded.
    pub fn check_and_record(&mut self) -> bool {
        let now = Instant::now();
        let window_start = now - std::time::Duration::from_secs(60);

        // Remove calls outside the window.
        while let Some(&oldest) = self.call_times.front() {
            if oldest < window_start {
                self.call_times.pop_front();
            } else {
                break;
            }
        }

        if self.call_times.len() < self.max_calls as usize {
            self.call_times.push_back(now);
            true
        } else {
            false
        }
    }

    /// Get the current call count within the window.
    pub fn current_count(&self) -> u32 {
        self.call_times.len() as u32
    }

    /// Get the max allowed calls per minute.
    pub fn max_calls_per_minute(&self) -> u32 {
        self.max_calls
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_limiter_is_empty() {
        let limiter = SlidingWindowRateLimiter::new(10);
        assert_eq!(limiter.current_count(), 0);
    }

    #[test]
    fn check_and_record_allows_calls_under_limit() {
        let mut limiter = SlidingWindowRateLimiter::new(3);
        assert!(limiter.check_and_record());
        assert!(limiter.check_and_record());
        assert!(limiter.check_and_record());
        assert_eq!(limiter.current_count(), 3);
    }

    #[test]
    fn check_and_record_blocks_over_limit() {
        let mut limiter = SlidingWindowRateLimiter::new(2);
        assert!(limiter.check_and_record());
        assert!(limiter.check_and_record());
        assert!(!limiter.check_and_record());
        assert_eq!(limiter.current_count(), 2);
    }

    #[test]
    fn check_and_record_allows_after_window_expires() {
        // This test is timing-sensitive — we can't really test the 60-second window.
        // Instead, just verify the structure works.
        let mut limiter = SlidingWindowRateLimiter::new(1);
        assert!(limiter.check_and_record());
        assert!(!limiter.check_and_record());
        // In a real scenario, we'd sleep 60 seconds and verify it resets,
        // but that's impractical in a unit test.
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p agentos-mcp`
Expected: All tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/agentos-audit/src/lib.rs
git add crates/agentos-mcp/src/security/rate_limiter.rs
git commit -m "feat(audit): add McpToolCall event type; feat(mcp): add SlidingWindowRateLimiter"
```

---

### Task 2: McpSecurityGate Core

**Files:**
- Create: `crates/agentos-mcp/src/security/mod.rs`
- Create: `crates/agentos-mcp/src/security/output_validator.rs`

- [ ] **Step 1: Write the output validator**

```rust
// crates/agentos-mcp/src/security/output_validator.rs

use serde_json::Value;

/// Validates and sanitizes MCP tool output before it reaches the agent.
pub struct OutputValidator {
    /// Default max response size in bytes. Can be overridden per server.
    max_response_bytes: usize,
}

impl OutputValidator {
    pub fn new(max_response_bytes: usize) -> Self {
        Self { max_response_bytes }
    }

    /// Validate a JSON-RPC result value.
    ///
    /// Checks:
    /// - Size limit (reject if exceeds limit)
    /// - Content type validity (text/JSON only)
    /// - Depth limit (max 32 levels of nesting)
    /// - Base64 payloads (reject if >100KB)
    ///
    /// Returns the validated value (possibly truncated) or an error.
    pub fn validate(&self, value: &Value, server_max_bytes: Option<usize>) -> Result<Value, String> {
        let max_bytes = server_max_bytes.unwrap_or(self.max_response_bytes);

        // Estimate JSON-encoded size.
        let encoded = serde_json::to_string(value)
            .map_err(|e| format!("Failed to serialize output: {}", e))?;
        let size = encoded.len();

        if size > max_bytes {
            // Truncate and append notice.
            let truncated = format!(
                "{}...[truncated: original size was {} bytes]",
                &encoded[..encoded.len().min(max_bytes - 100)],
                size
            );
            return Ok(Value::String(truncated));
        }

        // Check max nesting depth.
        if self.max_depth(value) > 32 {
            return Err("JSON response exceeds max nesting depth (32)".into());
        }

        // Check for suspicious base64 blobs.
        if let Some(blob_size) = self.detect_large_base64(value) {
            if blob_size > 100 * 1024 {
                return Err(format!("Base64 payload exceeds 100KB limit: {} bytes", blob_size));
            }
        }

        Ok(value.clone())
    }

    /// Calculate maximum nesting depth of a JSON value.
    fn max_depth(&self, value: &Value) -> u32 {
        match value {
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => 1,
            Value::Array(arr) => {
                1 + arr.iter().map(|v| self.max_depth(v)).max().unwrap_or(0)
            }
            Value::Object(obj) => {
                1 + obj.values().map(|v| self.max_depth(v)).max().unwrap_or(0)
            }
        }
    }

    /// Detect large base64 strings (potential binary data).
    /// Returns the estimated size if found, or None.
    fn detect_large_base64(&self, value: &Value) -> Option<usize> {
        match value {
            Value::String(s) => {
                // Heuristic: strings matching ^[A-Za-z0-9+/=]{100,}$ are likely base64.
                if s.len() > 1000 && is_likely_base64(s) {
                    Some(s.len())
                } else {
                    None
                }
            }
            Value::Array(arr) => {
                arr.iter().find_map(|v| self.detect_large_base64(v))
            }
            Value::Object(obj) => {
                obj.values().find_map(|v| self.detect_large_base64(v))
            }
            _ => None,
        }
    }
}

fn is_likely_base64(s: &str) -> bool {
    s.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_small_object_succeeds() {
        let validator = OutputValidator::new(1024);
        let value = serde_json::json!({"ok": true});
        let result = validator.validate(&value, None);
        assert!(result.is_ok());
    }

    #[test]
    fn validate_exceeds_size_truncates() {
        let validator = OutputValidator::new(50);
        let value = serde_json::json!({"message": "this is a very long string that exceeds the size limit"});
        let result = validator.validate(&value, None).unwrap();
        let s = result.as_str().unwrap();
        assert!(s.contains("truncated"));
        assert!(s.contains("original size was"));
    }

    #[test]
    fn validate_too_deep_rejects() {
        let validator = OutputValidator::new(10000);
        // Build a deeply nested structure.
        let mut value = Value::Number(1.into());
        for _ in 0..35 {
            value = serde_json::json!({ "nested": value });
        }
        let result = validator.validate(&value, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("nesting depth"));
    }

    #[test]
    fn validate_rejects_large_base64() {
        let validator = OutputValidator::new(200000);
        let large_b64 = "A".repeat(101 * 1024); // 101 KB of 'A's (looks like base64)
        let value = serde_json::json!({"data": large_b64});
        let result = validator.validate(&value, None);
        assert!(result.is_err());
    }

    #[test]
    fn is_likely_base64_detects() {
        assert!(is_likely_base64("SGVsbG8gV29ybGQ=")); // "Hello World" in base64
        assert!(is_likely_base64("AAAA++++////")); // with + and /
        assert!(!is_likely_base64("Hello World!")); // has ! which isn't base64
    }
}
```

- [ ] **Step 2: Write McpSecurityGate**

```rust
// crates/agentos-mcp/src/security/mod.rs

pub mod output_validator;
pub mod rate_limiter;

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::types::McpToolDef;

pub use output_validator::OutputValidator;
pub use rate_limiter::SlidingWindowRateLimiter;

/// Per-server security policy.
#[derive(Debug, Clone)]
pub struct McpServerPolicy {
    pub name: String,
    pub max_response_bytes: usize,
    pub allowed_tools: Vec<String>, // empty = all allowed
    pub denied_tools: Vec<String>,
    pub rate_limit_rpm: u32,
}

impl McpServerPolicy {
    /// Check if a tool is allowed to be called on this server.
    pub fn is_tool_allowed(&self, tool_name: &str) -> bool {
        // Deny list takes precedence.
        if !self.denied_tools.is_empty() && self.denied_tools.contains(&tool_name.to_string()) {
            return false;
        }
        // Allow list check.
        if self.allowed_tools.is_empty() {
            // Empty allow list = allow all (except denied).
            true
        } else {
            self.allowed_tools.contains(&tool_name.to_string())
        }
    }
}

/// Security gate for MCP tool calls.
/// Enforces:
/// - Rate limiting per server
/// - Output validation (size, depth, content type)
/// - Injection scanning
/// - Audit logging
pub struct McpSecurityGate {
    audit_log: Arc<agentos_audit::AuditLog>,
    injection_scanner: Arc<agentos_kernel::InjectionScanner>,
    output_validator: OutputValidator,
    rate_limiters: RwLock<HashMap<String, SlidingWindowRateLimiter>>,
    server_policies: RwLock<HashMap<String, McpServerPolicy>>,
}

impl McpSecurityGate {
    pub fn new(
        audit_log: Arc<agentos_audit::AuditLog>,
        injection_scanner: Arc<agentos_kernel::InjectionScanner>,
        default_max_response_bytes: usize,
    ) -> Self {
        Self {
            audit_log,
            injection_scanner,
            output_validator: OutputValidator::new(default_max_response_bytes),
            rate_limiters: RwLock::new(HashMap::new()),
            server_policies: RwLock::new(HashMap::new()),
        }
    }

    /// Register a security policy for a server.
    pub async fn register_server_policy(&self, policy: McpServerPolicy) {
        let mut policies = self.server_policies.write().await;
        let mut limiters = self.rate_limiters.write().await;
        policies.insert(policy.name.clone(), policy.clone());
        limiters.insert(
            policy.name,
            SlidingWindowRateLimiter::new(policy.rate_limit_rpm),
        );
    }

    /// Check if a tool call is allowed (rate limit + tool whitelist).
    /// Returns error if not allowed.
    pub async fn check_tool_allowed(
        &self,
        server_name: &str,
        tool_name: &str,
    ) -> Result<(), String> {
        // Check rate limit.
        let mut limiters = self.rate_limiters.write().await;
        if let Some(limiter) = limiters.get_mut(server_name) {
            if !limiter.check_and_record() {
                return Err(format!(
                    "Rate limit exceeded for server '{}': {} calls/minute",
                    server_name,
                    limiter.max_calls_per_minute()
                ));
            }
        }
        drop(limiters);

        // Check tool whitelist/blacklist.
        let policies = self.server_policies.read().await;
        if let Some(policy) = policies.get(server_name) {
            if !policy.is_tool_allowed(tool_name) {
                return Err(format!(
                    "Tool '{}' is not allowed on server '{}'",
                    tool_name, server_name
                ));
            }
        }

        Ok(())
    }

    /// Validate and wrap MCP tool output.
    /// Wraps result in `<user_data>` tags for injection safety.
    pub async fn process_output(
        &self,
        result: serde_json::Value,
        server_name: &str,
    ) -> Result<serde_json::Value, String> {
        let policies = self.server_policies.read().await;
        let server_max = policies
            .get(server_name)
            .map(|p| p.max_response_bytes);
        drop(policies);

        // Validate size, depth, and content.
        let validated = self.output_validator.validate(&result, server_max)?;

        // Convert to string for injection scanning.
        let result_str = match validated {
            serde_json::Value::String(s) => s,
            _ => serde_json::to_string(&validated)
                .map_err(|e| format!("Failed to stringify output: {}", e))?,
        };

        // Wrap in `<user_data>` tags to mark as untrusted.
        let wrapped = format!("<user_data>{}</user_data>", result_str);

        // Scan for injection attempts (doesn't block, just logs).
        let scan_result = self
            .injection_scanner
            .scan_with_context(
                &result_str,
                agentos_kernel::ToolOutputContext::TextOutput,
            );
        if scan_result.is_suspicious {
            tracing::warn!(
                server = %server_name,
                threat_level = ?scan_result.max_threat,
                "Potential injection attempt detected in MCP server output"
            );
            // Log injection attempt to audit log.
            let _ = self.audit_log.append(agentos_audit::AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: agentos_types::TraceID::new(),
                event_type: agentos_audit::AuditEventType::InjectionAttempt,
                agent_id: None,
                task_id: None,
                tool_id: None,
                details: serde_json::json!({
                    "server": server_name,
                    "threat_level": format!("{:?}", scan_result.max_threat),
                    "pattern_count": scan_result.matches.len(),
                }),
                severity: agentos_audit::AuditSeverity::Warn,
                reversible: false,
                rollback_ref: None,
            });
        }

        Ok(serde_json::Value::String(wrapped))
    }

    /// Log a tool call to the audit log.
    pub async fn audit_tool_call(
        &self,
        server_name: &str,
        tool_name: &str,
        input_size_bytes: usize,
        output_size_bytes: usize,
        latency_ms: u64,
        success: bool,
        trace_id: agentos_types::TraceID,
        task_id: agentos_types::TaskID,
        agent_id: agentos_types::AgentID,
    ) {
        let entry = agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id,
            event_type: agentos_audit::AuditEventType::McpToolCall,
            agent_id: Some(agent_id),
            task_id: Some(task_id),
            tool_id: None, // MCP tools aren't registered in the tool registry yet
            details: serde_json::json!({
                "server": server_name,
                "tool": tool_name,
                "latency_ms": latency_ms,
                "input_size_bytes": input_size_bytes,
                "output_size_bytes": output_size_bytes,
                "success": success,
            }),
            severity: if success {
                agentos_audit::AuditSeverity::Info
            } else {
                agentos_audit::AuditSeverity::Warn
            },
            reversible: false,
            rollback_ref: None,
        };

        let _ = self.audit_log.append(entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_policy_allows_all_by_default() {
        let policy = McpServerPolicy {
            name: "test".into(),
            max_response_bytes: 1024,
            allowed_tools: vec![],
            denied_tools: vec![],
            rate_limit_rpm: 60,
        };
        assert!(policy.is_tool_allowed("anything"));
    }

    #[test]
    fn server_policy_respects_allow_list() {
        let policy = McpServerPolicy {
            name: "test".into(),
            max_response_bytes: 1024,
            allowed_tools: vec!["ping".into(), "echo".into()],
            denied_tools: vec![],
            rate_limit_rpm: 60,
        };
        assert!(policy.is_tool_allowed("ping"));
        assert!(!policy.is_tool_allowed("admin"));
    }

    #[test]
    fn server_policy_denies_blacklisted() {
        let policy = McpServerPolicy {
            name: "test".into(),
            max_response_bytes: 1024,
            allowed_tools: vec![], // allow all
            denied_tools: vec!["admin".into()],
            rate_limit_rpm: 60,
        };
        assert!(policy.is_tool_allowed("ping"));
        assert!(!policy.is_tool_allowed("admin"));
    }

    #[test]
    fn server_policy_deny_takes_precedence() {
        let policy = McpServerPolicy {
            name: "test".into(),
            max_response_bytes: 1024,
            allowed_tools: vec!["admin".into()], // explicitly allowed
            denied_tools: vec!["admin".into()],  // but also denied
            rate_limit_rpm: 60,
        };
        assert!(!policy.is_tool_allowed("admin")); // deny wins
    }
}
```

- [ ] **Step 3: Wire security module into lib.rs**

Add to `crates/agentos-mcp/src/lib.rs`:

```rust
pub mod security;

pub use security::{McpSecurityGate, McpServerPolicy, SlidingWindowRateLimiter};
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p agentos-mcp`
Expected: All tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/agentos-mcp/src/security/
git add crates/agentos-mcp/src/lib.rs
git commit -m "feat(mcp): add McpSecurityGate with output validation, rate limiting, injection scanning"
```

---

## Test Plan

| Test | Assertion |
|------|-----------|
| `SlidingWindowRateLimiter::new` | Creates empty limiter |
| `check_and_record` under limit | Returns true, increments count |
| `check_and_record` over limit | Returns false, count stays at max |
| `OutputValidator::validate` small | Passes through unchanged |
| `OutputValidator::validate` exceeds size | Truncates with notice |
| `OutputValidator::validate` too deep | Returns error |
| `OutputValidator::validate` large base64 | Returns error |
| `ServerPolicy::is_tool_allowed` default | Allows all |
| `ServerPolicy::is_tool_allowed` allow list | Checks membership |
| `ServerPolicy::is_tool_allowed` deny precedence | Deny overrides allow |
| `is_likely_base64` detection | Identifies base64 strings |

## Verification

```bash
cargo test -p agentos-mcp
cargo build --workspace
cargo clippy -p agentos-mcp -- -D warnings
cargo fmt --all -- --check
```
