---
title: Phase 1 — Repository Bootstrap & CI Baseline
tags:
  - scaffolding
  - ci
  - phase-1
date: 2026-04-18
status: planned
effort: 1d
priority: high
---

# Phase 1 — Repository Bootstrap & CI Baseline

> Initialize a standalone Rust repository with workspace, CI, lint/fmt, and a release skeleton. Nothing Gmail-specific yet — this is the vehicle we'll keep adding to.

---

## Why this phase

A production daemon stands or falls on its release discipline. We front-load the scaffolding before there's anything to break so every subsequent phase ships through the same pipeline: lint → test → build → sign → publish.

---

## Deliverables

- New GitHub repository `github.com/<org>/gmail-mcp`.
- Rust edition 2024 workspace with two crates: `gmail-mcp` (binary) and `gmail-mcp-core` (library).
- CI that runs on every PR: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test --workspace`, `cargo audit`.
- Release workflow triggered by tags (`v*`) that builds signed artifacts for Linux/macOS/Windows × x86_64/aarch64.
- `README.md`, `SECURITY.md`, `CONTRIBUTING.md`, `LICENSE-APACHE`, `LICENSE-MIT`, `.editorconfig`, `rustfmt.toml`, `clippy.toml`.
- Pre-commit hook config (`.pre-commit-config.yaml`) that runs fmt + clippy locally.

---

## Directory layout

```
gmail-mcp/
├── Cargo.toml              # workspace root
├── Cargo.lock              # committed
├── rust-toolchain.toml     # pins stable 1.XX.X
├── rustfmt.toml
├── clippy.toml
├── deny.toml               # cargo-deny config (licenses, sources, advisories)
├── .editorconfig
├── .pre-commit-config.yaml
├── README.md
├── SECURITY.md
├── CONTRIBUTING.md
├── LICENSE-APACHE
├── LICENSE-MIT
├── .github/
│   ├── workflows/
│   │   ├── ci.yml           # lint + test + audit on PR
│   │   ├── release.yml      # signed binaries on tag
│   │   └── security.yml     # cargo-audit nightly
│   ├── dependabot.yml
│   └── ISSUE_TEMPLATE/
│       ├── bug_report.md
│       └── feature_request.md
├── crates/
│   ├── gmail-mcp/           # binary crate
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── main.rs      # stub: prints "gmail-mcp v0.0.1"
│   └── gmail-mcp-core/      # library crate (reusable pieces)
│       ├── Cargo.toml
│       └── src/
│           └── lib.rs       # empty
└── docs/
    ├── architecture.md      # empty stub
    └── protocol.md          # empty stub
```

---

## Cargo workspace

```toml
# Cargo.toml (workspace root)
[workspace]
resolver = "2"
members = ["crates/*"]

[workspace.package]
version = "0.0.1"
edition = "2024"
rust-version = "1.85"
authors = ["<author>"]
license = "Apache-2.0 OR MIT"
repository = "https://github.com/<org>/gmail-mcp"
homepage = "https://github.com/<org>/gmail-mcp"

[workspace.lints.rust]
unsafe_code = "forbid"

[workspace.lints.clippy]
all = { level = "deny", priority = -1 }
pedantic = { level = "warn", priority = -1 }
module_name_repetitions = "allow"
missing_errors_doc = "allow"
must_use_candidate = "allow"

[workspace.dependencies]
# pinned deliberately; tighten later only if needed
tokio = { version = "1", features = ["rt-multi-thread", "macros", "io-util", "net", "process", "signal", "sync", "time"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
anyhow = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }
```

Binary crate pulls from workspace deps:

```toml
# crates/gmail-mcp/Cargo.toml
[package]
name = "gmail-mcp"
version.workspace = true
edition.workspace = true
license.workspace = true

[[bin]]
name = "gmail-mcp"
path = "src/main.rs"

[dependencies]
gmail-mcp-core = { path = "../gmail-mcp-core" }
tokio.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
```

---

## `main.rs` stub

```rust
use std::io::{self, Write};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(io::stderr)       // never stdout — that's MCP protocol territory
        .json()
        .init();

    writeln!(io::stderr(), "gmail-mcp {}", env!("CARGO_PKG_VERSION")).ok();
    std::process::exit(0);
}
```

Note: stderr for logs, stdout reserved for MCP. This is the **only** rule everything else flows from.

---

## CI workflow — `.github/workflows/ci.yml`

```yaml
name: CI
on:
  pull_request:
  push:
    branches: [main]

concurrency:
  group: ci-${{ github.ref }}
  cancel-in-progress: true

jobs:
  fmt:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with: { components: rustfmt }
      - run: cargo fmt --all -- --check

  clippy:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with: { components: clippy }
      - uses: Swatinem/rust-cache@v2
      - run: cargo clippy --workspace --all-targets -- -D warnings

  test:
    strategy:
      matrix:
        os: [ubuntu-latest, macos-latest, windows-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - run: cargo test --workspace --all-features

  audit:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: rustsec/audit-check@v2
        with: { token: ${{ secrets.GITHUB_TOKEN }} }

  deny:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: EmbarkStudios/cargo-deny-action@v2
```

---

## `deny.toml` — supply-chain gates

```toml
[advisories]
vulnerability = "deny"
unmaintained = "warn"
unsound = "deny"
yanked = "deny"
notice = "warn"

[licenses]
unlicensed = "deny"
allow = ["MIT", "Apache-2.0", "Apache-2.0 WITH LLVM-exception", "BSD-2-Clause", "BSD-3-Clause", "ISC", "Unicode-DFS-2016", "Zlib", "MPL-2.0", "CC0-1.0"]
copyleft = "deny"
confidence-threshold = 0.93

[bans]
multiple-versions = "warn"
wildcards = "deny"
```

---

## Release workflow — `.github/workflows/release.yml` (stub)

```yaml
name: Release
on:
  push:
    tags: ["v*"]

jobs:
  build:
    strategy:
      matrix:
        include:
          - { os: ubuntu-latest,  target: x86_64-unknown-linux-musl }
          - { os: ubuntu-latest,  target: aarch64-unknown-linux-musl }
          - { os: macos-latest,   target: x86_64-apple-darwin }
          - { os: macos-latest,   target: aarch64-apple-darwin }
          - { os: windows-latest, target: x86_64-pc-windows-msvc }
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with: { targets: ${{ matrix.target }} }
      - run: cargo build --release --target ${{ matrix.target }}
      - name: Package
        run: |
          # tar or zip depending on OS
      - uses: actions/upload-artifact@v4

  sign:
    needs: build
    runs-on: ubuntu-latest
    permissions: { id-token: write, contents: write }
    steps:
      - uses: actions/download-artifact@v4
      - uses: sigstore/cosign-installer@v3
      - run: cosign sign-blob --yes <artifact>
      - name: Create GitHub Release
        uses: softprops/action-gh-release@v2
```

The release workflow is a skeleton in Phase 1; Phase 9 fleshes it out with full signing + distribution.

---

## Dependencies to add (minimal in Phase 1)

Just the workspace deps listed above. No Gmail/MCP/OAuth libs yet.

---

## Files created

| File | Purpose |
|------|---------|
| `Cargo.toml` | Workspace root |
| `Cargo.lock` | Committed lockfile |
| `rust-toolchain.toml` | Pin stable 1.85 |
| `rustfmt.toml` | `max_width = 100`, `group_imports = "StdExternalCrate"` |
| `clippy.toml` | `cognitive-complexity-threshold = 15` |
| `deny.toml` | Licenses + advisories + bans |
| `.github/workflows/ci.yml` | Lint/test/audit on PR |
| `.github/workflows/release.yml` | Signed release skeleton |
| `.github/workflows/security.yml` | Nightly `cargo audit` |
| `.github/dependabot.yml` | Weekly dep updates |
| `crates/gmail-mcp/Cargo.toml` | Binary crate |
| `crates/gmail-mcp/src/main.rs` | Stub entry point |
| `crates/gmail-mcp-core/Cargo.toml` | Library crate |
| `crates/gmail-mcp-core/src/lib.rs` | Empty module |
| `README.md` | High-level intro + install |
| `SECURITY.md` | Disclosure policy + threat model |
| `CONTRIBUTING.md` | Dev setup + PR flow |
| `LICENSE-APACHE` / `LICENSE-MIT` | Dual license |

---

## Dependencies

- **Requires:** none.
- **Blocks:** every subsequent phase.

---

## Test plan

Phase 1 has no product code to test. The phase passes when:

1. `cargo build --workspace` succeeds.
2. `cargo test --workspace` succeeds (zero tests, zero failures).
3. `cargo clippy --workspace --all-targets -- -D warnings` clean.
4. `cargo fmt --all -- --check` clean.
5. `cargo audit` clean.
6. `cargo deny check` clean.
7. CI on a PR with a deliberate warning rejects the PR.
8. Release workflow on a `v0.0.1` tag produces artifacts for all 5 target triples.

---

## Verification

```bash
git clone https://github.com/<org>/gmail-mcp
cd gmail-mcp
cargo build --workspace
./target/debug/gmail-mcp
# prints: gmail-mcp 0.0.1

# Bump version + tag
git tag v0.0.1 && git push --tags
# → watch release workflow produce 5 signed tarballs
```

---

## Related

- [[Gmail MCP Server Plan]]
- [[02-mcp-protocol-and-stdio]]
- [[09-distribution-and-releases]]
