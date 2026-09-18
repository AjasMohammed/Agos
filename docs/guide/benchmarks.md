# Footprint Benchmarks

Numbers here are produced by one checked-in script, on a named machine, at a named commit. Anything not reproducible with that script does not belong in this file or in the README.

```bash
# Full build (vector search via ONNX/MiniLM)
cargo build --profile dist -p agentos-cli
bash scripts/bench-footprint.sh target/dist/agentos 5

# Same binary, embedder switched off in config ([memory] disable_embedder = true)
BENCH_LITE=1 bash scripts/bench-footprint.sh target/dist/agentos 5

# Lite build (no ONNX runtime linked at all; FTS5 lexical search only)
cargo build --profile dist -p agentos-cli --no-default-features
bash scripts/bench-footprint.sh target/dist/agentos 5
```

What the script measures:

| Field | Meaning |
|---|---|
| `size_bytes` | binary on disk |
| `first_boot_ms` | process start → `AgentOS is running.` on a fresh data dir: vault init, DB creation, and (full build) the one-time ~23 MB MiniLM model download |
| `cold_start_ms` | same, second boot of that data dir. This is the number that matters for restarts |
| `idle_rss_kb` | `VmRSS` after the settle period with no agents, no channels, API off |
| `threads` | kernel thread count at idle |

The script copies `config/default.toml` into a throwaway directory (all `/tmp/agentos` paths rewritten, health port randomised), so it is safe to run next to a live kernel.

## Results

Dev box: 16-core x86_64, 13 GB RAM, Linux 7.1, NVMe. Idle RSS is stable to about ±5 % across runs; cold start varies with disk cache.

| Build | Commit | Binary | Cold start | First boot | Idle RSS | Threads |
|---|---|---|---|---|---|---|
| `release` profile, full | 4a4492f | 177 MiB | 1.1 s | 89 s (model download) | 344 MB | 52 |
| `release` profile, `disable_embedder = true` | 4a4492f | 177 MiB | 0.66 s | 1.0 s | 146 MB | 37 |
| `dist` profile (thin LTO, shipped), full | 4a4492f | 108 MiB | 2.5 s | 121 s (model download) | 334 MB | 52 |
| `dist` profile, `disable_embedder = true` | 4a4492f | 108 MiB | 1.1 s | 2.2 s | 143 MB | 38 |
| `dist` profile, lite (`--no-default-features`) | 4a4492f | 87 MiB | 0.32 s | 1.4 s | 134 MB | 37 |

## Reading the numbers

- **The embedder is the footprint.** ONNX Runtime plus the MiniLM model account for roughly 200 MB of idle RSS and most of the binary. Everything else (kernel, 136 tool manifests, SQLite stores, channel adapters, HTTP stack) idles well under 150 MB.
- **Lite is not a toy.** Same kernel, same capability tokens, same approval gates, same audit log. Memory search degrades from hybrid (FTS5 + cosine + RRF) to FTS5 keyword matching: exact terms hit, paraphrases miss.
- **First boot of the full build is dominated by the model download**, so `first_boot_ms` is a network number, not a startup number. A stalled download times out after `embedder_init_timeout_secs` and boot continues with the zero-vector embedder.
- **What we do not claim:** sub-100 MB idle for the full build, or parity with single-binary agents that ship no vector search. Compare like with like: our lite build against theirs.

## Related

- `scripts/bench-footprint.sh`, `.github/workflows/bench.yml` (uploads `footprint.txt` per run)
- `docs/guide/06-security.md` for the security claims that go with these numbers
