use criterion::{black_box, criterion_group, criterion_main, Criterion};

/// Minimal config TOML covering all required (non-default) fields.
/// Mirrors the pattern used in the kernel's own unit tests.
const MINIMAL_TOML: &str = r#"
[kernel]
max_concurrent_tasks = 4
default_task_timeout_secs = 60
context_window_max_entries = 100
context_window_token_budget = 8000

[secrets]
vault_path = "/tmp/agentos/vault/secrets.db"

[audit]
log_path = "/tmp/agentos/data/audit.db"

[tools]
core_tools_dir = "/tmp/agentos/tools/core"
user_tools_dir  = "/tmp/agentos/tools/user"
data_dir        = "/tmp/agentos/data"

[bus]
socket_path = "/tmp/agentos/agentos.sock"

[ollama]
host          = "http://localhost:11434"
default_model = "llama3.2"
"#;

fn bench_config_parse(c: &mut Criterion) {
    // Benchmark deserialising the required fields from a TOML string.
    // This is the primary cost of a kernel cold start that can be measured
    // in isolation without opening databases or spawning tasks.
    c.bench_function("kernel_config_parse", |b| {
        b.iter(|| {
            let cfg: agentos_kernel::config::KernelConfig =
                toml::from_str(black_box(MINIMAL_TOML)).expect("parse config");
            black_box(cfg);
        });
    });
}

fn bench_config_serialize_roundtrip(c: &mut Criterion) {
    // Benchmark re-serialising a loaded config back to TOML.
    // Exercises the serde path used when saving config changes at runtime.
    let config: agentos_kernel::config::KernelConfig =
        toml::from_str(MINIMAL_TOML).expect("parse config");

    c.bench_function("kernel_config_toml_serialize", |b| {
        b.iter(|| {
            black_box(toml::to_string(black_box(&config)).expect("serialize config"));
        });
    });
}

criterion_group!(
    benches,
    bench_config_parse,
    bench_config_serialize_roundtrip
);
criterion_main!(benches);
