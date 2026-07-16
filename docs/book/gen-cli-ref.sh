#!/usr/bin/env bash
# Generate docs/book/src/cli.md from the `agentos` binary's --help output so the
# CLI reference can never drift from the binary. Re-run after CLI changes:
#
#   cargo build -p agentos-cli && docs/book/gen-cli-ref.sh
#
# Override the binary path with AGENTOS_BIN (e.g. target/release/agentos).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
BIN="${AGENTOS_BIN:-$ROOT/target/debug/agentos}"
OUT="$ROOT/docs/book/src/cli.md"

if [[ ! -x "$BIN" ]]; then
  echo "error: $BIN not found — run 'cargo build -p agentos-cli' first" >&2
  exit 1
fi

# Print the --help for a (possibly empty) subcommand path.
emit_help() { "$BIN" $* --help 2>&1; }

# Extract immediate subcommand names from a command's Commands: section.
# clap lists them at a 2-space indent; we drop the built-in `help`.
list_subcommands() {
  "$BIN" "$@" --help 2>&1 \
    | awk '/^Commands:/{f=1;next} /^[A-Za-z]/{f=0} f && /^  [a-z][a-z0-9_-]*[[:space:]]/{print $1}' \
    | grep -vx help || true
}

{
  echo "# CLI Reference"
  echo
  echo "> **Generated** from \`agentos --help\` by \`docs/book/gen-cli-ref.sh\`."
  echo "> Do not edit by hand — re-run the script after CLI changes so this page"
  echo "> can never drift from the binary. The version embeds via clap"
  echo "> \`#[command(version)]\`."
  echo
  echo "Most commands talk to a running kernel over a Unix domain socket; a few"
  echo "(key generation, signing, \`doctor\`, \`config\`) run offline."
  echo
  echo '## `agentos`'
  echo
  echo '```text'
  emit_help
  echo '```'
  echo

  for cmd in $(list_subcommands); do
    echo "## \`agentos $cmd\`"
    echo
    echo '```text'
    emit_help "$cmd"
    echo '```'
    echo
    for sub in $(list_subcommands "$cmd"); do
      echo "### \`agentos $cmd $sub\`"
      echo
      echo '```text'
      emit_help "$cmd $sub"
      echo '```'
      echo
    done
  done
} >"$OUT"

echo "wrote $OUT ($(wc -l <"$OUT") lines)"
