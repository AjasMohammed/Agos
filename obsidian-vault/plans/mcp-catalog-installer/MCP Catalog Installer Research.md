---
title: MCP Catalog Installer Research Synthesis
tags:
  - mcp
  - ux
  - research
date: 2026-04-18
status: complete
effort: 0.5d
priority: high
---

# MCP Catalog Installer — Research Synthesis

> Synthesis of how existing MCP clients (Claude Desktop, Cursor, OpenAI Agents SDK, Smithery, mcp-agent) solve server-install UX, and what applies to AgentOS.

---

## Problem

Attaching an MCP server to AgentOS today (circa 2026-04-18) requires six manual steps:

1. Find the npm/pypi package name for the MCP server.
2. Install it globally (`npm install -g …`).
3. Locate the binary (breaks under nvm/volta/asdf).
4. Reconcile runtime version (system node v12 vs nvm v20 was the real incident).
5. Run OAuth helper out-of-band and save credentials.
6. Construct the full `agentos mcp attach` command.

A single typo or version mismatch manifests as `"MCP server closed connection unexpectedly"`. This is below the bar set by the wider MCP ecosystem.

---

## What the ecosystem does

### OpenAI Agents SDK — Hosted MCP tools
- User provides only a URL to a managed MCP server.
- OpenAI operates the runtime; no local install, no binary paths, no env vars.
- **Applies to AgentOS?** Yes, partially — the HTTP transport (`--url`) already covers this. We should promote it more prominently in docs.

### mcp-agent — `uvx` runner
- Commands like `uvx mcp-agent init` / `uvx mcp-agent deploy my-agent`.
- `uvx` runs Python tools in ephemeral, isolated environments — no global installs.
- **Applies to AgentOS?** The pattern (ephemeral install + execute) is worth adopting. `npx -y` is the JS equivalent; AgentOS can invoke either from a catalog entry.

### Google ADK — MCP Toolbox
- Curated catalog of production MCP servers for BigQuery, Spanner, Cloud SQL, etc.
- Users pick from a known-good list rather than searching npm.
- **Applies to AgentOS?** Directly — we already have `plugins/core/` for channel plugins. Extend the same pattern for MCP.

### Smithery (smithery.ai)
- Public MCP server registry with YAML metadata.
- Includes a browser-installable UI and a CLI for adding servers.
- **Applies to AgentOS?** Registry format is importable. Phase 7+ could add a Smithery → AgentOS catalog converter.

### Anthropic DXT (Desktop Extensions)
- `.dxt` bundles containing the server code + manifest + optional runtime, installable with one click.
- **Applies to AgentOS?** Long-term. Not in the critical path, but the bundle format is interesting for offline/air-gapped deployments.

---

## What already works in AgentOS

| Primitive | Purpose | Applicable |
|-----------|---------|------------|
| `PluginManifest` (`plugin.toml`) | Describes channel plugins in `plugins/core/` | ✅ Extend for MCP |
| `PluginRegistry` | Loads and validates manifests at boot | ✅ Reuse |
| `rust-embed` of `config/` and `skills/core/` | Ships curated artifacts in the binary | ✅ Embed `plugins/mcp-catalog/` |
| `TrustTier` enum (Core / Verified / Community / Blocked) | Security classification | ✅ Reuse |
| Ed25519 signing (`agentos-tools/src/signing.rs`) | Sign/verify manifest payloads | ✅ Reuse for `verified`-tier entries |
| `mcp attach` / `mcp detach` CLI | Runtime attach flow | ✅ Reuse end-to-end |
| `mcp_attachments.db` | Persist attached servers across restarts | ✅ No change needed |
| `agentos onboard` / `doctor` | Interactive wizards w/ `dialoguer` | ✅ Reuse patterns for install prompts |
| Stderr capture in stdio transport | Surface crash reasons | ✅ Shipped 2026-04-18 — install errors will be visible |
| Vault (AES-256-GCM) | Encrypted credential storage | ✅ Use for OAuth credential paths |

---

## Gaps to fill

1. **No runtime resolver.** Kernel inherits `PATH` but doesn't understand nvm/volta/asdf shims.
2. **No MCP-specific manifest format.** Current `plugin.toml` describes channels, not MCP servers.
3. **No first-run package prefetch.** `npx -y` cold start exceeds the 30s attach timeout.
4. **No OAuth helper orchestration.** User runs it manually in another terminal.
5. **No one-command UX.** Everything is a multi-step ceremony.

---

## Design implications

- **Keep the catalog close to the binary.** Embed built-ins via `rust-embed` so `agentos mcp install gmail` works on a fresh install with no network dependency beyond the package itself.
- **Reuse `mcp attach`.** Do not build a parallel attachment path. The install command is a resolver + prefetcher + credentials-checker that calls the existing attach flow at the end.
- **Fail loud, not silent.** Runtime resolver logs the chosen binary; OAuth helper runs in foreground; stderr capture is already wired.
- **Trust tiers for catalog entries, not just tools.** A `"community"` catalog entry should require explicit confirmation before it spawns a subprocess on the host.
- **Do not over-engineer authn.** OAuth helper is just "run this command, wait for it to exit, check the credentials path appeared." Support the common case (browser flow writes a JSON file) without trying to handle every provider's eccentricity.

---

## Supply-chain audit of candidate servers (2026-04-18)

Before seeding the catalog, each candidate MCP server is audited for maintenance, security posture, and token handling. First audit result:

### `@gongrzhe/server-gmail-autoauth-mcp` — REJECTED

Despite being the most popular Gmail MCP server on npm (~83k weekly downloads), it was rejected on the following grounds:

- Upstream GitHub repo is **archived** (read-only, no future fixes).
- OAuth refresh tokens stored as **plaintext JSON on disk** (`~/.gmail-mcp/credentials.json`). No encryption, no keychain, no file-mode hardening.
- OAuth scopes hardcoded to `gmail.modify` + `gmail.settings.basic` — full mailbox + filter control, no read-only path.
- 71 open issues, 5 open PRs, last commit 2025-08-06, maintainer disengaged.
- No tests, no CI, license mismatch (MIT in LICENSE, ISC in package.json).
- MCP SDK pinned to pre-1.0 (`^0.4.0`) — will not receive protocol updates.
- Open, unaddressed bugs in `send_email` argument validation and reply threading.

### `taylorwilsdon/google_workspace_mcp` — ACCEPTED as replacement

2,139 stars, actively maintained (commits within the audit window), covers Gmail + Calendar + Drive + Docs in a single server, Python-based, supports per-service scopes so a read-only catalog entry is feasible.

### Audit checklist for all future catalog seed candidates

Every proposed `trust_tier = "verified"` catalog entry must pass:

1. Upstream repo is not archived or abandoned (commit in last 90 days).
2. Token storage documented and reviewable; must not leak secrets into world-readable paths by default.
3. OAuth scope configurability (read-only path available) for any service with a meaningful read/write distinction.
4. MCP SDK version within 1 minor of the current release line.
5. Either tests + CI, or a sufficiently small and auditable codebase to review manually.
6. Maintainer identity verifiable (org, recognized individual, or signed releases).

Community-tier candidates get a laxer check but still must demonstrate active maintenance. Anything failing these is listed in the research doc and explicitly not seeded.

---

## Non-goals (for this plan)

- Running MCP servers inside WASM sandboxes. (Valuable, but belongs in a separate plan under `agentos-wasm`.)
- Hosting AgentOS's own public MCP registry. (Future work — we start with built-in + user-local catalogs.)
- Replacing existing `PluginManifest` for channels. Channels and MCP servers remain distinct.

---

## Related

- [[MCP Catalog Installer Plan]]
- [[MCP Catalog Installer Data Flow]]
- [[agentos-mcp-runtime-attach]]
