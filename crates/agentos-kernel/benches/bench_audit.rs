use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use tempfile::TempDir;

fn bench_audit_open(c: &mut Criterion) {
    // Benchmark opening (and initialising) a fresh AuditLog database.
    // This is a one-shot cost paid at kernel startup and after crash recovery.
    c.bench_function("audit_log_open", |b| {
        b.iter_batched(
            || TempDir::new().expect("tempdir"),
            |tmp| {
                let path = tmp.path().join("audit_bench.db");
                black_box(agentos_audit::AuditLog::open(&path).expect("open audit log"));
                // keep `tmp` alive until after the measured section
                tmp
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

fn bench_audit_append(c: &mut Criterion) {
    // Benchmark appending a single event to an already-open AuditLog.
    // This is the hot path called for every security-relevant operation in the
    // kernel (tool executions, permission decisions, task state changes, …).
    let tmp = TempDir::new().expect("tempdir");
    let path = tmp.path().join("audit_bench.db");
    let audit_log = agentos_audit::AuditLog::open(&path).expect("open audit log");

    let mut group = c.benchmark_group("audit_throughput");
    group.throughput(Throughput::Elements(1));

    group.bench_function("append_single_event", |b| {
        b.iter(|| {
            let entry = agentos_audit::AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: agentos_types::TraceID::new(),
                event_type: agentos_audit::AuditEventType::ToolExecutionStarted,
                agent_id: None,
                task_id: None,
                tool_id: None,
                details: serde_json::json!({ "bench": true }),
                severity: agentos_audit::AuditSeverity::Info,
                reversible: false,
                rollback_ref: None,
            };
            black_box(audit_log.append(entry).expect("append audit entry"));
        });
    });

    group.finish();
}

criterion_group!(benches, bench_audit_open, bench_audit_append);
criterion_main!(benches);
