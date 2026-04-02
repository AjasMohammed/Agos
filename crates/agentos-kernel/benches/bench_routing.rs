use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};

fn bench_command_dispatch_overhead(c: &mut Criterion) {
    // Benchmark the overhead of matching a KernelCommand variant.
    // This is a microbenchmark of the dispatch enum match itself — the
    // innermost hot path that the kernel run loop executes for every
    // inbound command.
    let mut group = c.benchmark_group("command_routing");
    group.throughput(Throughput::Elements(1));

    group.bench_function("list_agents_match", |b| {
        b.iter(|| {
            let cmd = black_box(agentos_bus::KernelCommand::ListAgents);
            let result = match cmd {
                agentos_bus::KernelCommand::ListAgents => 1usize,
                _ => 0,
            };
            black_box(result);
        });
    });

    group.bench_function("get_status_match", |b| {
        b.iter(|| {
            let cmd = black_box(agentos_bus::KernelCommand::GetStatus);
            let result = match cmd {
                agentos_bus::KernelCommand::GetStatus => 1usize,
                _ => 0,
            };
            black_box(result);
        });
    });

    group.finish();
}

criterion_group!(benches, bench_command_dispatch_overhead);
criterion_main!(benches);
