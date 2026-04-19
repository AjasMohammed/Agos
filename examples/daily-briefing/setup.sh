#!/usr/bin/env bash
# =============================================================================
# AgentOS Daily Briefing — One-Time Setup
#
# Creates the agent, registers the Slack channel, and installs the 8 AM
# weekday schedule. Run this once; use run.sh for on-demand execution.
#
# Prerequisites:
#   - AgentOS kernel running: agentos start
#   - ANTHROPIC_API_KEY, SLACK_BOT_TOKEN, SLACK_CHANNEL_ID set
# =============================================================================
set -euo pipefail

AGENT_NAME="briefing-agent"
SCHEDULE_NAME="daily-briefing"
AGENTOS="${AGENTOS_BIN:-agentos}"

GREEN='\033[0;32m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

# Validate env vars
: "${SLACK_BOT_TOKEN:?SLACK_BOT_TOKEN must be set}"
: "${SLACK_CHANNEL_ID:?SLACK_CHANNEL_ID must be set}"

echo ""
echo -e "${BOLD}╔══════════════════════════════════════════════════════╗${NC}"
echo -e "${BOLD}║     AgentOS Daily Briefing — Setup                   ║${NC}"
echo -e "${BOLD}╚══════════════════════════════════════════════════════╝${NC}"
echo ""

# --- Step 1: Store Slack credentials in the vault ---
echo -e "${CYAN}[1/5]${NC} Storing Slack credentials in encrypted vault..."
$AGENTOS secret set SLACK_BOT_TOKEN --scope "agent:$AGENT_NAME"
# Prompt will appear for the value — paste your token

# --- Step 2: Connect the briefing agent ---
echo -e "${CYAN}[2/5]${NC} Connecting briefing agent..."
$AGENTOS agent connect \
  --provider anthropic \
  --model claude-sonnet-4-6 \
  --name "$AGENT_NAME" 2>/dev/null || true

# --- Step 3: Register Slack channel adapter ---
echo -e "${CYAN}[3/5]${NC} Registering Slack channel adapter..."
$AGENTOS channel connect slack \
  --name "team-slack" \
  --channel-id "$SLACK_CHANNEL_ID" \
  --token-secret SLACK_BOT_TOKEN

# --- Step 4: Grant required permissions ---
echo -e "${CYAN}[4/5]${NC} Granting permissions..."
$AGENTOS perm grant "$AGENT_NAME" net.search:r          # web search
$AGENTOS perm grant "$AGENT_NAME" memory.semantic:rw     # recall + store
$AGENTOS perm grant "$AGENT_NAME" memory.episodic:r      # task history recall
$AGENTOS perm grant "$AGENT_NAME" "channel.slack:w"      # send to Slack

# --- Step 5: Create the cron schedule ---
echo -e "${CYAN}[5/5]${NC} Creating 8 AM weekday schedule..."

# Write the task prompt to a file for reuse
cat > task-prompt.txt << 'PROMPT'
Generate a morning briefing for the team.

Steps:
1. Search semantic memory for "task completed yesterday" and "priorities"
2. Search the web for the top 3 tech/engineering news items relevant to our work
3. Review the task history from the last 24 hours using memory search
4. Synthesize a concise morning briefing with these sections:
   - Greeting with today's date
   - Yesterday's Context (what was completed, what carried over)
   - Today's Focus (based on priorities in memory)
   - Tech Pulse (3 news items with 1-sentence summaries)
   - Action Items (concrete next steps)
5. Send the briefing to the team-slack channel
6. Store the briefing in semantic memory tagged as "daily-briefing"

Keep it under 400 words. Be direct and useful, not verbose.
PROMPT

$AGENTOS schedule create \
  --name "$SCHEDULE_NAME" \
  --cron "0 0 8 * * 1-5" \
  --agent "$AGENT_NAME" \
  --task "$(cat task-prompt.txt)" \
  --permissions "net.search:r,memory.semantic:rw,memory.episodic:r,channel.slack:w"

echo ""
echo -e "${GREEN}✓${NC} Setup complete!"
echo ""
echo -e "  Schedule:   ${CYAN}$SCHEDULE_NAME${NC} — runs weekdays at 8:00 AM"
echo -e "  Agent:      ${CYAN}$AGENT_NAME${NC}"
echo -e "  Channel:    ${CYAN}team-slack${NC} → #$(echo $SLACK_CHANNEL_ID)"
echo ""
echo -e "Run a test briefing now:"
echo "  bash run.sh"
echo ""
echo -e "Manage the schedule:"
echo "  $AGENTOS schedule list"
echo "  $AGENTOS schedule pause $SCHEDULE_NAME"
echo ""
