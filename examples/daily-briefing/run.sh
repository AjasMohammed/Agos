#!/usr/bin/env bash
# =============================================================================
# AgentOS Daily Briefing — On-Demand Run
#
# Runs the briefing agent immediately (useful for testing or manual triggers).
# Run setup.sh first to create the agent and register the Slack channel.
#
# Usage:
#   bash run.sh
# =============================================================================
set -euo pipefail

AGENT_NAME="briefing-agent"
AGENTOS="${AGENTOS_BIN:-agentos}"

CYAN='\033[0;36m'
GREEN='\033[0;32m'
BOLD='\033[1m'
NC='\033[0m'

echo ""
echo -e "${BOLD}Running daily briefing (on demand)...${NC}"
echo ""

$AGENTOS task run \
  --agent "$AGENT_NAME" \
  "$(cat "$(dirname "$0")/task-prompt.txt")"

echo ""
echo -e "${GREEN}✓${NC} Briefing delivered to Slack."
echo ""
echo -e "${GREEN}✓${NC} Cost for this run:"
$AGENTOS cost report --agent "$AGENT_NAME" --last 1

echo ""
echo -e "${GREEN}✓${NC} View in audit log:"
echo "  $AGENTOS audit logs --last 10"
echo ""
