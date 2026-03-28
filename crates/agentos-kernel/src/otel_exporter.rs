use crate::config::OtelConfig;

#[cfg(feature = "otel")]
use opentelemetry::trace::TraceContextExt;

#[derive(Clone)]
pub struct OtelExporter {
    #[cfg(feature = "otel")]
    inner: std::sync::Arc<OtelExporterInner>,
}

#[cfg(feature = "otel")]
struct OtelExporterInner {
    tracer: opentelemetry_sdk::trace::Tracer,
    #[allow(dead_code)]
    tracer_provider: opentelemetry_sdk::trace::SdkTracerProvider,
    #[allow(dead_code)]
    meter_provider: opentelemetry_sdk::metrics::SdkMeterProvider,
    task_duration_ms: opentelemetry::metrics::Histogram<f64>,
    task_cost_usd: opentelemetry::metrics::Counter<f64>,
    task_tokens_input: opentelemetry::metrics::Counter<u64>,
    task_tokens_output: opentelemetry::metrics::Counter<u64>,
    tool_call_duration_ms: opentelemetry::metrics::Histogram<f64>,
    llm_request_duration_ms: opentelemetry::metrics::Histogram<f64>,
    active_tasks: opentelemetry::metrics::UpDownCounter<i64>,
    health_cpu_percent: opentelemetry::metrics::Histogram<f64>,
    health_memory_percent: opentelemetry::metrics::Histogram<f64>,
    health_disk_percent: opentelemetry::metrics::Histogram<f64>,
    shutdown: std::sync::atomic::AtomicBool,
    scrub_tool_inputs: bool,
    scrub_tool_outputs: bool,
}

#[cfg(feature = "otel")]
impl OtelExporterInner {
    fn noop() -> anyhow::Result<Self> {
        use opentelemetry::metrics::MeterProvider as _;
        use opentelemetry::trace::TracerProvider as _;
        use opentelemetry_sdk::Resource;

        let resource = Resource::builder()
            .with_service_name("agentos")
            .with_attribute(opentelemetry::KeyValue::new(
                "service.version",
                env!("CARGO_PKG_VERSION"),
            ))
            .build();
        let tracer_provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
            .with_resource(resource.clone())
            .build();
        let tracer = tracer_provider.tracer("agentos-kernel");
        let meter_provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder()
            .with_resource(resource)
            .build();
        let meter = meter_provider.meter("agentos-kernel");

        Ok(Self {
            tracer,
            tracer_provider,
            meter_provider,
            task_duration_ms: meter.f64_histogram("agentos.task.duration_ms").build(),
            task_cost_usd: meter.f64_counter("agentos.task.cost_usd").build(),
            task_tokens_input: meter.u64_counter("agentos.task.tokens.input").build(),
            task_tokens_output: meter.u64_counter("agentos.task.tokens.output").build(),
            tool_call_duration_ms: meter.f64_histogram("agentos.tool.call.duration_ms").build(),
            llm_request_duration_ms: meter
                .f64_histogram("agentos.llm.request.duration_ms")
                .build(),
            active_tasks: meter
                .i64_up_down_counter("agentos.agent.active_tasks")
                .build(),
            health_cpu_percent: meter.f64_histogram("agentos.health.cpu.percent").build(),
            health_memory_percent: meter.f64_histogram("agentos.health.memory.percent").build(),
            health_disk_percent: meter.f64_histogram("agentos.health.disk.percent").build(),
            shutdown: std::sync::atomic::AtomicBool::new(false),
            scrub_tool_inputs: true,
            scrub_tool_outputs: true,
        })
    }
}

#[derive(Default)]
pub struct OtelSpan {
    #[cfg(feature = "otel")]
    context: Option<opentelemetry::Context>,
    #[cfg(feature = "otel")]
    ended: std::sync::atomic::AtomicBool,
}

#[allow(clippy::derivable_impls)]
impl Default for OtelExporter {
    fn default() -> Self {
        #[cfg(feature = "otel")]
        {
            // Return a noop exporter rather than panicking — this is safe for tests
            // and for the `enabled = false` config path.
            Self {
                inner: std::sync::Arc::new(
                    OtelExporterInner::noop().expect("noop OtelExporter creation should not fail"),
                ),
            }
        }
        #[cfg(not(feature = "otel"))]
        {
            Self {}
        }
    }
}

impl OtelExporter {
    #[cfg(feature = "otel")]
    pub fn new(config: &OtelConfig) -> anyhow::Result<Self> {
        use opentelemetry::metrics::MeterProvider as _;
        use opentelemetry::trace::TracerProvider as _;
        use opentelemetry_otlp::WithExportConfig as _;
        use opentelemetry_sdk::metrics::PeriodicReader;
        use opentelemetry_sdk::trace::Sampler;
        use opentelemetry_sdk::Resource;

        let resource = Resource::builder()
            .with_service_name(config.service_name.clone())
            .with_attribute(opentelemetry::KeyValue::new(
                "service.version",
                env!("CARGO_PKG_VERSION"),
            ))
            .build();

        let span_exporter = match config.protocol {
            crate::config::OtelProtocol::Grpc => opentelemetry_otlp::SpanExporter::builder()
                .with_tonic()
                .with_endpoint(config.endpoint.clone())
                .build()?,
            crate::config::OtelProtocol::Http => opentelemetry_otlp::SpanExporter::builder()
                .with_http()
                .with_endpoint(config.endpoint.clone())
                .build()?,
        };

        let tracer_provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
            .with_batch_exporter(span_exporter)
            .with_sampler(Sampler::TraceIdRatioBased(config.sample_rate))
            .with_resource(resource.clone())
            .build();
        let tracer = tracer_provider.tracer("agentos-kernel");

        let metric_exporter = match config.protocol {
            crate::config::OtelProtocol::Grpc => opentelemetry_otlp::MetricExporter::builder()
                .with_tonic()
                .with_endpoint(config.endpoint.clone())
                .build()?,
            crate::config::OtelProtocol::Http => opentelemetry_otlp::MetricExporter::builder()
                .with_http()
                .with_endpoint(config.endpoint.clone())
                .build()?,
        };

        let reader = PeriodicReader::builder(metric_exporter).build();
        let meter_provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder()
            .with_reader(reader)
            .with_resource(resource)
            .build();
        let meter = meter_provider.meter("agentos-kernel");

        let inner = OtelExporterInner {
            tracer,
            tracer_provider,
            meter_provider,
            task_duration_ms: meter
                .f64_histogram("agentos.task.duration_ms")
                .with_description("Task duration in milliseconds")
                .build(),
            task_cost_usd: meter
                .f64_counter("agentos.task.cost_usd")
                .with_description("Cumulative task cost in USD")
                .build(),
            task_tokens_input: meter
                .u64_counter("agentos.task.tokens.input")
                .with_description("Input tokens consumed by task execution")
                .build(),
            task_tokens_output: meter
                .u64_counter("agentos.task.tokens.output")
                .with_description("Output tokens consumed by task execution")
                .build(),
            tool_call_duration_ms: meter
                .f64_histogram("agentos.tool.call.duration_ms")
                .with_description("Tool call duration in milliseconds")
                .build(),
            llm_request_duration_ms: meter
                .f64_histogram("agentos.llm.request.duration_ms")
                .with_description("LLM request duration in milliseconds")
                .build(),
            active_tasks: meter
                .i64_up_down_counter("agentos.agent.active_tasks")
                .with_description("Active tasks currently running in the kernel")
                .build(),
            health_cpu_percent: meter
                .f64_histogram("agentos.health.cpu.percent")
                .with_description("Observed CPU usage percentage")
                .build(),
            health_memory_percent: meter
                .f64_histogram("agentos.health.memory.percent")
                .with_description("Observed memory usage percentage")
                .build(),
            health_disk_percent: meter
                .f64_histogram("agentos.health.disk.percent")
                .with_description("Observed disk usage percentage")
                .build(),
            shutdown: std::sync::atomic::AtomicBool::new(false),
            scrub_tool_inputs: config.scrub_tool_inputs,
            scrub_tool_outputs: config.scrub_tool_outputs,
        };

        Ok(Self {
            inner: std::sync::Arc::new(inner),
        })
    }

    #[cfg(not(feature = "otel"))]
    pub fn new(_config: &OtelConfig) -> anyhow::Result<Self> {
        Ok(Self::disabled())
    }

    pub fn disabled() -> Self {
        Self::default()
    }

    pub fn from_config(config: &OtelConfig) -> anyhow::Result<Self> {
        if !config.enabled {
            #[cfg(feature = "otel")]
            {
                return Ok(Self {
                    inner: std::sync::Arc::new(OtelExporterInner::noop()?),
                });
            }
            #[cfg(not(feature = "otel"))]
            {
                return Ok(Self::disabled());
            }
        }

        #[cfg(feature = "otel")]
        {
            Self::new(config)
        }
        #[cfg(not(feature = "otel"))]
        {
            tracing::warn!(
                "OTEL requested in config, but agentos-kernel was built without the 'otel' feature"
            );
            Ok(Self::disabled())
        }
    }

    pub fn start_task_span(&self, task_id: &str, agent_id: &str, model: &str) -> OtelSpan {
        #[cfg(feature = "otel")]
        {
            use opentelemetry::trace::Tracer as _;

            let span = self.inner.tracer.start("task.run");
            let cx = opentelemetry::Context::current_with_span(span);
            let wrapper = OtelSpan {
                context: Some(cx),
                ended: std::sync::atomic::AtomicBool::new(false),
            };
            wrapper.set_string_attribute("task.id", task_id);
            wrapper.set_string_attribute("agent.id", agent_id);
            wrapper.set_string_attribute("llm.model", model);
            wrapper
        }
        #[cfg(not(feature = "otel"))]
        {
            let _ = (task_id, agent_id, model);
            OtelSpan::default()
        }
    }

    pub fn start_iteration_span(&self, parent: &OtelSpan, iteration: u32, model: &str) -> OtelSpan {
        #[cfg(feature = "otel")]
        {
            let wrapper = self.start_span_from_context(parent.parent_context(), "task.iteration");
            wrapper.set_i64_attribute("task.iteration", iteration as i64);
            wrapper.set_string_attribute("llm.model", model);
            wrapper
        }
        #[cfg(not(feature = "otel"))]
        {
            let _ = (parent, iteration, model);
            OtelSpan::default()
        }
    }

    pub fn start_tool_span(&self, parent: &OtelSpan, tool_name: &str) -> OtelSpan {
        #[cfg(feature = "otel")]
        {
            let wrapper = self.start_span_from_context(parent.parent_context(), "tool.call");
            wrapper.set_string_attribute("tool.name", tool_name);
            wrapper
        }
        #[cfg(not(feature = "otel"))]
        {
            let _ = (parent, tool_name);
            OtelSpan::default()
        }
    }

    #[cfg(feature = "otel")]
    pub fn start_tool_span_from_context(
        &self,
        parent_context: Option<opentelemetry::Context>,
        tool_name: &str,
    ) -> OtelSpan {
        let wrapper = self.start_span_from_context(parent_context, "tool.call");
        wrapper.set_string_attribute("tool.name", tool_name);
        wrapper
    }

    #[cfg(feature = "otel")]
    fn start_span_from_context(
        &self,
        parent_context: Option<opentelemetry::Context>,
        name: &'static str,
    ) -> OtelSpan {
        #[cfg(feature = "otel")]
        {
            use opentelemetry::trace::Tracer as _;

            let span = if let Some(parent_cx) = parent_context.as_ref() {
                self.inner.tracer.start_with_context(name, parent_cx)
            } else {
                self.inner.tracer.start(name)
            };
            let cx = opentelemetry::Context::current_with_span(span);
            OtelSpan {
                context: Some(cx),
                ended: std::sync::atomic::AtomicBool::new(false),
            }
        }
    }

    pub fn record_cost(
        &self,
        agent_id: &str,
        model: &str,
        cost_usd: f64,
        input_tokens: u64,
        output_tokens: u64,
    ) {
        #[cfg(feature = "otel")]
        {
            let attrs = [
                opentelemetry::KeyValue::new("agent.id", agent_id.to_string()),
                opentelemetry::KeyValue::new("llm.model", model.to_string()),
            ];
            self.inner.task_cost_usd.add(cost_usd.max(0.0), &attrs);
            self.inner.task_tokens_input.add(input_tokens, &attrs);
            self.inner.task_tokens_output.add(output_tokens, &attrs);
        }
        #[cfg(not(feature = "otel"))]
        {
            let _ = (agent_id, model, cost_usd, input_tokens, output_tokens);
        }
    }

    pub fn record_llm_request(
        &self,
        agent_id: &str,
        provider: &str,
        model: &str,
        duration_ms: u64,
    ) {
        #[cfg(feature = "otel")]
        {
            let attrs = [
                opentelemetry::KeyValue::new("agent.id", agent_id.to_string()),
                opentelemetry::KeyValue::new("llm.provider", provider.to_string()),
                opentelemetry::KeyValue::new("llm.model", model.to_string()),
            ];
            self.inner
                .llm_request_duration_ms
                .record(duration_ms as f64, &attrs);
        }
        #[cfg(not(feature = "otel"))]
        {
            let _ = (agent_id, provider, model, duration_ms);
        }
    }

    pub fn record_tool_metric(
        &self,
        agent_id: &str,
        tool_name: &str,
        duration_ms: u64,
        success: bool,
    ) {
        #[cfg(feature = "otel")]
        {
            let attrs = [
                opentelemetry::KeyValue::new("agent.id", agent_id.to_string()),
                opentelemetry::KeyValue::new("tool.name", tool_name.to_string()),
                opentelemetry::KeyValue::new("tool.success", success),
            ];
            self.inner
                .tool_call_duration_ms
                .record(duration_ms as f64, &attrs);
        }
        #[cfg(not(feature = "otel"))]
        {
            let _ = (agent_id, tool_name, duration_ms, success);
        }
    }

    pub fn record_task_metric(&self, agent_id: &str, status: &str, duration_ms: u64) {
        #[cfg(feature = "otel")]
        {
            let attrs = [
                opentelemetry::KeyValue::new("agent.id", agent_id.to_string()),
                opentelemetry::KeyValue::new("task.status", status.to_string()),
            ];
            self.inner
                .task_duration_ms
                .record(duration_ms as f64, &attrs);
        }
        #[cfg(not(feature = "otel"))]
        {
            let _ = (agent_id, status, duration_ms);
        }
    }

    pub fn adjust_active_tasks(&self, delta: i64) {
        #[cfg(feature = "otel")]
        {
            self.inner.active_tasks.add(delta, &[]);
        }
        #[cfg(not(feature = "otel"))]
        {
            let _ = delta;
        }
    }

    pub fn record_health_snapshot(
        &self,
        cpu_percent: Option<f64>,
        memory_percent: Option<f64>,
        disk_percent: Option<f64>,
    ) {
        #[cfg(feature = "otel")]
        {
            if let Some(cpu_percent) = cpu_percent {
                self.inner.health_cpu_percent.record(cpu_percent, &[]);
            }
            if let Some(memory_percent) = memory_percent {
                self.inner.health_memory_percent.record(memory_percent, &[]);
            }
            if let Some(disk_percent) = disk_percent {
                self.inner.health_disk_percent.record(disk_percent, &[]);
            }
        }
        #[cfg(not(feature = "otel"))]
        {
            let _ = (cpu_percent, memory_percent, disk_percent);
        }
    }

    /// Returns true if tool input data should be scrubbed from OTel spans
    /// (defaults to true — prevents secrets leaking to observability backends).
    pub fn should_scrub_tool_inputs(&self) -> bool {
        #[cfg(feature = "otel")]
        {
            self.inner.scrub_tool_inputs
        }
        #[cfg(not(feature = "otel"))]
        {
            true
        }
    }

    /// Returns true if tool output data should be scrubbed from OTel spans
    /// (defaults to true — prevents secrets leaking to observability backends).
    pub fn should_scrub_tool_outputs(&self) -> bool {
        #[cfg(feature = "otel")]
        {
            self.inner.scrub_tool_outputs
        }
        #[cfg(not(feature = "otel"))]
        {
            true
        }
    }

    pub fn shutdown(&self) {
        #[cfg(feature = "otel")]
        {
            if self
                .inner
                .shutdown
                .swap(true, std::sync::atomic::Ordering::AcqRel)
            {
                return;
            }

            if let Err(err) = self.inner.tracer_provider.force_flush() {
                tracing::warn!(error = %err, "Failed to flush OTEL tracer provider");
            }
            if let Err(err) = self.inner.meter_provider.force_flush() {
                tracing::warn!(error = %err, "Failed to flush OTEL meter provider");
            }
            if let Err(err) = self.inner.tracer_provider.shutdown() {
                tracing::warn!(error = %err, "Failed to shut down OTEL tracer provider");
            }
            if let Err(err) = self.inner.meter_provider.shutdown() {
                tracing::warn!(error = %err, "Failed to shut down OTEL meter provider");
            }
        }
    }
}

impl OtelSpan {
    #[cfg(feature = "otel")]
    pub fn parent_context(&self) -> Option<opentelemetry::Context> {
        self.context.clone()
    }

    pub fn set_string_attribute(&self, key: &'static str, value: impl Into<String>) {
        #[cfg(feature = "otel")]
        {
            if let Some(cx) = &self.context {
                cx.span()
                    .set_attribute(opentelemetry::KeyValue::new(key, value.into()));
            }
        }
        #[cfg(not(feature = "otel"))]
        {
            let _ = (key, value.into());
        }
    }

    pub fn set_bool_attribute(&self, key: &'static str, value: bool) {
        #[cfg(feature = "otel")]
        {
            if let Some(cx) = &self.context {
                cx.span()
                    .set_attribute(opentelemetry::KeyValue::new(key, value));
            }
        }
        #[cfg(not(feature = "otel"))]
        {
            let _ = (key, value);
        }
    }

    pub fn set_i64_attribute(&self, key: &'static str, value: i64) {
        #[cfg(feature = "otel")]
        {
            if let Some(cx) = &self.context {
                cx.span()
                    .set_attribute(opentelemetry::KeyValue::new(key, value));
            }
        }
        #[cfg(not(feature = "otel"))]
        {
            let _ = (key, value);
        }
    }

    pub fn set_f64_attribute(&self, key: &'static str, value: f64) {
        #[cfg(feature = "otel")]
        {
            if let Some(cx) = &self.context {
                cx.span()
                    .set_attribute(opentelemetry::KeyValue::new(key, value));
            }
        }
        #[cfg(not(feature = "otel"))]
        {
            let _ = (key, value);
        }
    }

    pub fn add_event(&self, name: &'static str, attributes: Vec<(&'static str, String)>) {
        #[cfg(feature = "otel")]
        {
            if let Some(cx) = &self.context {
                cx.span().add_event(
                    name.to_string(),
                    attributes
                        .into_iter()
                        .map(|(key, value)| opentelemetry::KeyValue::new(key, value))
                        .collect(),
                );
            }
        }
        #[cfg(not(feature = "otel"))]
        {
            let _ = (name, attributes);
        }
    }

    pub fn record_error(&self, message: impl Into<String>) {
        let message = message.into();
        #[cfg(feature = "otel")]
        {
            if let Some(cx) = &self.context {
                cx.span()
                    .set_status(opentelemetry::trace::Status::error(message.clone()));
                cx.span().add_event(
                    "exception".to_string(),
                    vec![opentelemetry::KeyValue::new("exception.message", message)],
                );
            }
        }
        #[cfg(not(feature = "otel"))]
        {
            let _ = message;
        }
    }

    pub fn end(&self) {
        #[cfg(feature = "otel")]
        {
            if self.ended.swap(true, std::sync::atomic::Ordering::AcqRel) {
                return;
            }
            if let Some(cx) = &self.context {
                cx.span().end();
            }
        }
    }
}

impl Drop for OtelSpan {
    fn drop(&mut self) {
        self.end();
    }
}

impl Drop for OtelExporter {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OtelConfig;
    #[cfg(feature = "otel")]
    use crate::config::OtelProtocol;

    #[test]
    fn disabled_exporter_from_config_is_noop() {
        let config = OtelConfig::default();
        let exporter = OtelExporter::from_config(&config).expect("disabled exporter should build");
        let task_span = exporter.start_task_span("task-1", "agent-1", "model-1");
        let iter_span = exporter.start_iteration_span(&task_span, 1, "model-1");
        let tool_span = exporter.start_tool_span(&iter_span, "file-reader");
        tool_span.set_bool_attribute("tool.success", true);
        exporter.record_task_metric("agent-1", "complete", 10);
    }

    #[cfg(feature = "otel")]
    #[test]
    fn enabled_exporter_builds_and_records_without_panicking() {
        let config = OtelConfig {
            enabled: true,
            endpoint: "http://127.0.0.1:4317".to_string(),
            protocol: OtelProtocol::Grpc,
            service_name: "agentos-test".to_string(),
            sample_rate: 1.0,
            scrub_tool_inputs: true,
            scrub_tool_outputs: true,
        };

        let exporter = OtelExporter::from_config(&config).expect("enabled exporter should build");
        let task_span = exporter.start_task_span("task-1", "agent-1", "model-1");
        let iter_span = exporter.start_iteration_span(&task_span, 1, "model-1");
        let tool_span = exporter.start_tool_span(&iter_span, "file-reader");
        tool_span.set_bool_attribute("tool.success", true);
        tool_span.set_i64_attribute("tool.duration_ms", 12);
        exporter.record_llm_request("agent-1", "openai", "model-1", 42);
        exporter.record_cost("agent-1", "model-1", 0.25, 100, 50);
        exporter.record_tool_metric("agent-1", "file-reader", 12, true);
        exporter.record_health_snapshot(Some(12.0), Some(22.0), Some(33.0));
        exporter.record_task_metric("agent-1", "complete", 99);
    }
}
