#!/usr/bin/env bash
# Reproducible footprint numbers for one AgentOS binary: size on disk, cold
# start to "AgentOS is running.", and idle RSS after a settle period.
#
#   scripts/bench-footprint.sh [path/to/agentos] [settle_seconds]
#
# Boots into a throwaway data dir with its own socket/ports so it can run next
# to a live kernel. Builds nothing. Prints key=value lines; exit 1 if the
# kernel never reports ready within 60 s.
set -euo pipefail
BIN=${1:-target/release/agentos}
SETTLE=${2:-5}
cd "$(dirname "$0")/.."
[ -x "$BIN" ] || { echo "not executable: $BIN" >&2; exit 2; }

DATA=$(mktemp -d)
cleanup() {
  kill "${pid:-}" 2>/dev/null || true; wait "${pid:-}" 2>/dev/null || true
  if [ -n "${BENCH_KEEP:-}" ]; then echo "kept=$DATA"; else rm -rf "$DATA"; fi
}
trap cleanup EXIT

# Isolated copy of the default config: every /tmp/agentos path → $DATA, and
# the health port moved off 9091 so a running kernel on this host is untouched.
port=$(( 20000 + RANDOM % 20000 ))
sed -e "s|/tmp/agentos|$DATA|g" -e "s|^\(health_port *= *\)9091|\1$port|" config/default.toml > "$DATA/config.toml"
# BENCH_LITE=1: skip the ONNX embedding model (`[memory] disable_embedder`),
# the same switch an operator uses for a lexical-only deployment.
if [ -n "${BENCH_LITE:-}" ]; then
  sed -i 's|^embedder_init_timeout_secs *=.*|&\ndisable_embedder = true|' "$DATA/config.toml"
  grep -q '^disable_embedder = true' "$DATA/config.toml" || { echo "could not enable lite mode in config" >&2; exit 2; }
fi
mkdir -p "$DATA/data" "$DATA/logs"

echo "binary=$BIN"
if [ -n "${BENCH_LITE:-}" ]; then echo "mode=lite"; else echo "mode=full"; fi
echo "commit=$(git rev-parse --short HEAD 2>/dev/null || echo unknown)"
echo "size_bytes=$(stat -c %s "$BIN")"
echo "size_mb=$(( $(stat -c %s "$BIN") / 1048576 ))"

boot() { # $1 = log name; sets $pid and $ELAPSED_MS (no subshell: pid must survive)
  local log="$DATA/$1" start
  start=$(date +%s%N)
  AGENTOS_VAULT_PASSPHRASE=bench-passphrase \
    "$BIN" --config "$DATA/config.toml" start >"$log" 2>&1 &
  pid=$!
  for _ in $(seq 1 2400); do
    if grep -q 'AgentOS is running' "$log"; then break; fi
    if ! kill -0 "$pid" 2>/dev/null; then
      echo "kernel exited before ready; last log lines:" >&2; tail -20 "$log" >&2; exit 1
    fi
    sleep 0.05
  done
  grep -q 'AgentOS is running' "$log" || { echo "timeout waiting for ready" >&2; tail -20 "$log" >&2; exit 1; }
  ELAPSED_MS=$(( ($(date +%s%N) - start) / 1000000 ))
}

# First boot of a fresh data dir: includes one-time work (vault init, DB
# creation, embedding model fetch). Reported separately; not "cold start".
boot first-boot.log; echo "first_boot_ms=$ELAPSED_MS"
kill "$pid"; wait "$pid" 2>/dev/null || true

# Cold start: process start → ready, with on-disk state already initialised.
boot boot.log; echo "cold_start_ms=$ELAPSED_MS"

sleep "$SETTLE"
rss_kb=$(awk '/VmRSS/ {print $2}' /proc/"$pid"/status)
echo "idle_rss_kb=$rss_kb"
echo "idle_rss_mb=$(( rss_kb / 1024 ))"
echo "threads=$(awk '/Threads/ {print $2}' /proc/"$pid"/status)"
