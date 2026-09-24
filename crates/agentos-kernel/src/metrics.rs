use metrics::{counter, gauge, histogram};
use std::sync::atomic::{AtomicU64, Ordering};

static RETRIEVAL_REFRESH_TOTAL: AtomicU64 = AtomicU64::new(0);
static RETRIEVAL_REUSE_TOTAL: AtomicU64 = AtomicU64::new(0);

// ── Event channel metrics ──────────────────────────────────────────────────
// Updated via `record_event_*` helpers and readable via `event_metrics_snapshot`.
// Prometheus counters are also emitted so the health endpoint picks them up.

static EVENTS_EMITTED_TOTAL: AtomicU64 = AtomicU64::new(0);
static EVENTS_DROPPED_TOTAL: AtomicU64 = AtomicU64::new(0);
static EVENTS_PROCESSED_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Record a task being added to the queue.
pub fn record_task_queued() {
    counter!("agentos_tasks_queued_total").increment(1);
    gauge!("agentos_task_queue_depth").increment(1.0);
}

/// Record a task completing (success or failure).
pub fn record_task_completed(duration_ms: u64, success: bool) {
    gauge!("agentos_task_queue_depth").decrement(1.0);
    if success {
        counter!("agentos_tasks_completed_total").increment(1);
    } else {
        counter!("agentos_tasks_failed_total").increment(1);
    }
    histogram!("agentos_task_duration_ms").record(duration_ms as f64);
}

/// Record an LLM inference call.
pub fn record_inference(
    provider: &str,
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
    latency_ms: u64,
) {
    counter!("agentos_inference_total", "provider" => provider.to_string(), "model" => model.to_string()).increment(1);
    counter!("agentos_tokens_input_total", "provider" => provider.to_string())
        .increment(input_tokens);
    counter!("agentos_tokens_output_total", "provider" => provider.to_string())
        .increment(output_tokens);
    histogram!("agentos_inference_latency_ms", "provider" => provider.to_string())
        .record(latency_ms as f64);
}

/// Record a tool execution.
pub fn record_tool_execution(tool_name: &str, duration_ms: u64, success: bool) {
    counter!("agentos_tool_executions_total", "tool" => tool_name.to_string(), "success" => success.to_string()).increment(1);
    histogram!("agentos_tool_duration_ms", "tool" => tool_name.to_string())
        .record(duration_ms as f64);
}

/// Record an agent connection event.
pub fn record_agent_connected() {
    gauge!("agentos_connected_agents").increment(1.0);
}

/// Record an agent disconnection.
pub fn record_agent_disconnected() {
    gauge!("agentos_connected_agents").decrement(1.0);
}

/// Record a rate-limited request.
pub fn record_rate_limited() {
    counter!("agentos_rate_limited_total").increment(1);
}

/// Record whether retrieval context was refreshed or reused this iteration.
pub fn record_retrieval_refresh_decision(refreshed: bool) {
    if refreshed {
        RETRIEVAL_REFRESH_TOTAL.fetch_add(1, Ordering::Relaxed);
        counter!("agentos_retrieval_refresh_total").increment(1);
    } else {
        RETRIEVAL_REUSE_TOTAL.fetch_add(1, Ordering::Relaxed);
        counter!("agentos_retrieval_reuse_total").increment(1);
    }
}

/// Record retrieval refresh performance and output size.
pub fn record_retrieval_refresh(duration_ms: u64, knowledge_blocks: usize) {
    histogram!("agentos_retrieval_refresh_latency_ms").record(duration_ms as f64);
    histogram!("agentos_retrieval_knowledge_blocks").record(knowledge_blocks as f64);
}

pub fn retrieval_refresh_snapshot() -> (u64, u64) {
    (
        RETRIEVAL_REFRESH_TOTAL.load(Ordering::Relaxed),
        RETRIEVAL_REUSE_TOTAL.load(Ordering::Relaxed),
    )
}

/// Record one event entering the dispatch channel.
pub fn record_event_emitted() {
    EVENTS_EMITTED_TOTAL.fetch_add(1, Ordering::Relaxed);
    counter!("agentos_events_emitted_total").increment(1);
}

/// Record one event dropped because the dispatch channel was full.
pub fn record_event_dropped() {
    EVENTS_DROPPED_TOTAL.fetch_add(1, Ordering::Relaxed);
    counter!("agentos_events_dropped_total").increment(1);
}

/// Record one event successfully consumed by the EventDispatcher task.
pub fn record_event_processed() {
    EVENTS_PROCESSED_TOTAL.fetch_add(1, Ordering::Relaxed);
    counter!("agentos_events_processed_total").increment(1);
}

/// Return a point-in-time snapshot of event channel counters:
/// `(emitted, dropped, processed)`.
pub fn event_metrics_snapshot() -> (u64, u64, u64) {
    (
        EVENTS_EMITTED_TOTAL.load(Ordering::Relaxed),
        EVENTS_DROPPED_TOTAL.load(Ordering::Relaxed),
        EVENTS_PROCESSED_TOTAL.load(Ordering::Relaxed),
    )
}

// ── KMC (Kernel Mediated Capabilities) metrics ────────────────────────────

/// Record a capability request dispatched to a provider.
pub fn record_capability_request(domain: &str, action: &str) {
    counter!("agentos_capability_requests_total", "domain" => domain.to_string(), "action" => action.to_string()).increment(1);
}

/// Record a capability execution that succeeded.
pub fn record_capability_success(domain: &str, action: &str) {
    counter!("agentos_capability_successes_total", "domain" => domain.to_string(), "action" => action.to_string()).increment(1);
}

/// Record a capability execution that failed.
pub fn record_capability_failure(domain: &str, action: &str) {
    counter!("agentos_capability_failures_total", "domain" => domain.to_string(), "action" => action.to_string()).increment(1);
}

// ── SLI surface ────────────────────────────────────────────────────────────
// These names are the contract the observability docs and later phases use.
// Histograms must end in `_ms` or `health.rs` renders them as broken summaries.

/// One retention sweep finished (Phase 05).
pub fn record_retention_sweep(policy: &'static str, deleted: u64, duration_ms: u64) {
    counter!("agentos_retention_sweeps_total", "policy" => policy).increment(1);
    counter!("agentos_retention_swept_total", "policy" => policy).increment(deleted);
    histogram!("agentos_retention_sweep_duration_ms", "policy" => policy)
        .record(duration_ms as f64);
    gauge!("agentos_retention_last_run_timestamp_seconds", "policy" => policy)
        .set(chrono::Utc::now().timestamp() as f64);
}

/// A retention policy failed. Alert on any increase: a policy that errors is a
/// policy that is not reclaiming.
pub fn record_retention_error(policy: &'static str) {
    counter!("agentos_retention_errors_total", "policy" => policy).increment(1);
}

/// Classification of an LLM response (Phase 08). `class` is one of
/// `usable` / `tool_only` / `empty` / `malformed` / `truncated`.
pub fn record_llm_response_class(provider: &str, model: &str, class: &'static str) {
    counter!(
        "agentos_llm_response_class_total",
        "provider" => provider.to_string(),
        "model" => model.to_string(),
        "class" => class
    )
    .increment(1);
}

/// A tool payload was coerced into schema bounds rather than rejected (Phase 09).
pub fn record_tool_payload_coerced(tool: &str) {
    counter!("agentos_tool_payload_coerced_total", "tool" => tool.to_string()).increment(1);
}

/// Whether an iteration reused the previous prompt prefix (Phase 09). A low
/// stable ratio means provider-side prompt caching can never engage.
pub fn record_prompt_prefix(stable: bool) {
    counter!("agentos_prompt_prefix_total", "stable" => stable.to_string()).increment(1);
}

/// The audit log rejected an append. This must stay at zero — a dropped audit
/// entry is an integrity failure, not a performance blip.
pub fn record_audit_append_failure() {
    counter!("agentos_audit_append_failures_total").increment(1);
}

/// Every emitted tracing event, by level, so error rate is a metric rather
/// than a grep over a multi-megabyte log file.
pub fn record_log_event(level: &'static str) {
    counter!("agentos_log_events_total", "level" => level).increment(1);
}

/// Current disk headroom and pressure level. `agentos_resource_pressure_level`
/// is 0 = ok, 1 = warn, 2 = critical — alert on `> 0`.
pub fn record_resource_headroom(
    free_bytes: u64,
    free_inodes: u64,
    level: agentos_types::PressureLevel,
) {
    gauge!("agentos_resource_disk_free_bytes").set(free_bytes as f64);
    gauge!("agentos_resource_disk_free_inodes").set(free_inodes as f64);
    gauge!("agentos_resource_pressure_level").set(level as u8 as f64);
}
