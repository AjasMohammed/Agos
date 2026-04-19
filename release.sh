#!/usr/bin/env bash
# release.sh — Build and run AgentOS in release mode directly on the host.
# Full hardware access, real filesystem paths, no Docker isolation.
#
# Usage:
#   ./release.sh                                    # defaults
#   ./release.sh --clean                            # wipe data and start fresh
#   AGENTOS_PORT=9090 ./release.sh                  # custom port
#   AGENTOS_VAULT_PASSPHRASE=mypass ./release.sh    # skip vault prompt
#   AGENTOS_LLM_PROVIDER=anthropic ./release.sh     # use cloud LLM
#
# Prerequisites:
#   - Rust 1.91+ (with clang + mold linker)
#   - bubblewrap (apt install bubblewrap)
#   - ca-certificates
#   - Ollama running locally OR a cloud LLM API key in vault
#
# Optional (for HAL hardware access):
#   - pipewire + pipewire-tools   (audio)
#   - bluez + dbus                (bluetooth)
#   - x11-xserver-utils           (display)
#   - v4l-utils                   (webcam)
set -euo pipefail

# Always run from the repo root
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

# ─── Configuration (override via env vars) ───────────────────────────────────

HOST="${AGENTOS_HOST:-127.0.0.1}"
PORT="${AGENTOS_PORT:-8080}"
DATA_DIR="${AGENTOS_DATA_DIR:-$HOME/.agentos}"
export AGENTOS_VAULT_PASSPHRASE="${AGENTOS_VAULT_PASSPHRASE:-devpass}"
export AGENTOS_AUTO_INIT_VAULT="${AGENTOS_AUTO_INIT_VAULT:-true}"
EXTRA_FEATURES="${AGENTOS_FEATURES:-otel}"

# ─── Parse flags ─────────────────────────────────────────────────────────────

CLEAN=0
SKIP_BUILD=0
for arg in "$@"; do
  case "$arg" in
    --clean)  CLEAN=1 ;;
    --skip-build) SKIP_BUILD=1 ;;
    --help|-h)
      echo "Usage: ./release.sh [--clean] [--skip-build]"
      echo ""
      echo "  --clean       Wipe $DATA_DIR and start fresh"
      echo "  --skip-build  Skip cargo build (use existing binary)"
      echo ""
      echo "Environment variables:"
      echo "  AGENTOS_HOST              Listen address (default: 127.0.0.1)"
      echo "  AGENTOS_PORT              Listen port (default: 8080)"
      echo "  AGENTOS_DATA_DIR          Data directory (default: ~/.agentos)"
      echo "  AGENTOS_VAULT_PASSPHRASE  Vault passphrase (default: devpass)"
      echo "  AGENTOS_FEATURES          Extra cargo features (default: otel)"
      echo "  AGENTOS_OLLAMA_HOST       Ollama endpoint (default: http://localhost:11434)"
      echo "  AGENTOS_LLM_URL           Custom LLM endpoint"
      exit 0
      ;;
  esac
done

# ─── Preflight checks ───────────────────────────────────────────────────────

check_cmd() {
  if ! command -v "$1" &>/dev/null; then
    echo "ERROR: '$1' is not installed. $2"
    return 1
  fi
}

echo "==> Preflight checks"
MISSING=0
check_cmd cargo    "Install Rust: https://rustup.rs"             || MISSING=1
check_cmd clang    "apt install clang"                            || MISSING=1
check_cmd mold     "apt install mold"                             || MISSING=1
check_cmd bwrap    "apt install bubblewrap (required for shell-exec sandbox)" || MISSING=1

if [[ $MISSING -eq 1 ]]; then
  echo ""
  echo "Install missing dependencies and re-run."
  exit 1
fi

# Optional checks (warn but don't block)
echo ""
echo "    Hardware drivers (optional):"
command -v pw-cli       &>/dev/null && echo "    [ok] pipewire (audio)"       || echo "    [--] pipewire not found (audio HAL disabled)"
command -v bluetoothctl &>/dev/null && echo "    [ok] bluez (bluetooth)"      || echo "    [--] bluez not found (bluetooth HAL disabled)"
command -v xrandr       &>/dev/null && echo "    [ok] xrandr (display)"       || echo "    [--] xrandr not found (display HAL limited)"
command -v v4l2-ctl     &>/dev/null && echo "    [ok] v4l-utils (webcam)"     || echo "    [--] v4l-utils not found (webcam HAL disabled)"
command -v lsusb        &>/dev/null && echo "    [ok] usbutils (usb)"         || echo "    [--] usbutils not found (USB HAL disabled)"
echo ""

# Check Ollama
OLLAMA_HOST="${AGENTOS_OLLAMA_HOST:-http://localhost:11434}"
if curl -sf "$OLLAMA_HOST/api/tags" &>/dev/null; then
  echo "    [ok] Ollama reachable at $OLLAMA_HOST"
else
  echo "    [!!] Ollama not reachable at $OLLAMA_HOST"
  echo "         Local inference won't work. Set AGENTOS_LLM_URL for cloud LLM."
fi
echo ""

# ─── Clean if requested ─────────────────────────────────────────────────────

if [[ $CLEAN -eq 1 ]]; then
  echo "==> Cleaning $DATA_DIR"
  rm -rf "$DATA_DIR"
fi

# ─── Create directory structure ──────────────────────────────────────────────

mkdir -p \
  "$DATA_DIR/data" \
  "$DATA_DIR/data/models" \
  "$DATA_DIR/vault" \
  "$DATA_DIR/tools/core" \
  "$DATA_DIR/tools/user" \
  "$DATA_DIR/plugins/core" \
  "$DATA_DIR/plugins/user" \
  "$DATA_DIR/static" \
  "$DATA_DIR/logs"

# Seed core tool manifests
cp -r "$SCRIPT_DIR"/tools/core/. "$DATA_DIR/tools/core/"

# Seed core plugin manifests if they exist
if [[ -d "$SCRIPT_DIR/plugins/core" ]]; then
  cp -r "$SCRIPT_DIR"/plugins/core/. "$DATA_DIR/plugins/core/"
fi

# Copy web UI static assets if they exist
if [[ -d "$SCRIPT_DIR/crates/agentos-web/static" ]]; then
  cp -r "$SCRIPT_DIR"/crates/agentos-web/static/. "$DATA_DIR/static/"
fi

# ─── Generate host config ───────────────────────────────────────────────────

CONFIG="$DATA_DIR/config.toml"

# Only generate config if it doesn't exist (preserve user edits)
if [[ ! -f "$CONFIG" ]]; then
  echo "==> Generating config at $CONFIG"
  cat > "$CONFIG" << TOML
# AgentOS host-native configuration (generated by release.sh)
# Edit this file to customize. Re-run with --clean to regenerate.

[kernel]
max_concurrent_tasks = 8
default_task_timeout_secs = 3600
context_window_max_entries = 500
context_window_token_budget = 128000
state_db_path = "$DATA_DIR/data/kernel_state.db"
sandbox_policy = "trust_aware"

[kernel.task_limits]
max_iterations_low = 50
max_iterations_medium = 200
max_iterations_high = 1000

[kernel.tool_calls]
allow_parallel = true
max_parallel = 10

[kernel.autonomous_mode]
max_iterations = 10000
task_timeout_secs = 86400
tool_timeout_seconds = 600
max_parallel_tool_calls = 10

[kernel.events]
channel_capacity = 1024

[kernel.tool_execution]
max_output_bytes = 262144
default_timeout_seconds = 300

[secrets]
vault_path = "$DATA_DIR/vault/secrets.db"

[audit]
log_path = "$DATA_DIR/data/audit.db"
max_audit_entries = 500000
verify_last_n_entries = 1000

[tools]
core_tools_dir = "$DATA_DIR/tools/core"
user_tools_dir = "$DATA_DIR/tools/user"
data_dir = "$DATA_DIR/data"

[tools.workspace]
# Agent can access these host directories via storage zones.
# Desktop and projects are auto-granted by KMC policy.
allowed_paths = ["$HOME/Desktop", "$HOME/projects", "$HOME/Documents", "/media", "/run/media"]

[bus]
socket_path = "$DATA_DIR/data/agentos.sock"

[ollama]
host = "$OLLAMA_HOST"
default_model = "llama3.2"
request_timeout_secs = 300

[llm]
openai_base_url = "https://api.openai.com/v1"
anthropic_base_url = "https://api.anthropic.com/v1"
gemini_base_url = "https://generativelanguage.googleapis.com/v1beta"
max_tokens = 8192
ollama_context_window = 32768

[memory]
model_cache_dir = "$DATA_DIR/data/models"

[memory.extraction]
enabled = true
conflict_threshold = 0.85
max_facts_per_result = 5
min_result_length = 50

[memory.consolidation]
enabled = true
min_pattern_occurrences = 3
task_completions_trigger = 100
time_trigger_hours = 24
max_episodes_per_cycle = 500

[memory.context]
enabled = true
max_tokens = 4096
max_versions = 50
db_path = "$DATA_DIR/data/context_memory.db"

[context_budget]
total_tokens = 128000
reserve_pct = 0.25
system_pct = 0.15
tools_pct = 0.18
knowledge_pct = 0.30
history_pct = 0.25
task_pct = 0.12

[context]
summarization_mode = "llm"
summarization_max_input_chars = 8000

[logging]
log_dir = "$DATA_DIR/logs"
log_level = "info"
log_format = "text"

[otel]
enabled = false
endpoint = "http://localhost:4317"

[notifications]
max_inbox_size = 1000
notify_on_task_complete = true
notify_on_task_failed = true

[notifications.adapters.desktop]
enabled = true
min_priority = "warning"
notify_on_task_complete = true

[health_monitor]
enabled = true
check_interval_secs = 30

[health_monitor.thresholds]
cpu_warning_percent = 85.0
memory_warning_percent = 80.0
disk_warning_percent = 85.0
disk_critical_percent = 95.0
gpu_vram_warning_percent = 90.0

[scratchpad]
enabled = true
db_path = "$DATA_DIR/data/scratchpad.db"
context_depth = 2
max_context_pages = 5
max_context_bytes = 8192
max_page_size = 65536
max_pages_per_agent = 1000
auto_write_on_completion = true
auto_write_min_steps = 3
auto_write_max_summary = 2048

[skills]
core_skills_dir = "skills/core"
user_skills_dir = "skills/user"

[api]
enabled = true
host = "$HOST"
port = $PORT

[mcp]
servers = []

[runtime]
backend = "docker"
default_memory_limit_mb = 1024
default_cpu_limit = 1.0
default_pids_limit = 100
default_ttl_seconds = 3600
max_concurrent_containers = 10
workspace_base_dir = "$DATA_DIR/sandboxes"

allowed_images = [
    "python:3.11-slim",
    "python:3.12-slim",
    "node:20-alpine",
    "node:22-alpine",
    "ubuntu:22.04",
    "ubuntu:24.04",
    "rust:1.78-slim",
    "alpine:3.19",
]
TOML
else
  echo "==> Using existing config at $CONFIG"
fi

export AGENTOS_CONFIG="$CONFIG"
export AGENTOS_STATIC_DIR="$DATA_DIR/static"

# ─── Build ───────────────────────────────────────────────────────────────────

if [[ $SKIP_BUILD -eq 0 ]]; then
  echo "==> Building AgentOS (release)..."
  FEATURES_FLAG=""
  if [[ -n "$EXTRA_FEATURES" ]]; then
    FEATURES_FLAG="--features $EXTRA_FEATURES"
  fi
  cargo build --release -p agentos-cli $FEATURES_FLAG 2>&1
  echo "    Binary: $(du -h target/release/agentos | cut -f1)"
else
  if [[ ! -f target/release/agentos ]]; then
    echo "ERROR: No release binary found. Run without --skip-build first."
    exit 1
  fi
  echo "==> Skipping build (using existing binary)"
fi

# ─── Launch ──────────────────────────────────────────────────────────────────

echo ""
echo "==> Starting AgentOS (release, host-native)"
echo "    Config   : $CONFIG"
echo "    Data     : $DATA_DIR"
echo "    Web UI   : http://$HOST:$PORT"
echo "    Logs     : $DATA_DIR/logs/"
echo "    Socket   : $DATA_DIR/data/agentos.sock"
echo ""
echo "    Capabilities enabled:"
echo "      File I/O   : $HOME/Desktop, $HOME/projects, $HOME/Documents (via storage zones)"
echo "      Hardware   : GPU, display, thermal, webcam, audio, bluetooth (if drivers present)"
echo "      Processes  : sandboxed shell-exec, whitelisted proc-spawn"
echo "      Network    : outbound HTTP (SSRF-protected)"
echo "      Containers : Docker (if daemon available)"
echo ""
echo "    Press Ctrl+C to stop."
echo ""

exec ./target/release/agentos --config "$CONFIG" web serve --host "$HOST" --port "$PORT"
