#!/usr/bin/env bash
# dev.sh — Build and run AgentOS locally (kernel + web UI in one process).
# No Docker needed. Uses config/default.toml with ephemeral /tmp paths.
#
# Usage:
#   ./dev.sh
#   AGENTOS_PORT=9090 ./dev.sh
#   AGENTOS_CONFIG=config/docker.toml ./dev.sh
#   AGENTOS_VAULT_PASSPHRASE=mypass ./dev.sh
set -euo pipefail

# Always run from the repo root regardless of where the script is called from
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

# --- Config (override via env vars) ---
HOST="${AGENTOS_HOST:-127.0.0.1}"
PORT="${AGENTOS_PORT:-8080}"
CONFIG="${AGENTOS_CONFIG:-config/default.toml}"
# Vault passphrase — set this to your dev passphrase to skip the interactive prompt
export AGENTOS_VAULT_PASSPHRASE="${AGENTOS_VAULT_PASSPHRASE:-devpass}"
# Set AGENTOS_NO_CLEAN=1 to keep /tmp/agentos state across restarts
AGENTOS_NO_CLEAN="${AGENTOS_NO_CLEAN:-1}"

# Wipe ephemeral runtime state (/tmp/agentos) so the vault passphrase always matches.
# All data there is intentionally temporary — skip with AGENTOS_NO_CLEAN=1.
if [[ "$AGENTOS_NO_CLEAN" != "1" ]]; then
  echo "==> Cleaning /tmp/agentos (set AGENTOS_NO_CLEAN=1 to skip)"
  rm -rf /tmp/agentos
fi

# Seed tool manifests from the repo's tools/core/ into the runtime tools directory.
# The kernel resolves tools from core_tools_dir at startup; without these it won't boot.
mkdir -p /tmp/agentos/tools/core /tmp/agentos/tools/user /tmp/agentos/data /tmp/agentos/vault
cp -r "$SCRIPT_DIR"/tools/core/. /tmp/agentos/tools/core/

# Build only the CLI crate for fast incremental rebuilds
echo "==> Building AgentOS..."
cargo build -p agentos-cli 2>&1

echo ""
echo "==> Starting AgentOS (kernel + web UI)"
echo "    Config : $CONFIG"
echo "    Web UI : http://$HOST:$PORT"
echo ""

# `web serve` boots the kernel internally and starts the web server in one process.
# Ctrl+C triggers graceful shutdown of both.
exec ./target/debug/agentctl --config "$CONFIG" web serve --host "$HOST" --port "$PORT"
