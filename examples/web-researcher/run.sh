#!/usr/bin/env bash
# =============================================================================
# AgentOS Web Researcher Example
#
# Runs a research agent that searches the web, synthesizes findings into a
# structured report, and stores key facts in persistent memory.
#
# Usage:
#   bash run.sh "your research topic"
#
# Prerequisites:
#   - AgentOS kernel running: agentos start
#   - ANTHROPIC_API_KEY set (or adjust --provider/--model below)
# =============================================================================
set -euo pipefail

TOPIC="${1:-Recent advances in Rust async runtimes}"
OUTPUT_DIR="$(pwd)/output"
REPORT_FILE="$OUTPUT_DIR/report.md"
AGENT_NAME="researcher"
AGENTOS="${AGENTOS_BIN:-agentos}"

GREEN='\033[0;32m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

mkdir -p "$OUTPUT_DIR"

echo ""
echo -e "${BOLD}╔══════════════════════════════════════════════════════╗${NC}"
echo -e "${BOLD}║        AgentOS Web Researcher Agent                  ║${NC}"
echo -e "${BOLD}╚══════════════════════════════════════════════════════╝${NC}"
echo ""
echo -e "${CYAN}Topic:${NC} $TOPIC"
echo ""

# --- Step 1: Connect the researcher agent ---
echo -e "${CYAN}[1/4]${NC} Connecting researcher agent..."
$AGENTOS agent connect \
  --provider anthropic \
  --model claude-sonnet-4-6 \
  --name "$AGENT_NAME" 2>/dev/null || true  # idempotent — already connected is fine

# --- Step 2: Grant required permissions ---
echo -e "${CYAN}[2/4]${NC} Granting permissions..."
$AGENTOS perm grant "$AGENT_NAME" net.search:r      # web search
$AGENTOS perm grant "$AGENT_NAME" fs.output:rw      # write report
$AGENTOS perm grant "$AGENT_NAME" memory.semantic:rw # store findings

# --- Step 3: Run the research task ---
echo -e "${CYAN}[3/4]${NC} Running research task (this may take 1-2 minutes)..."
echo ""

TASK_PROMPT="Research the following topic thoroughly: \"$TOPIC\"

Instructions:
1. Use the web-search tool to find 3-5 authoritative sources
2. Read and analyze each source carefully
3. Synthesize the findings into a structured markdown report with:
   - Executive Summary (3-5 sentences)
   - Key Findings (bulleted list)
   - Technical Details (where relevant)
   - Sources (with URLs)
4. Write the final report to: $REPORT_FILE
5. Store 3-5 key facts as semantic memories for future recall

Be thorough but concise. Prioritize accuracy over breadth."

$AGENTOS task run \
  --agent "$AGENT_NAME" \
  --thinking medium \
  "$TASK_PROMPT"

# --- Step 4: Show results ---
echo ""
echo -e "${CYAN}[4/4]${NC} Research complete."
echo ""

if [ -f "$REPORT_FILE" ]; then
  echo -e "${GREEN}✓${NC} Report written to: $REPORT_FILE"
  echo ""
  echo -e "${BOLD}--- Report Preview (first 30 lines) ---${NC}"
  head -30 "$REPORT_FILE"
  echo ""
  echo -e "  [full report: $REPORT_FILE]"
else
  echo "  Note: Report file not found — check task output above."
fi

echo ""
echo -e "${GREEN}✓${NC} Audit trail recorded. View with:"
echo "  $AGENTOS audit logs --last 20"
echo ""
echo -e "${GREEN}✓${NC} Findings stored in memory. Recall with:"
echo "  $AGENTOS task run --agent $AGENT_NAME \"What did we find about: $TOPIC\""
echo ""
