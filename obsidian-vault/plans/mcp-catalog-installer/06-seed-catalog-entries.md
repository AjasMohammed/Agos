---
title: Phase 6 — Seed Catalog Entries
tags:
  - mcp
  - catalog
  - content
  - phase-6
date: 2026-04-18
status: planned
effort: 1d
priority: medium
---

# Phase 6 — Seed Catalog Entries

> Author the first batch of vetted catalog entries so `agentos mcp install <name>` has useful options from day one.

---

## Why this phase

The catalog mechanism (Phases 1–5) is worthless without entries. This phase ships eight hand-authored, tested entries covering the most common MCP use cases, signed where the trust tier requires it.

Targets for the first release:

| id | Transport | Runtime | Auth | Trust tier |
|-----|-----------|---------|------|-----------|
| `google-workspace` | stdio | python >= 3.11 | oauth (google) | verified |
| `github` | stdio | node >= 18 | api_key (`GITHUB_TOKEN`) | verified |
| `filesystem` | stdio | node >= 18 | none | verified |
| `slack` | stdio | node >= 18 | api_key (`SLACK_BOT_TOKEN`) | verified |
| `postgres` | stdio | node >= 18 | connection string in env | verified |
| `sqlite` | stdio | node >= 18 | none | verified |
| `brave-search` | stdio | node >= 18 | api_key (`BRAVE_API_KEY`) | verified |
| `puppeteer` | stdio | node >= 18 | none | community |

`puppeteer` starts as `community` because it pulls in a Chromium download; the trust-tier confirmation prompt is appropriate.

### Do NOT seed `@gongrzhe/server-gmail-autoauth-mcp`

The initial Gmail entry proposed in earlier drafts pointed at `@gongrzhe/server-gmail-autoauth-mcp`. A 2026-04-18 audit disqualified it:

- Upstream GitHub repo is **archived** — no future fixes will land.
- OAuth refresh tokens are written to disk as **plaintext JSON** (`~/.gmail-mcp/credentials.json`), no encryption or keychain.
- OAuth scopes hardcoded to `gmail.modify` + `gmail.settings.basic` — no read-only path.
- 71 open issues / 5 open PRs all abandoned. MCP SDK pinned to pre-1.0 (`^0.4.0`).
- No tests, no CI, license mismatch (MIT vs ISC).

We seed **`taylorwilsdon/google_workspace_mcp`** instead: actively maintained, covers Gmail + Calendar + Drive + Docs in a single server, Python-based (requires runtime_resolver python support from Phase 1). AgentOS-specific extra: force the server's credentials directory to live under the encrypted `agentos-vault` path instead of `~/.google-workspace-mcp/` where possible; if not possible at the MCP server level, document the plaintext-on-disk caveat in the install prompt.

---

## Current → Target

**Current:** `plugins/mcp-catalog/` is empty (created in Phase 2).

**Target:** 8 `*.toml` entries that install cleanly via `agentos mcp install <id>`, each with an Ed25519 signature where `trust_tier = "verified"`.

---

## Detailed subtasks

### 1. Draft each entry

Author in this order — easiest first so edge cases surface early:

1. `filesystem.toml` — no auth, no env vars.
2. `sqlite.toml` — no auth, single env var for DB path.
3. `github.toml` — api-key auth.
4. `brave-search.toml` — api-key auth.
5. `postgres.toml` — env-var-based connection string.
6. `slack.toml` — api-key auth + socket mode considerations.
7. `google-workspace.toml` — full OAuth helper flow via `taylorwilsdon/google_workspace_mcp` (replaces rejected `@gongrzhe` server).
8. `puppeteer.toml` — community tier (pulls Chromium).

### 2. Example: `filesystem.toml`

```toml
id = "filesystem"
display_name = "Filesystem"
version = "1.0.0"
description = "Read and write files under a declared directory"
homepage = "https://github.com/modelcontextprotocol/servers/tree/main/src/filesystem"
trust_tier = "verified"
author_pubkey = "<agentos-release-pubkey>"
signature = "<ed25519 sig over canonical payload>"

[mcp]
transport = "stdio"
runtime = "node"
runtime_min_version = "18"

[mcp.install]
strategy = "npx"
package = "@modelcontextprotocol/server-filesystem"
package_version = "0.6.2"
entry_js = "dist/index.js"
prefetch_timeout_secs = 180

[[mcp.install.args]]
value = "{home}/Documents"   # default root; user overrides with --arg or user-catalog entry

[mcp.env]
# none

[mcp.auth]
type = "none"

[mcp.tools]
default_risk_class = "readonly_scoped"

[mcp.tools.overrides]
write_file = "write_scoped"
create_directory = "write_scoped"
move_file = "write_scoped"
edit_file = "write_scoped"
```

### 3. Example: `google-workspace.toml`

Replaces the abandoned `@gongrzhe/server-gmail-autoauth-mcp` entry. Uses the actively-maintained `taylorwilsdon/google_workspace_mcp` (Python, covers Gmail + Calendar + Drive + Docs):

```toml
id = "google-workspace"
display_name = "Google Workspace"
version = "1.0.0"
description = "Gmail + Calendar + Drive + Docs via the actively-maintained google_workspace_mcp"
homepage = "https://github.com/taylorwilsdon/google_workspace_mcp"
trust_tier = "verified"
author_pubkey = "<agentos-release-pubkey>"
signature = "<ed25519 sig>"

[mcp]
transport = "stdio"
runtime = "python"
runtime_min_version = "3.11"
timeout_secs = 30
rate_limit_rpm = 30

[mcp.install]
strategy = "pip"
package = "google-workspace-mcp"   # verify exact pypi name during implementation
package_version = "*"
module = "google_workspace_mcp"
prefetch_timeout_secs = 300

[mcp.env]
# Exact env vars depend on package README; verify at implementation time.
GOOGLE_OAUTH_CLIENT_SECRETS = "{home}/.config/agentos/google-workspace/oauth-client.json"
GOOGLE_OAUTH_TOKEN_PATH = "{home}/.config/agentos/google-workspace/token.json"

[mcp.auth]
type = "oauth"
provider = "google"
helper_command = "python"
helper_args = ["-m", "google_workspace_mcp.auth"]
credentials_path = "{home}/.config/agentos/google-workspace/token.json"
helper_requires_browser = true

[mcp.tools]
default_risk_class = "exec_capable"

[mcp.tools.overrides]
# Gmail
gmail_search = "readonly_external"
gmail_read = "readonly_external"
gmail_list_labels = "readonly_external"
gmail_send = "exec_capable"
gmail_delete = "exec_capable"
# Calendar
calendar_list_events = "readonly_external"
calendar_create_event = "write_scoped"
calendar_delete_event = "exec_capable"
# Drive
drive_list = "readonly_external"
drive_read_file = "readonly_external"
drive_upload = "write_scoped"
drive_delete = "exec_capable"
# Docs
docs_read = "readonly_external"
docs_create = "write_scoped"
docs_edit = "write_scoped"
```

**Hardening notes for enterprise deployments:**

1. **GCP credentials are still required out-of-band** — the user creates a GCP project, enables Gmail/Calendar/Drive/Docs APIs, downloads `oauth-client.json`. Document clearly in the install prompt.
2. **Least-privilege scopes:** unlike the archived `gongrzhe` server, `taylorwilsdon/google_workspace_mcp` supports per-service scopes. Our catalog entry should configure read-only scopes by default; declare a separate `google-workspace-write` catalog entry for deployments that need send/delete.
3. **Vault integration:** at install time, optionally move the OAuth token from `{home}/.config/agentos/google-workspace/token.json` into `agentos-vault` under key `google_workspace.token` and rewrite the env var as `vault:google_workspace.token`. The MCP server still reads a file path — write the vault contents to a tmpfs at server start and revoke on detach. (Implementation detail for Phase 4 polish.)
4. **Risk classes are conservative:** every write/delete tool is `write_scoped` or `exec_capable`, so ApprovalHook prompts the user before any mailbox, calendar, or drive modification.

### 4. Example: `github.toml`

```toml
id = "github"
display_name = "GitHub"
version = "1.0.0"
description = "Inspect repos, manage issues and PRs via the GitHub API"
homepage = "https://github.com/github/github-mcp-server"
trust_tier = "verified"
author_pubkey = "<agentos-release-pubkey>"
signature = "<ed25519 sig>"

[mcp]
transport = "stdio"
runtime = "node"
runtime_min_version = "18"

[mcp.install]
strategy = "npx"
package = "@modelcontextprotocol/server-github"
package_version = "0.7.0"
entry_js = "dist/index.js"
prefetch_timeout_secs = 180

[mcp.env]
GITHUB_PERSONAL_ACCESS_TOKEN = "vault:GITHUB_TOKEN"

[mcp.auth]
type = "api_key"
env_var = "GITHUB_TOKEN"

[mcp.tools]
default_risk_class = "readonly_external"

[mcp.tools.overrides]
create_issue = "write_scoped"
update_issue = "write_scoped"
create_pull_request = "write_scoped"
merge_pull_request = "exec_capable"
delete_file = "exec_capable"
```

### 5. Signing the entries

Reuse existing signing infrastructure:

```bash
# Build-time step (part of release pipeline):
agentos tool sign \
  --key-file /path/to/agentos-release.ed25519 \
  --input plugins/mcp-catalog/gmail.toml \
  --field-under "signature"
```

Validation happens in `CatalogRegistry::load` for `trust_tier = "verified"`. `"core"` tier is reserved for manifests embedded inside the binary signed with the agentos release key.

### 6. Testing each entry end-to-end

For every entry:

1. Start a fresh kernel.
2. Run `agentos mcp install <id>` (with `--yes` and a pre-provisioned credentials file for auth entries).
3. Verify `agentos mcp tools | grep <id>` lists the expected tools.
4. Call at least one readonly tool and verify it works.

For `gmail` and `github`, CI uses pre-provisioned test accounts; for `filesystem` and `sqlite` there's nothing external.

### 7. Documentation

Create `obsidian-vault/reference/MCP Catalog.md` listing all entries, their purpose, and known limitations. Link from the MCP section of the user-facing CLI guide.

---

## Files changed

| File | Change |
|------|--------|
| `plugins/mcp-catalog/google-workspace.toml` | New (replaces gmail — see rejected section) |
| `plugins/mcp-catalog/github.toml` | New |
| `plugins/mcp-catalog/filesystem.toml` | New |
| `plugins/mcp-catalog/slack.toml` | New |
| `plugins/mcp-catalog/postgres.toml` | New |
| `plugins/mcp-catalog/sqlite.toml` | New |
| `plugins/mcp-catalog/brave-search.toml` | New |
| `plugins/mcp-catalog/puppeteer.toml` | New |
| `obsidian-vault/reference/MCP Catalog.md` | New reference doc |

---

## Dependencies

- **Requires:** Phases 1–5 (runtime resolver, catalog registry, install command, OAuth automation).
- **Blocks:** Public release of the feature.

---

## Test plan

Per entry (smoke tests, run in CI with networked harness):

1. `agentos mcp install <id>` succeeds.
2. `agentos mcp status` shows the server as connected.
3. At least one read-only tool call succeeds.
4. `agentos mcp uninstall <id>` cleanly removes.

Aggregate test: install all 8 back-to-back, verify no collisions in cache/env/credentials paths.

---

## Verification

```bash
# Catalog loads without errors
cargo test -p agentos-kernel catalog::load_seed_entries

# Signature verification passes
cargo test -p agentos-kernel catalog::verified_entries_all_valid

# Manual install smoke (each entry):
for id in filesystem sqlite github brave-search postgres slack puppeteer google-workspace; do
    agentos mcp install "$id" --yes
    agentos mcp tools | grep "$id"
    agentos mcp uninstall "$id" --purge
done
```

---

## Related

- [[MCP Catalog Installer Plan]]
- [[02-catalog-format-and-registry]]
- [[04-install-command]]
- [[05-oauth-helper-automation]]
