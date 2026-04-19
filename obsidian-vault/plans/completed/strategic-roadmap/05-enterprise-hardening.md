---
title: "Phase 5: Enterprise Hardening"
tags:
  - strategy
  - enterprise
  - security
  - observability
  - phase-5
date: 2026-04-08
status: planned
effort: 2w
priority: high
---

# Phase 5: Enterprise Hardening

> Close the remaining gaps between AgentOS's current security model and what enterprises require for production deployment: dynamic permission adaptation, OpenTelemetry export, anomaly detection foundations, and compliance documentation.

---

## Why This Phase

Research finding: Enterprises demand more than static permissions. They need **dynamic permission adjustment** that adapts based on runtime behavior, **proactive analytics** that flag anomalies before breaches, and **observability exports** (OpenTelemetry) that feed into existing SIEM/monitoring stacks. These are table stakes for enterprise procurement.

AgentOS has the best security primitives in the agent OS space. This phase turns primitives into enterprise-grade features.

---

## Current → Target State

**Current:** CapabilityTokens are static (minted at agent registration, fixed permissions). Audit log is SQLite-only (no external export). No anomaly detection. No OpenTelemetry integration.

**Target:** Dynamic permission scoping based on runtime context, OpenTelemetry trace/metric export, anomaly scoring on audit events, and a compliance posture document.

---

## Detailed Subtasks

### 1. Dynamic Permission Scoping

Allow CapabilityTokens to have **context-dependent** permission rules:

```rust
// crates/agentos-capability/src/dynamic.rs

/// A rule that modifies permissions based on runtime context
#[derive(Serialize, Deserialize)]
pub struct DynamicPermissionRule {
    pub condition: PermissionCondition,
    pub grant: Vec<String>,     // Permissions to add when condition met
    pub revoke: Vec<String>,    // Permissions to remove when condition met
}

pub enum PermissionCondition {
    /// Time-of-day restriction (e.g., no file writes after hours)
    TimeWindow { start: NaiveTime, end: NaiveTime },
    /// Budget threshold (e.g., reduce permissions when >80% budget consumed)
    BudgetThreshold { max_percent: f64 },
    /// Task count (e.g., expand permissions after N successful tasks)
    TaskCountAbove { count: u64 },
    /// Escalation active (restrict while human review pending)
    EscalationPending,
}
```

**Integration point:** `PermissionSet::check()` in `crates/agentos-capability/` evaluates dynamic rules against current kernel state before returning allow/deny.

### 2. OpenTelemetry Export

Wire AgentOS's existing audit events and internal metrics to OpenTelemetry:

```rust
// crates/agentos-kernel/src/telemetry.rs

use opentelemetry::trace::{Tracer, SpanKind};
use opentelemetry::metrics::Meter;

pub struct TelemetryExporter {
    tracer: Box<dyn Tracer + Send + Sync>,
    meter: Meter,
}

impl TelemetryExporter {
    /// Emit a span for each task execution
    pub fn trace_task_execution(&self, task: &AgentTask) -> Span { ... }

    /// Record metrics: task latency, tool call count, cost, token usage
    pub fn record_task_metrics(&self, task: &AgentTask, result: &TaskResult) { ... }

    /// Export audit events as log records
    pub fn export_audit_event(&self, event: &AuditEvent) { ... }
}
```

**Config:**
```toml
# config/default.toml
[telemetry]
enabled = false
exporter = "otlp"           # otlp, jaeger, zipkin
endpoint = "http://localhost:4317"
service_name = "agentos"
```

**Dependencies:** Add `opentelemetry`, `opentelemetry-otlp`, `opentelemetry-sdk` to `agentos-kernel/Cargo.toml`.

### 3. Anomaly Scoring Foundation

Add a lightweight anomaly scorer that flags unusual patterns in audit events:

```rust
// crates/agentos-audit/src/anomaly.rs

pub struct AnomalyScorer {
    /// Rolling statistics per agent
    agent_stats: HashMap<AgentID, AgentBehaviorStats>,
}

pub struct AgentBehaviorStats {
    pub avg_tools_per_task: f64,
    pub avg_task_duration_ms: f64,
    pub permission_denial_rate: f64,
    pub last_updated: DateTime<Utc>,
}

impl AnomalyScorer {
    /// Score an event. Returns 0.0 (normal) to 1.0 (highly anomalous)
    pub fn score(&mut self, event: &AuditEvent) -> f64 {
        // Heuristics:
        // - Permission denials > 3x rolling average = high score
        // - Task duration > 5x average = elevated score
        // - Tool calls from unfamiliar agent = elevated score
        // - Multiple vault access attempts in short window = high score
    }

    /// Events scoring above threshold trigger a notification
    pub fn check_threshold(&self, score: f64, threshold: f64) -> Option<AnomalyAlert> { ... }
}
```

**Integration:** Wire into `AuditLog::log()` path — score each event, emit notification if above threshold.

### 4. Compliance Posture Document

Create a document mapping AgentOS features to common compliance frameworks:

**File:** `docs/compliance/security-posture.md`

| Requirement (NIST/SOC2/ISO27001) | AgentOS Feature | Evidence |
|----------------------------------|----------------|---------|
| Access control (AC-3) | CapabilityTokens | Every tool call validated |
| Audit logging (AU-2) | Append-only SQLite + HMAC chain | 83+ event types |
| Encryption at rest (SC-28) | AES-256-GCM vault | Argon2id KDF |
| Least privilege (AC-6) | Per-tool permission sets + deny entries | Token-scoped |
| System monitoring (SI-4) | Anomaly scorer + OpenTelemetry | Real-time scoring |
| Incident response (IR-4) | Escalation manager + notifications | SQLite-backed workflow |

### 5. RBAC Role Definitions

Define standard enterprise roles with pre-configured permission sets:

```rust
// crates/agentos-capability/src/roles.rs

pub enum EnterpriseRole {
    /// Full kernel access — can mint tokens, manage agents, read vault
    Admin,
    /// Can create agents and tasks, but cannot access vault or modify kernel config
    Operator,
    /// Can view dashboards and audit logs, but cannot execute tasks
    Auditor,
    /// Can run tasks with pre-assigned capability tokens only
    Agent,
    /// Read-only access to status and metrics
    Viewer,
}

impl EnterpriseRole {
    pub fn default_permissions(&self) -> PermissionSet { ... }
}
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-capability/src/dynamic.rs` (new) | Dynamic permission rules |
| `crates/agentos-capability/src/roles.rs` (new) | Enterprise RBAC roles |
| `crates/agentos-capability/src/lib.rs` | Re-export new modules |
| `crates/agentos-kernel/src/telemetry.rs` (new) | OpenTelemetry exporter |
| `crates/agentos-kernel/Cargo.toml` | Add opentelemetry deps |
| `crates/agentos-audit/src/anomaly.rs` (new) | Anomaly scorer |
| `crates/agentos-audit/src/log.rs` | Wire anomaly scoring |
| `config/default.toml` | Add `[telemetry]` section |
| `docs/compliance/security-posture.md` (new) | Compliance mapping |

---

## Dependencies

- **Requires:** Phase 1 (security hardening fixes), Phase 2 (MCP surface needs telemetry)
- **Blocks:** Nothing directly — enriches all other phases

---

## Test Plan

1. Dynamic permissions: create token with `BudgetThreshold` rule → exhaust 80% budget → verify permission revoked
2. Dynamic permissions: create token with `TimeWindow` rule → test inside/outside window
3. OpenTelemetry: start OTLP collector → run task → verify traces and metrics received
4. Anomaly scorer: feed 100 normal events → feed 1 anomalous event → verify score > threshold
5. RBAC: mint token for each role → verify correct permission grants and denials
6. `cargo build --workspace && cargo test --workspace && cargo clippy --workspace -- -D warnings`

---

## Verification

```bash
# Test dynamic permissions
cargo test -p agentos-capability -- dynamic

# Test anomaly scorer
cargo test -p agentos-audit -- anomaly

# Test RBAC roles
cargo test -p agentos-capability -- roles

# Full workspace check
cargo build --workspace && cargo test --workspace && cargo clippy --workspace -- -D warnings
```
