#!/usr/bin/env bash
# =============================================================================
# AgentOS Code Reviewer Example
#
# Runs a code review agent that reads source files, identifies security and
# quality issues, produces a structured report, and escalates critical findings.
#
# Usage:
#   bash run.sh <path-to-review>
#
# Prerequisites:
#   - AgentOS kernel running: agentos start
#   - ANTHROPIC_API_KEY set
# =============================================================================
set -euo pipefail

TARGET_PATH="${1:-.}"
TARGET_PATH="$(realpath "$TARGET_PATH")"
TIMESTAMP="$(date +%Y%m%d-%H%M%S)"
OUTPUT_DIR="$(pwd)/output"
REPORT_FILE="${OUTPUT_FILE:-$OUTPUT_DIR/review-$TIMESTAMP.md}"
AGENT_NAME="reviewer"
AGENTOS="${AGENTOS_BIN:-agentos}"

GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

mkdir -p "$OUTPUT_DIR"

echo ""
echo -e "${BOLD}╔══════════════════════════════════════════════════════╗${NC}"
echo -e "${BOLD}║        AgentOS Code Reviewer Agent                   ║${NC}"
echo -e "${BOLD}╚══════════════════════════════════════════════════════╝${NC}"
echo ""
echo -e "${CYAN}Target:${NC} $TARGET_PATH"
echo -e "${CYAN}Report:${NC} $REPORT_FILE"
echo ""

# --- Step 1: Connect the reviewer agent ---
echo -e "${CYAN}[1/4]${NC} Connecting reviewer agent..."
$AGENTOS agent connect \
  --provider anthropic \
  --model claude-sonnet-4-6 \
  --name "$AGENT_NAME" 2>/dev/null || true

# --- Step 2: Grant required permissions ---
echo -e "${CYAN}[2/4]${NC} Granting permissions..."
# Grant read access to the target directory only (no write, no network)
$AGENTOS perm grant "$AGENT_NAME" "fs.$(dirname "$TARGET_PATH"):r"
$AGENTOS perm grant "$AGENT_NAME" fs.output:w        # write the report
$AGENTOS perm grant "$AGENT_NAME" memory.semantic:rw # store findings
# Note: shell-exec is intentionally NOT granted — reviewer reads only

# --- Step 3: Run the review task ---
echo -e "${CYAN}[3/4]${NC} Running code review (this may take 2-5 minutes depending on codebase size)..."
echo ""

TASK_PROMPT="Perform a thorough code review of the source files in: $TARGET_PATH

Review each file for the following categories:

**Security:**
- SQL injection, XSS, path traversal, SSRF vulnerabilities
- Hardcoded secrets or credentials
- Unsafe deserialization or eval usage
- Missing input validation

**Code Quality:**
- Error handling gaps (unwrap(), expect() in production paths)
- Resource leaks (unclosed files, connections)
- Race conditions or unsafe concurrent access
- Dead code or unreachable branches

**Architecture:**
- Violation of single responsibility principle
- Tight coupling between modules
- Missing abstractions for repeated patterns

**Testing Gaps:**
- Untested edge cases or error paths
- Missing security invariant tests

For each finding include:
- File and line number (if determinable)
- Severity: Critical / High / Medium / Low
- Description of the issue
- Suggested fix

Write the complete review report to: $REPORT_FILE

Format:
# Code Review Report
**Target:** $TARGET_PATH
**Date:** $(date -u +%Y-%m-%dT%H:%M:%SZ)
**Reviewer:** AgentOS Automated Review

## Executive Summary
[Overall assessment in 2-3 sentences]

## Critical Findings
[Must fix before ship]

## High Findings
[Should fix soon]

## Medium / Low Findings
[Address in normal course]

## Positive Observations
[What the code does well]

---

After writing the report, store the key security findings as semantic memories
prefixed with 'code-review-finding:' for trend tracking."

$AGENTOS task run \
  --agent "$AGENT_NAME" \
  --thinking high \
  "$TASK_PROMPT"

# --- Step 4: Show results ---
echo ""
echo -e "${CYAN}[4/4]${NC} Review complete."
echo ""

if [ -f "$REPORT_FILE" ]; then
  echo -e "${GREEN}✓${NC} Report: $REPORT_FILE"
  echo ""

  # Count findings by severity
  CRITICAL=$(grep -c "^### \|**Critical\|## Critical" "$REPORT_FILE" 2>/dev/null || echo "0")
  echo -e "${BOLD}--- Review Summary ---${NC}"
  grep -A2 "## Executive Summary" "$REPORT_FILE" 2>/dev/null | tail -2 || true
  echo ""
else
  echo -e "${YELLOW}!${NC} Report file not found — check task output above."
fi

# Check for escalations (critical findings)
ESCALATION_COUNT=$($AGENTOS escalation list 2>/dev/null | grep -c "pending" || echo "0")
if [ "$ESCALATION_COUNT" -gt 0 ]; then
  echo -e "${YELLOW}!${NC} $ESCALATION_COUNT critical finding(s) require human sign-off:"
  echo "  $AGENTOS escalation list"
  echo "  $AGENTOS escalation approve <id>  # or deny"
fi

echo ""
echo -e "${GREEN}✓${NC} Audit trail recorded. View with:"
echo "  $AGENTOS audit logs --last 30"
echo ""
echo -e "${GREEN}✓${NC} Findings stored in memory. Track trends with:"
echo "  $AGENTOS task run --agent $AGENT_NAME \"Summarize all past code review findings and trends\""
echo ""
