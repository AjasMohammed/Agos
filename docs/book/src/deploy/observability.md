# Observability

AgentOS exposes three observability surfaces, identical across every deploy mode
(host/systemd, Docker, Helm, gateway, dev):

1. **Structured logs** — JSON to stdout/stderr (+ optional file) via `tracing`.
2. **Prometheus metrics** — `/metrics` on the kernel health port (default `9091`).
3. **OpenTelemetry traces/metrics** — OTLP export, opt-in (build with `--features otel`).

## HTTP endpoints (port 9091)

| Endpoint | Purpose |
|----------|---------|
| `GET /healthz` | Liveness — `{"status":"ok"}` once the kernel is up. |
| `GET /readyz`  | Readiness — ok when subsystems are accepting work. |
| `GET /metrics` | Prometheus text exposition (all `agentos_*` series). |

```bash
curl -sf http://localhost:9091/healthz
curl -sf http://localhost:9091/metrics | grep -c '^agentos_'   # > 0
```

## Metric catalog

Source of truth: `crates/agentos-kernel/src/metrics.rs`.

| Metric | Type | Labels | Meaning |
|--------|------|--------|---------|
| `agentos_tasks_queued_total` | counter | — | Tasks enqueued |
| `agentos_tasks_completed_total` | counter | — | Tasks finished successfully |
| `agentos_tasks_failed_total` | counter | — | Tasks that failed |
| `agentos_task_queue_depth` | gauge | — | Tasks currently queued/running |
| `agentos_task_duration_ms` | histogram¹ | — | End-to-end task wall time |
| `agentos_inference_total` | counter | `provider` | LLM inference calls |
| `agentos_inference_latency_ms` | histogram¹ | `provider` | LLM call latency |
| `agentos_tokens_input_total` | counter | `provider` | Prompt tokens consumed |
| `agentos_tokens_output_total` | counter | `provider` | Completion tokens produced |
| `agentos_tool_executions_total` | counter | `tool`, `success` | Tool invocations |
| `agentos_tool_duration_ms` | histogram¹ | `tool` | Tool execution time |
| `agentos_connected_agents` | gauge | — | Live connected agents |
| `agentos_events_emitted_total` | counter | — | Events emitted to the bus |
| `agentos_events_processed_total` | counter | — | Events processed |
| `agentos_events_dropped_total` | counter | — | Events dropped (backpressure) |
| `agentos_capability_requests_total` | counter | — | Capability checks attempted |
| `agentos_capability_successes_total` | counter | — | Capability checks allowed |
| `agentos_capability_failures_total` | counter | — | Capability checks denied |
| `agentos_rate_limited_total` | counter | — | Requests rejected by rate limiting |
| `agentos_retrieval_refresh_total` | counter | — | Memory retrieval refreshes |
| `agentos_retrieval_reuse_total` | counter | — | Cached retrieval reuses |
| `agentos_retrieval_knowledge_blocks` | gauge | — | Knowledge blocks in context |
| `agentos_retrieval_refresh_latency_ms` | histogram¹ | — | Retrieval refresh latency |

¹ Histograms are rendered by `metrics_exporter_prometheus` as **summaries** with
`{quantile="..."}` labels plus `_sum`/`_count`. Query p95 as
`agentos_task_duration_ms{quantile="0.95"}`.

## Prometheus + Grafana

### Docker / Compose

The provided overlay scrapes `agentos:9091` automatically and provisions Grafana with the
**AgentOS** dashboard:

```bash
docker compose -f docker-compose.yml \
               -f deploy/observability/docker-compose.observability.yml up -d
```

- Prometheus → <http://localhost:9090>
- Grafana → <http://localhost:3000> (admin/admin)

### systemd / host

Point Prometheus at the host port — edit `deploy/observability/prometheus.yml`:

```yaml
static_configs:
  - targets: ["localhost:9091"]
```

## Structured logs

Set in `[logging]` (or via `RUST_LOG` / `AGENTOS_LOG_FORMAT`):

```toml
[logging]
log_dir = "/var/log/agentos"
log_format = "json"   # "text" for human-readable dev logs
log_level = "info"
```

JSON log lines carry span fields (`task_id`, `agent_id`, `trace_id`) from instrumented hot
paths, so you can filter one task's lines:

```bash
jq 'select(.span.task_id=="<id>")' agentos.log
```

Ship JSON logs to Loki/ELK/Datadog with any stdout log driver. Change the level at runtime
without a restart: `agentos log set-level debug`.

## OpenTelemetry → Jaeger

Build with `--features otel` (the `Dockerfile` and `release.sh` already do), then enable:

```toml
[otel]
enabled = true
endpoint = "http://jaeger:4317"   # OTLP gRPC collector
protocol = "grpc"
service_name = "agentos"
sample_rate = 1.0
```

or at runtime: `AGENTOS_OTEL_ENABLED=true AGENTOS_OTEL_ENDPOINT=http://jaeger:4317`.

The root `docker-compose.yml` already runs Jaeger and wires the endpoint. Open the Jaeger UI
at <http://localhost:16686> and look for the **agentos** service — task and tool spans appear
with `task_id`/`tool` attributes, cross-referencible with the `trace_id` log field.
