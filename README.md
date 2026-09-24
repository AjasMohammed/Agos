<p align="center">
  <h1 align="center">AgentOS</h1>
  <p align="center"><strong>A hardened, self-hosted AI agent daemon you talk to from Telegram, Discord, or a terminal.</strong></p>
  <p align="center">Single Rust binary. Capability-scoped tools. Encrypted secrets. Append-only audit. Every security claim below links to the test that proves it.</p>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/language-Rust-orange?style=flat-square" alt="Language: Rust" />
  <img src="https://img.shields.io/badge/license-Apache--2.0-green?style=flat-square" alt="License: Apache-2.0" />
  <img src="https://img.shields.io/badge/status-v1.0.0--rc%20%C2%B7%20single--operator-blue?style=flat-square" alt="Status: v1.0.0-rc, single operator" />
  <img src="https://img.shields.io/badge/platform-Linux%20x86__64-lightgrey?style=flat-square" alt="Platform: Linux x86_64" />
</p>

---

## What it is

AgentOS runs LLM agents as a long-lived daemon on your own machine. Agents get tools (files, shell, web, memory, 130+ more) only through signed manifests and per-task capability tokens; anything risky stops at an approval prompt in your chat; every action lands in an audit log you can query. It speaks to OpenAI, Anthropic, Gemini, Ollama, and 20 other providers.

**Why not just run OpenClaw?** OpenClaw's WebSocket accepted ambient browser credentials, which became [CVE-2026-25253](https://nvd.nist.gov/vuln/detail/CVE-2026-25253) (cross-site hijack to remote code execution), and its skill marketplace shipped over a thousand malicious skills in the ClawHavoc campaign. AgentOS has no cookie auth on its socket, no marketplace, and refuses unsigned community tools. The table under [Security](#security-what-we-test-against) links the regression test for each of those attack shapes.

## Quick start (five minutes)

Linux x86_64. macOS via Docker. Windows via WSL2. Full walkthrough with expected output: [docs/guide/00-five-minute-tour.md](docs/guide/00-five-minute-tour.md).

**Install** (pick one):

```bash
# Signed release binary (available from the first tagged release; verifies the minisign signature)
curl -fsSL https://raw.githubusercontent.com/AjasMohammed/Agos/main/scripts/install.sh | bash
# Lite build (no ONNX/MiniLM vector search, FTS5 only; ~134 MB idle): same line with AGENTOS_FLAVOR=lite
curl -fsSL https://raw.githubusercontent.com/AjasMohammed/Agos/main/scripts/install.sh | AGENTOS_FLAVOR=lite bash

# From source (Rust 1.91+)
cargo install --git https://github.com/AjasMohammed/Agos agentos-cli

# Container (read-only rootfs, non-root, Ollama + Jaeger sidecars)
cp .env.example .env && docker compose up -d
```

**Run** (terminal 1):

```bash
export AGENTOS_VAULT_PASSPHRASE='choose-a-passphrase'   # or omit and be prompted
agentos start
```

**Connect an agent and a Telegram bot** (terminal 2):

```bash
agentos agent connect --provider ollama --model llama3.2 --name assistant

# Token from @BotFather. Value is prompted, never passed on the command line.
agentos secret set TELEGRAM_BOT_TOKEN --scope global
agentos channel connect --kind telegram --display-name "my-bot" \
  --credential-key TELEGRAM_BOT_TOKEN --active-agent assistant
```

Send `/start` to your bot in Telegram. It replies with a 6-character pairing code. Approve it once:

```bash
agentos channel pair approve ABC123
```

Now send "hi". The first time the agent wants to run something with side effects, the bot asks you to approve or deny in the chat. Nothing else on Telegram can talk to it: unpaired senders are ignored.

No Telegram? `agentos task run --agent assistant "Summarise the files in my workspace"` works from the terminal, and `agentos start` with `[api] enabled = true` in config serves the REST API the React panel talks to.

## Security: what we test against

Each row is a public 2026 agent-framework incident class, the control that stops it here, and the test that proves it. Kept in sync with [docs/guide/06-security.md](docs/guide/06-security.md); the limitations we know about are in [SECURITY.md](SECURITY.md#known-limitations).

| # | Incident class | AgentOS control | Test |
|---|---|---|---|
| 1 | Cross-site WebSocket hijack → token theft → RCE (OpenClaw CVE-2026-25253) | No cookie auth; WS accepts only a single-use 30 s ticket minted with a bearer key | [`service_tests.rs`](crates/agentos-api/tests/service_tests.rs) `security_ws_has_no_ambient_auth_for_cross_origin_pages` |
| 2 | Unauthenticated control plane when no credential is configured (OpenFang #1034) | Fail-closed API key middleware; login returns 503 when nothing is configured; no dev-mode branch | [`service_tests.rs`](crates/agentos-api/tests/service_tests.rs) `security_no_configured_credentials_means_no_access` |
| 3 | Prompt-injected control-plane write (OpenClaw CVE-2026-35650) | `RiskClass::ControlPlane` decided by the operator's approval mode; ToolPre hook on every execution path | [`security_injected_config_write_requires_approval.rs`](crates/agentos-kernel/tests/security_injected_config_write_requires_approval.rs) |
| 4 | Malicious or relabelled skill install (ClawHavoc) | Trust tiers + Ed25519 signature over a payload that includes `risk_class` and `name` | [`security_acceptance_test.rs`](crates/agentos-kernel/tests/security_acceptance_test.rs) scenarios F, G, H |
| 5 | SSRF to cloud metadata / loopback | Host extraction that survives userinfo, integer/hex/octal IPv4, IPv6-mapped, and metadata hostnames | [`security_ssrf_metadata_blocked.rs`](crates/agentos-kernel/tests/security_ssrf_metadata_blocked.rs) |
| 6 | Internet-exposed instances by default (40k OpenClaw hosts) | API off by default; API and health bind `127.0.0.1` | [`config.rs`](crates/agentos-kernel/src/config.rs) `api_default_bind_is_loopback` |
| 7 | Secret payload shown in an approval prompt | Redaction on the escalation preview | [`approval_hook.rs`](crates/agentos-kernel/src/hooks/approval_hook.rs) `redaction_*` |
| 8 | Path traversal / symlink escape from file tools | `..` rejected before I/O, percent-decoding, canonical containment in the agent home | [`security_file_tools_reject_traversal.rs`](crates/agentos-kernel/tests/security_file_tools_reject_traversal.rs) |
| — | A new execution path skipping the approval hook | CI tripwire over every `tool_runner.execute(` call site | [`scripts/check-toolpre-guard.sh`](scripts/check-toolpre-guard.sh) |

Underneath: HMAC-signed capability tokens per task, AES-256-GCM vault with Argon2id, seccomp-BPF and bubblewrap for non-core tools, append-only SQLite audit log, Ed25519-signed tool manifests, `<user_data>` wrapping plus an injection scanner as defense in depth. Report vulnerabilities via [SECURITY.md](SECURITY.md).

## Footprint

Measured by [`scripts/bench-footprint.sh`](scripts/bench-footprint.sh) at commit 4a4492f on a 16-core x86_64 box. Method and caveats: [docs/guide/benchmarks.md](docs/guide/benchmarks.md).

| Build | Binary | Cold start | Idle RSS |
|---|---|---|---|
| Full (hybrid vector + keyword memory search) | 108 MiB | 2.5 s | 334 MB |
| Full, `[memory] disable_embedder = true` | 108 MiB | 1.1 s | 143 MB |
| Lite (`--no-default-features`, keyword search only) | 87 MiB | 0.32 s | 134 MB |

The ONNX embedding runtime is the footprint. Lite is the same kernel and the same security model; memory search degrades from hybrid to exact-keyword FTS5.

## How it compares

Only cells we can cite are filled. Blank means we did not verify it.

| | AgentOS | OpenClaw | ZeroClaw | Hermes Agent |
|---|---|---|---|---|
| Language | Rust | TypeScript | Rust | Python |
| Tool sandbox | seccomp-BPF + bwrap, per-tool policy | | WASM | |
| Signed tool manifests | Ed25519, trust tiers | | | |
| Approval gate on risky tools | per-tool risk class, chat approve/deny | | | |
| Audit log | append-only SQLite, 80+ event types | | | |
| Idle RSS (self-reported) | 134 MB lite / 334 MB full | | < 5 MB | |
| Public CVE history | none yet (no release yet) | CVE-2026-25253, CVE-2026-35650 | | |

## How it works

```
┌──────────────────────────────────────────────────────────┐
│                        AgentOS                            │
│  ┌──────────────┐   ┌──────────────────────────────────┐ │
│  │  agentos CLI │   │ Channels: Telegram · Discord     │ │
│  └──────┬───────┘   │ Slack · Matrix · Teams · Webhook │ │
│         │ UDS       └──────────────┬───────────────────┘ │
│  ┌──────▼──────────────────────────▼───────────────────┐ │
│  │              Inference Kernel                        │ │
│  │  Scheduler · Router · Context · Agent Registry      │ │
│  │  Capability Engine · Vault · Audit · Approval Hooks │ │
│  └──────┬──────────────────────┬───────────────────────┘ │
│  ┌──────▼──────────┐   ┌──────▼────────────────────────┐ │
│  │ LLM Adapters    │   │ Tool Registry + Sandbox       │ │
│  │ Ollama · OpenAI │   │ 130+ signed manifests         │ │
│  │ Anthropic       │   │ seccomp-BPF · bwrap           │ │
│  │ Gemini · 20 more│   │ WASM (experimental)           │ │
│  └─────────────────┘   └───────────────────────────────┘ │
└──────────────────────────────────────────────────────────┘
```

The design principle: the LLM is the CPU, tools are the programs, a structured intent is the syscall, and the kernel enforces capabilities between them. Details in [docs/guide/03-architecture.md](docs/guide/03-architecture.md).

**Also included:** three-tier agent memory (episodic, semantic, procedural) with consolidation; multi-step pipelines; scheduled tasks; REST API with an OpenAI-compatible `/v1/chat/completions`; MCP client for external tool servers; skills (SKILL.toml prompts); an agent scratchpad with wikilinks. **Experimental**, not covered by the v1 stability promise: hardware drivers (audio, webcam, wifi, bluetooth), WASM tools, and the React control panel.

## Documentation

| Document | Description |
|---|---|
| [00 — Five-minute tour](docs/guide/00-five-minute-tour.md) | The quick start above, with expected output |
| [01 — Introduction](docs/guide/01-introduction.md) | Vision, philosophy, current status |
| [02 — Getting Started](docs/guide/02-getting-started.md) | Build, configure, and run from source |
| [03 — Architecture](docs/guide/03-architecture.md) | System design, crate graph, boot sequence |
| [04 — CLI Reference](docs/guide/04-cli-reference.md) | Every command |
| [05 — Tools Guide](docs/guide/05-tools-guide.md) | Built-in tools, manifests, sandboxing, signing |
| [06 — Security Model](docs/guide/06-security.md) | Vault, tokens, permissions, audit, threat classes |
| [07 — Configuration](docs/guide/07-configuration.md) | TOML reference |
| [Benchmarks](docs/guide/benchmarks.md) | Footprint numbers and how to reproduce them |

## Building from source

```bash
git clone https://github.com/AjasMohammed/Agos.git && cd Agos
cargo build -p agentos-cli                  # the binary is target/debug/agentos
cargo test --workspace                      # ~3,200 tests
cargo build --profile dist -p agentos-cli   # what releases ship (thin LTO, stripped)
```

Needs Rust 1.91+ and about 13 GB of RAM with `-j 4`. See [CONTRIBUTING.md](CONTRIBUTING.md) for a 30-minute orientation, and [MAINTAINERS.md](MAINTAINERS.md) for who signs releases and what happens if they go quiet.

## Releases

Semantic versioning, one release per week at most, Tuesdays. A tag is cut only when formatting, clippy, the full test suite, the security regression suite, the ToolPre guard, `cargo deny`, and a clean-container smoke test all pass; artifacts are minisign-signed with an SBOM. Policy and rollback runbook live in the repo's planning vault.

Scope is frozen until v1.0.0: bug fixes, security fixes, tests, and docs are welcome; new channels, providers, or tools wait.

## License

[Apache License 2.0](LICENSE).
