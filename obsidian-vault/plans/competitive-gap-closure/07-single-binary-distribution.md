---
title: "Phase 3.1: Single Binary Distribution"
tags:
  - cli
  - distribution
  - v3
  - plan
  - phase-3
date: 2026-03-30
status: planned
effort: 2d
priority: high
---

# Phase 3.1: Single Binary Distribution

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Compile AgentOS into a single statically-linked binary (~30-40MB) with embedded assets, cross-compiled for 4 targets, with Docker image and install script.

**Architecture:** Rename the `agentctl` binary to `agentos`. Embed web UI templates, default config, core tool manifests, core skills, and provider catalog using `rust-embed`. Static link via musl for Linux, standard for macOS. GitHub Actions CI produces release artifacts.

**Tech Stack:** rust-embed, cross (cross-compilation), GitHub Actions, Docker

---

## Why This Phase

OpenFang ships as a ~32MB binary. AgentOS requires `cargo build` of a 17-crate workspace. Nobody discovers a project they can't install in 30 seconds.

## Current → Target State

**Current:** `cargo build --workspace` produces `target/debug/agentctl`. No release binaries. No Docker image. No install script.

**Target:** `curl -fsSL https://get.agentos.dev | sh` installs a single `agentos` binary. Also available via `cargo install agentos`, Docker, and GitHub Releases.

## Files Changed

| File | Action | Purpose |
|------|--------|---------|
| `crates/agentos-cli/Cargo.toml` | Modify | Rename binary, add rust-embed |
| `crates/agentos-cli/src/embedded.rs` | Create | Embedded asset extraction |
| `crates/agentos-cli/src/main.rs` | Modify | Extract embedded assets on first run |
| `.cargo/config.toml` | Create | musl target configuration |
| `.github/workflows/release.yml` | Create | CI/CD release pipeline |
| `Dockerfile` | Create | Multi-stage Docker build |
| `scripts/install.sh` | Create | One-liner install script |

## Dependencies

- **Requires:** Nothing — this is a root phase
- **Blocks:** Phase 3.2 (Benchmarks), Phase 3.3 (Community)

---

## Detailed Tasks

### Task 1: Rename Binary

- [ ] **Step 1: Change binary name in Cargo.toml**

In `crates/agentos-cli/Cargo.toml`:
```toml
[[bin]]
name = "agentos"
path = "src/main.rs"
```

- [ ] **Step 2: Add alias for backward compat**

Create `scripts/post-install.sh` that symlinks `agentctl → agentos`.

- [ ] **Step 3: Verify build**

Run: `cargo build -p agentos-cli && ls target/debug/agentos`
Expected: Binary exists at `target/debug/agentos`

- [ ] **Step 4: Commit**

```bash
git add crates/agentos-cli/Cargo.toml scripts/
git commit -m "feat(cli): rename binary from agentctl to agentos"
```

### Task 2: Embed Static Assets

**Files:**
- Create: `crates/agentos-cli/src/embedded.rs`
- Modify: `crates/agentos-cli/Cargo.toml` (add rust-embed)

- [ ] **Step 1: Add rust-embed dependency**

```toml
rust-embed = { version = "8", features = ["compression"] }
```

- [ ] **Step 2: Write embedded asset module**

```rust
use rust_embed::Embed;
use std::path::Path;
use tracing::info;

#[derive(Embed)]
#[folder = "../../config/"]
#[prefix = "config/"]
struct ConfigAssets;

#[derive(Embed)]
#[folder = "../../tools/core/"]
#[prefix = "tools/core/"]
struct ToolAssets;

#[derive(Embed)]
#[folder = "../../skills/core/"]
#[prefix = "skills/core/"]
struct SkillAssets;

/// Extract embedded assets to a data directory on first run.
pub fn extract_assets_if_needed(data_dir: &Path) -> std::io::Result<()> {
    let config_dir = data_dir.join("config");
    if !config_dir.exists() {
        std::fs::create_dir_all(&config_dir)?;
        for file in ConfigAssets::iter() {
            if let Some(content) = ConfigAssets::get(&file) {
                let path = data_dir.join(file.as_ref());
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&path, content.data.as_ref())?;
                info!("Extracted: {}", file);
            }
        }
    }

    let tools_dir = data_dir.join("tools/core");
    if !tools_dir.exists() {
        std::fs::create_dir_all(&tools_dir)?;
        for file in ToolAssets::iter() {
            if let Some(content) = ToolAssets::get(&file) {
                let path = data_dir.join(file.as_ref());
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&path, content.data.as_ref())?;
            }
        }
    }

    let skills_dir = data_dir.join("skills/core");
    if !skills_dir.exists() {
        std::fs::create_dir_all(&skills_dir)?;
        for file in SkillAssets::iter() {
            if let Some(content) = SkillAssets::get(&file) {
                let path = data_dir.join(file.as_ref());
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&path, content.data.as_ref())?;
            }
        }
    }

    Ok(())
}
```

- [ ] **Step 3: Call extraction in main.rs before kernel boot**

- [ ] **Step 4: Verify build**

Run: `cargo build -p agentos-cli --release`
Expected: Binary embeds all assets

- [ ] **Step 5: Commit**

```bash
git add crates/agentos-cli/
git commit -m "feat(cli): embed config, tools, and skills in binary via rust-embed"
```

### Task 3: Cross-Compilation and Release CI

**Files:**
- Create: `.cargo/config.toml`
- Create: `.github/workflows/release.yml`

- [ ] **Step 1: Write cargo config for musl**

```toml
[target.x86_64-unknown-linux-musl]
rustflags = ["-C", "target-feature=+crt-static"]

[target.aarch64-unknown-linux-musl]
rustflags = ["-C", "target-feature=+crt-static"]
```

- [ ] **Step 2: Write GitHub Actions release workflow**

```yaml
name: Release
on:
  push:
    tags: ['v*']

jobs:
  build:
    strategy:
      matrix:
        include:
          - target: x86_64-unknown-linux-musl
            os: ubuntu-latest
            artifact: agentos-linux-amd64
          - target: aarch64-unknown-linux-musl
            os: ubuntu-latest
            artifact: agentos-linux-arm64
          - target: x86_64-apple-darwin
            os: macos-latest
            artifact: agentos-darwin-amd64
          - target: aarch64-apple-darwin
            os: macos-latest
            artifact: agentos-darwin-arm64
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: ${{ matrix.target }}
      - name: Install musl tools
        if: contains(matrix.target, 'musl')
        run: sudo apt-get install -y musl-tools
      - name: Build
        run: cargo build --release --target ${{ matrix.target }} -p agentos-cli
      - name: Package
        run: |
          cp target/${{ matrix.target }}/release/agentos ${{ matrix.artifact }}
          sha256sum ${{ matrix.artifact }} > ${{ matrix.artifact }}.sha256
      - uses: actions/upload-artifact@v4
        with:
          name: ${{ matrix.artifact }}
          path: |
            ${{ matrix.artifact }}
            ${{ matrix.artifact }}.sha256

  docker:
    needs: build
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: docker/setup-buildx-action@v3
      - uses: docker/login-action@v3
        with:
          registry: ghcr.io
          username: ${{ github.actor }}
          password: ${{ secrets.GITHUB_TOKEN }}
      - uses: docker/build-push-action@v5
        with:
          push: true
          platforms: linux/amd64,linux/arm64
          tags: ghcr.io/${{ github.repository }}:latest,ghcr.io/${{ github.repository }}:${{ github.ref_name }}

  release:
    needs: [build, docker]
    runs-on: ubuntu-latest
    steps:
      - uses: actions/download-artifact@v4
      - uses: softprops/action-gh-release@v1
        with:
          files: |
            agentos-linux-amd64/*
            agentos-linux-arm64/*
            agentos-darwin-amd64/*
            agentos-darwin-arm64/*
```

- [ ] **Step 3: Write Dockerfile**

```dockerfile
FROM rust:1.80-bookworm AS builder
WORKDIR /build
COPY . .
RUN cargo build --release -p agentos-cli
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/agentos /usr/local/bin/agentos
EXPOSE 8080 8081 9091
ENTRYPOINT ["agentos"]
CMD ["start"]
```

- [ ] **Step 4: Write install script**

`scripts/install.sh`:
```bash
#!/bin/sh
set -e
REPO="agentos/agentos"
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)
case "$ARCH" in x86_64) ARCH="amd64" ;; aarch64|arm64) ARCH="arm64" ;; esac
ARTIFACT="agentos-${OS}-${ARCH}"
LATEST=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | grep tag_name | cut -d'"' -f4)
URL="https://github.com/${REPO}/releases/download/${LATEST}/${ARTIFACT}"
echo "Installing AgentOS ${LATEST} for ${OS}/${ARCH}..."
curl -fsSL "$URL" -o /tmp/agentos
chmod +x /tmp/agentos
sudo mv /tmp/agentos /usr/local/bin/agentos
echo "✓ Installed agentos to /usr/local/bin/agentos"
agentos --version
```

- [ ] **Step 5: Commit**

```bash
git add .cargo/ .github/ Dockerfile scripts/install.sh
git commit -m "feat(dist): add release CI, Dockerfile, and install script"
```

## Verification

```bash
# Local build test
cargo build --release -p agentos-cli
ls -lh target/release/agentos  # Check binary size

# Docker build test
docker build -t agentos:test .
docker run --rm agentos:test --version

# Verify embedded assets extract
./target/release/agentos start --help
```
