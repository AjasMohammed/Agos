#!/usr/bin/env bash
#
# e2e-smoke.sh — local-host smoke test for a built AgentOS binary.
#
# Verifies that a freshly built `agentos` binary and the v1.0.0 release/deploy
# artifacts are present and self-consistent. This is the offline, no-daemon
# portion of Phase 10 (end-to-end verification) — it does NOT install from a
# public channel or boot a long-lived kernel; for the full clean-VM matrix see
# obsidian-vault/plans/production-release-v1/E2E-LAUNCH-MATRIX.md and the
# agentos-agent-tester harness.
#
# Usage:
#   AGENTOS_BIN=./target/release/agentos scripts/e2e-smoke.sh
#   scripts/e2e-smoke.sh                 # defaults to ./target/release/agentos
#
# Idempotent and safe to run repeatedly: it never writes outside a temp dir and
# never starts a kernel. Each check prints PASS / FAIL / SKIP and the script
# ends with a summary count. Exit code is non-zero iff any check FAILs.
#
# Checks that need a running daemon (boot, /healthz, task round-trip, web chat,
# gateway run-as-bot, mcp install, self-update) are intentionally out of scope
# here — they live in the launch matrix and run on clean VMs.

set -euo pipefail

# Resolve repo root (this script lives in <root>/scripts).
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$ROOT_DIR"

AGENTOS_BIN="${AGENTOS_BIN:-./target/release/agentos}"

PASS=0
FAIL=0
SKIP=0

c_green="$(printf '\033[1;32m')"
c_red="$(printf '\033[1;31m')"
c_yellow="$(printf '\033[1;33m')"
c_reset="$(printf '\033[0m')"
# Disable colour when not writing to a terminal.
if [ ! -t 1 ]; then c_green=""; c_red=""; c_yellow=""; c_reset=""; fi

pass() { PASS=$((PASS + 1)); printf '  %sPASS%s %s\n' "$c_green" "$c_reset" "$1"; }
fail() { FAIL=$((FAIL + 1)); printf '  %sFAIL%s %s\n' "$c_red"   "$c_reset" "$1"; }
skip() { SKIP=$((SKIP + 1)); printf '  %sSKIP%s %s\n' "$c_yellow" "$c_reset" "$1"; }

section() { printf '\n%s\n' "$1"; }

# exists <path> <label>
exists() {
  if [ -e "$1" ]; then pass "$2 ($1)"; else fail "$2 missing: $1"; fi
}

printf 'AgentOS e2e smoke — binary: %s\n' "$AGENTOS_BIN"

# ── 0. Binary present ─────────────────────────────────────────────────────────
section "[0] Binary"
if [ ! -x "$AGENTOS_BIN" ]; then
  # Resolve via PATH as a fallback (e.g. installed binary).
  if command -v "$AGENTOS_BIN" >/dev/null 2>&1; then
    AGENTOS_BIN="$(command -v "$AGENTOS_BIN")"
    pass "binary resolved on PATH ($AGENTOS_BIN)"
  else
    fail "binary not found or not executable: $AGENTOS_BIN"
    printf '\nCannot continue without a binary. Build it first:\n'
    printf '  cargo build --release   # or set AGENTOS_BIN to a built binary\n\n'
    printf 'Summary: %d PASS, %d FAIL, %d SKIP\n' "$PASS" "$FAIL" "$SKIP"
    exit 1
  fi
else
  pass "binary is executable ($AGENTOS_BIN)"
fi

# ── 1. Version ────────────────────────────────────────────────────────────────
section "[1] Version"
if ver="$("$AGENTOS_BIN" --version 2>/dev/null)" && [ -n "$ver" ]; then
  pass "--version prints: $ver"
else
  fail "--version did not print a version"
fi

# ── 2. Doctor (exit 0 or warn-only) ───────────────────────────────────────────
section "[2] Doctor"
# `doctor` exits 0 even with WARN lines (e.g. missing data dirs); a non-zero
# exit indicates a hard error. Capture output but don't let set -e abort us.
if doctor_out="$("$AGENTOS_BIN" doctor 2>&1)"; then
  pass "doctor ran (exit 0; warnings tolerated)"
else
  # Hard failures only. WARN-only runs still exit 0.
  fail "doctor exited non-zero"
  printf '%s\n' "$doctor_out" | sed 's/^/      /'
fi

# ── 3. Config parse: production.toml ──────────────────────────────────────────
section "[3] Production config parses"
# `doctor --config config/production.toml` should parse the file. Directory
# permission / existence WARNs are acceptable; we only fail on a parse error or
# a non-zero exit.
if [ -f config/production.toml ]; then
  if prod_out="$("$AGENTOS_BIN" --config config/production.toml doctor 2>&1)"; then
    if printf '%s\n' "$prod_out" | grep -qiE 'invalid|parse error|failed to parse'; then
      fail "production.toml reported a parse error"
      printf '%s\n' "$prod_out" | sed 's/^/      /'
    else
      pass "production.toml parses (dir warnings ignored)"
    fi
  else
    fail "doctor --config config/production.toml exited non-zero"
    printf '%s\n' "$prod_out" | sed 's/^/      /'
  fi
else
  fail "config/production.toml not found"
fi

# ── 4. Installer is syntactically valid ───────────────────────────────────────
section "[4] Installer syntax"
if [ -f scripts/install.sh ]; then
  if bash -n scripts/install.sh; then
    pass "scripts/install.sh is valid bash (bash -n)"
  else
    fail "scripts/install.sh has a syntax error"
  fi
else
  fail "scripts/install.sh not found"
fi

# ── 5. Gateway command exists ─────────────────────────────────────────────────
section "[5] Gateway command"
if "$AGENTOS_BIN" gateway --help >/dev/null 2>&1; then
  pass "agentos gateway --help"
else
  fail "agentos gateway --help (gateway-first deploy command missing)"
fi
if "$AGENTOS_BIN" gateway run --help >/dev/null 2>&1; then
  pass "agentos gateway run --help"
else
  fail "agentos gateway run --help"
fi

# ── 6. MCP command exists ─────────────────────────────────────────────────────
section "[6] MCP command"
if "$AGENTOS_BIN" mcp --help >/dev/null 2>&1; then
  pass "agentos mcp --help"
else
  fail "agentos mcp --help"
fi

# ── 7. Deploy & packaging artifacts ───────────────────────────────────────────
section "[7] Deploy & packaging artifacts"
exists deploy/agentos.service                          "systemd unit (kernel)"
exists deploy/agentos-gateway.service                  "systemd unit (gateway-first)"
exists Dockerfile                                      "Dockerfile"
exists docker-compose.yml                              "docker-compose (kernel)"
exists docker-compose.gateway.yml                      "docker-compose (gateway)"
exists deploy/helm                                     "Helm chart dir"
exists deny.toml                                       "cargo-deny policy"
exists deploy/observability                            "observability dir"
exists deploy/observability/prometheus.yml             "Prometheus scrape config"
exists deploy/observability/docker-compose.observability.yml "observability compose"
exists deploy/observability/grafana-dashboard-agentos.json   "Grafana dashboard"
exists scripts/install.sh                              "curl|bash installer (sh)"
exists scripts/install.ps1                             "Windows installer (ps1, beta)"
exists packaging/homebrew/agentos.rb                   "Homebrew formula"
exists LICENSE                                         "Apache-2.0 LICENSE"
exists CHANGELOG.md                                    "CHANGELOG"
exists SECURITY.md                                     "security policy"

# ── 7b. Compose self-consistency: no host Docker socket by default ────────────
section "[7b] Compose security self-consistency"
# The default stack must NOT bind-mount the host Docker socket — doing so grants
# any LLM-driven agent effective root on the host. An uncommented list entry
# (line starting with '-', not '#') is the regression we guard against.
for compose in docker-compose.yml docker-compose.gateway.yml; do
  if [ -f "$compose" ]; then
    if grep -qE '^[[:space:]]*-[[:space:]]*/var/run/docker\.sock' "$compose"; then
      fail "$compose mounts host docker.sock by default (host-root escape)"
    else
      pass "$compose does not mount host docker.sock by default"
    fi
  fi
done

# Release build targets: linux musl was abandoned because ort/onnx ships no musl
# binary and openssl-sys is unvendored (musl release builds fail). Guard against
# a regression back to musl in the release workflow.
RELEASE_YML=".github/workflows/release.yml"
if [ -f "$RELEASE_YML" ]; then
  if grep -qE 'unknown-linux-musl' "$RELEASE_YML"; then
    fail "release.yml targets linux-musl (ort/openssl can't build on musl — use linux-gnu)"
  else
    pass "release.yml uses glibc/gnu linux targets (no musl)"
  fi
fi

# ── 8. systemd unit syntax (best-effort) ──────────────────────────────────────
section "[8] systemd unit syntax"
if command -v systemd-analyze >/dev/null 2>&1; then
  for unit in deploy/agentos.service deploy/agentos-gateway.service; do
    if systemd-analyze verify "$unit" >/dev/null 2>&1; then
      pass "systemd-analyze verify $unit"
    else
      # verify warns about absolute ExecStart paths on a non-installed unit;
      # treat only a hard non-zero with errors as a failure.
      if systemd-analyze verify "$unit" 2>&1 | grep -qiE 'error|invalid|bad'; then
        fail "systemd-analyze verify $unit reported errors"
      else
        pass "systemd-analyze verify $unit (warnings only)"
      fi
    fi
  done
else
  skip "systemd-analyze not installed (run on a clean Linux VM)"
fi

# ── 9. Docs site structure: every SUMMARY entry has a file ────────────────────
section "[9] Docs site (mdBook SUMMARY → files)"
SUMMARY="docs/book/src/SUMMARY.md"
if [ -f "$SUMMARY" ]; then
  src_dir="$(dirname "$SUMMARY")"
  missing=0
  total=0
  # Extract markdown link targets that point at local .md files.
  while IFS= read -r target; do
    [ -z "$target" ] && continue
    total=$((total + 1))
    # Strip any in-page anchor (#section).
    rel="${target%%#*}"
    if [ ! -f "$src_dir/$rel" ]; then
      fail "SUMMARY references missing page: $rel"
      missing=$((missing + 1))
    fi
  done < <(grep -oE '\]\(\.?/?[^)]+\.md[^)]*\)' "$SUMMARY" \
             | sed -E 's/^\]\(//; s/\)$//; s/^\.\///')
  if [ "$missing" -eq 0 ]; then
    pass "all $total SUMMARY.md entries resolve to files"
  fi
  exists docs/book/book.toml "mdBook config"
else
  fail "docs/book/src/SUMMARY.md not found"
fi

# ── Summary ───────────────────────────────────────────────────────────────────
section "Summary"
printf '  %sPASS%s=%d  %sFAIL%s=%d  %sSKIP%s=%d\n' \
  "$c_green" "$c_reset" "$PASS" \
  "$c_red"   "$c_reset" "$FAIL" \
  "$c_yellow" "$c_reset" "$SKIP"

if [ "$FAIL" -gt 0 ]; then
  printf '%sE2E SMOKE FAILED%s (%d check(s) failed)\n' "$c_red" "$c_reset" "$FAIL"
  exit 1
fi
printf '%sE2E SMOKE PASS%s\n' "$c_green" "$c_reset"
