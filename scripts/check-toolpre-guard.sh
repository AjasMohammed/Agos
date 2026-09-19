#!/usr/bin/env bash
# Guard against re-introducing the 2026-07-20 ToolPre bypass: every
# `tool_runner.execute(` in the kernel outside the task executor must be
# preceded, within the same enclosing `fn`, by `enforce_tool_pre(` (or the kernel
# wrapper `enforce_chat_tool_pre(`), so
# pipeline, scheduled, gateway, and any future direct call paths all fire
# ToolPre hooks (approval + audit) before running a tool.
#
# Heuristic tripwire, not a proof: it only checks that `enforce_tool_pre(`
# appears textually earlier in the same fn, not that it dominates the call.
# Call sites are matched layout-insensitively (rustfmt may split the
# receiver chain across lines).
set -euo pipefail
cd "$(dirname "$0")/.."
fail=0
sites=$(perl -0777 -ne 'while (/tool_runner\s*\.\s*execute\s*\(/g) {
    my $n = () = substr($_, 0, pos()) =~ /\n/g; print "$ARGV:" . ($n + 1) . "\n" }' \
  $(git ls-files 'crates/agentos-kernel/src/*.rs' 'crates/agentos-kernel/src/**/*.rs') \
  | grep -v '/task_executor\.rs:' || true)
if [ -z "$sites" ]; then
  echo "::error::no tool_runner.execute call sites found; pattern or path is stale"
  exit 1
fi
while IFS=: read -r file line; do
  # Start of the enclosing function (last `fn` header above the call).
  start=$(awk -v L="$line" 'NR<L && /^[[:space:]]*(pub(\([a-z: ]+\))?[[:space:]]+)?(const[[:space:]]+)?(async[[:space:]]+)?(unsafe[[:space:]]+)?fn[[:space:]]/{s=NR} END{print s+0}' "$file")
  if [ "$start" -eq 0 ]; then
    echo "::error file=$file,line=$line::no enclosing fn found above tool_runner.execute"
    fail=1
    continue
  fi
  if ! sed -n "${start},${line}p" "$file" | grep -E 'enforce(_chat)?_tool_pre[(]' >/dev/null; then
    echo "::error file=$file,line=$line::tool_runner.execute without enforce_tool_pre( / enforce_chat_tool_pre( in enclosing fn (starts line $start)"
    fail=1
  fi
done <<< "$sites"
if [ "$fail" -eq 0 ]; then echo "toolpre guard: ok ($(wc -l <<< "$sites") call sites checked)"; fi
exit "$fail"
