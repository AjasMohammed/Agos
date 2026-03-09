use metrics::{counter, gauge, histogram};

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
pub fn record_inference(provider: &str, model: &str, input_tokens: u64, output_tokens: u64, latency_ms: u64) {
    counter!("agentos_inference_total", "provider" => provider.to_string(), "model" => model.to_string()).increment(1);
    counter!("agentos_tokens_input_total", "provider" => provider.to_string()).increment(input_tokens);
    counter!("agentos_tokens_output_total", "provider" => provider.to_string()).increment(output_tokens);
    histogram!("agentos_inference_latency_ms", "provider" => provider.to_string()).record(latency_ms as f64);
}

/// Record a tool execution.
pub fn record_tool_execution(tool_name: &str, duration_ms: u64, success: bool) {
    counter!("agentos_tool_executions_total", "tool" => tool_name.to_string(), "success" => success.to_string()).increment(1);
    histogram!("agentos_tool_duration_ms", "tool" => tool_name.to_string()).record(duration_ms as f64);
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
