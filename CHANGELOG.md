# Changelog

All notable changes to AgentOS are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The distributed binary is `agentos` (crate `agentos-cli`); the version reported by
`agentos --version` tracks the workspace `Cargo.toml` `[workspace.package].version`.

## [Unreleased]

### Changed
- Peripheral HAL drivers (audio, bluetooth, display, printer, raw-usb,
  usb-storage, webcam, wifi) are registered at boot only when the host has the
  hardware and the service the driver uses. Their tools are hidden otherwise,
  so a headless server no longer advertises `wifi` or `audio`. Override per
  driver with `[hal] force_enable` / `[hal] disable`. `agentos doctor` shows
  what was detected.
- The linux-amd64 release binary now includes every peripheral driver except
  bluetooth (new `release-peripherals` feature). A release gate fails if the
  binary links any shared library beyond the glibc set.

## [1.0.0-rc.1] - 2026-09-17

Release candidate for 1.0.0: Linux x86_64, single-operator deployments. Same
content as the 1.0.0 section below plus the release-readiness work from
`plans/real-world-relevance/`:

### Added
- Security regression suite covering the OpenClaw CVE classes (SSRF to cloud
  metadata, file-tool path traversal, injected control-plane writes, tampered
  signed manifests, WebSocket ambient auth, no-credential access); wired into
  `release-gate.yml`. Listed in `docs/guide/06-security.md`.
- CI guard (`scripts/check-toolpre-guard.sh`) that fails if any kernel tool
  execution path skips the ToolPre hook chain.
- `dist` cargo profile (thin LTO) used by `release.yml`; lite build
  (`--no-default-features`, no ONNX/MiniLM) published as `agentos-lite-linux-amd64`.
- `scripts/bench-footprint.sh` and `docs/guide/benchmarks.md` (binary size,
  cold start, idle RSS, measured).
- `docs/guide/00-five-minute-tour.md`, `MAINTAINERS.md`, SECURITY.md
  "Known Limitations" and 90-day disclosure window.
- Release signing key (minisign id `0692DEA1023C9472`) committed at
  `packaging/signing/agentos-release.pub`.

### Changed
- `scripts/install.sh` pins the release public key, requires the `.sig`
  asset, and accepts `rsign` as a verifier.
- Streaming LLM adapters (OpenAI, Anthropic, Gemini, custom) carry partial
  UTF-8 sequences across chunk boundaries instead of emitting U+FFFD.
- `embeddings` is a cargo feature (default on in `agentos-cli` only);
  `Embedder::new()` failure falls back to no-op instead of aborting boot.

### Security
- Dependency advisories cleared (`cargo audit`: 12 → 0): wasmtime / wasmtime-wasi
  44.0.2 → 47.0.4 (filesystem sandbox escape, WASI `FilePerms` bypass, type-index
  mix-up, `fd_renumber` leak), rustls → 0.23.45 (TLS 1.3 handshake messages
  accepted across encryption levels), h2 → 0.4.19, quinn-proto → 0.11.18,
  crossbeam-epoch → 0.9.21, quick-xml → 0.41 (quadratic attribute check,
  unbounded namespace allocation; reachable through the `data-parser` tool).
- `data-parser` XML: ported to quick-xml 0.41 entity events. Character
  references and the five predefined entities resolve; DTD-declared entities
  are never expanded.

### Fixed
- SSRF guard: cloud metadata hostnames, trailing-dot hosts, dotted-octal /
  hex / short-form IPv4 encodings, and IPv4-mapped IPv6 are now blocked.

## [1.0.0] - unreleased

First production release. AgentOS graduates from "feature-complete + green
internally" to a single, unified, broadly distributable v1.0.0 with signed
artifacts, observability, gateway-first deployment, and a documented release and
rollback process.

### Added

- **Distribution & packaging.** One-line installer (`scripts/install.sh`,
  `curl … | bash`) for Linux/macOS (amd64/arm64) with mandatory SHA-256 checksum
  verification and minisign signature verification; Windows installer
  (`scripts/install.ps1`, beta) for WSL2. Homebrew formula
  (`packaging/homebrew/agentos.rb`, `brew tap agentos/tap && brew install agentos`)
  that pulls the prebuilt signed binary per arch. `cargo install` from the git
  repo. Signed prebuilt multi-arch binaries attached to every GitHub Release.
- **Production config finalization & boot preflight.** `config/production.toml`
  uses persistent paths under `/var/lib/agentos` (no `/tmp` dev defaults);
  boot-time validation rejects unsafe production configuration before the kernel
  serves traffic.
- **Observability & monitoring.** Structured JSON logs to stdout with correlation
  IDs, a Prometheus `/metrics` endpoint (port 9091), OpenTelemetry trace export
  (`config/docker.toml` → jaeger), and a Grafana dashboard
  (`deploy/observability/`). Every deploy mode is debuggable identically.
- **Gateway-first deployment ("run as a bot").** `agentos gateway run` daemon
  entrypoint reusing `agentos-channels` + `ChannelManager`, a dedicated systemd
  unit (`deploy/agentos-gateway.service`), connect-at-boot, and a compose service —
  run AgentOS as a Telegram/Discord/Slack bot via one command.
- **MCP catalog installer.** `agentos mcp install <id>` one-command install for
  catalog MCP servers with a runtime resolver and seed catalog entries.
- **`agentos update` self-update.** Downloads and verifies the signed GitHub
  Release asset for the host platform against the embedded release public key
  (fail-closed) and atomically replaces the running binary. `--check` reports an
  available version without installing.
- **Documentation site.** Public mdBook site generated from `docs/guide` and the
  Obsidian vault reference docs, published via GitHub Pages
  (`.github/workflows/docs.yml`).
- **Supply-chain security.** `cargo-audit` / `cargo-deny` CI gates (`deny.toml`),
  a CycloneDX SBOM (`bom.json`) attached to every release, and minisign-signed
  release artifacts. From the first signed release, the public verification key is
  published at `packaging/signing/agentos-release.pub`.
- **Release governance.** Keep-a-changelog `CHANGELOG.md`, semver-from-1.0.0
  policy with signed annotated tags (see
  `obsidian-vault/plans/production-release-v1/VERSION-AND-TAGGING-POLICY.md`),
  a release-notes template, and a documented rollback runbook
  (`obsidian-vault/plans/production-release-v1/ROLLBACK-RUNBOOK.md`).

### Changed

- **Native tool calling** (shipped pre-release): Anthropic / OpenAI-compat / Gemini
  adapters use provider-native `tool_use` / `tool_calls` / `functionCall` blocks
  instead of the JSON-in-markdown envelope; Ollama/Mock retain the envelope
  fallback.
- **Production stability** (shipped pre-release): memory/health-monitor resilience,
  graceful shutdown, and watchdog integration (`Type=notify`, `WATCHDOG=1`).
- **Scheduled task delivery** (shipped pre-release): unified `DeliveryMode`
  (Silent/Direct/ViaAgent) across cron/once/timer with persisted schedules and
  per-fire run history.
- **Web UI.** Chat streaming and markdown rendering fixes; no raw JSON blobs in the
  chat surface; broader CLI parity across web pages.
- **Release CI.** `.github/workflows/release.yml` cross-compiles multi-arch
  binaries and, with Phase 08 signing live, signs each artifact and attaches the
  detached signature, `.sha256` checksum, and SBOM to the GitHub Release.

### Security

- Minisign-signed release artifacts (from the first signed release onward); the
  one-line installer and Homebrew formula verify signatures before executing the
  downloaded binary (fail-closed).
- `cargo-audit` / `cargo-deny` CI gates enforce advisory and license policy.
- CycloneDX SBOM published per release for dependency scanning.
- `SECURITY.md` documents the trust boundaries (load-bearing vs. hardening), the
  vulnerability-reporting process, release verification, and the hardened-systemd
  bwrap caveat.

[Unreleased]: https://github.com/AjasMohammed/Agos/compare/v1.0.0...HEAD
[1.0.0]: https://github.com/AjasMohammed/Agos/releases/tag/v1.0.0
