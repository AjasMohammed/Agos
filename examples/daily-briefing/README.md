# Daily Briefing Agent

A scheduled agent that runs every morning, pulls data from multiple sources, synthesizes a briefing, and delivers it to a Slack channel — all orchestrated by AgentOS's scheduler and channel system.

**Showcases:** schedule system, Slack channel adapter, memory-search (recalls yesterday's context), pipeline orchestration, cost tracking.

## What it does

1. Runs automatically on a cron schedule (default: 8 AM every weekday)
2. Searches semantic memory for relevant context from previous briefings
3. Queries configured data sources (news search, internal metrics, task history)
4. Synthesizes a structured morning briefing
5. Delivers it to your Slack channel
6. Stores the briefing in memory for future recall and trend analysis

## Prerequisites

```bash
cargo build --workspace --release
./target/release/agentos start

export ANTHROPIC_API_KEY=your_key_here
export SLACK_BOT_TOKEN=xoxb-your-slack-bot-token
export SLACK_CHANNEL_ID=C1234567890  # your channel ID
```

## Setup (one time)

```bash
cd examples/daily-briefing
bash setup.sh
```

This will:
1. Connect the briefing agent
2. Register the Slack channel adapter
3. Create the 8 AM weekday schedule
4. Run a test briefing immediately so you can verify the output

## Run manually (on demand)

```bash
bash run.sh
```

## Schedule management

```bash
# View the schedule
agentos schedule list

# Pause the briefing
agentos schedule pause daily-briefing

# Resume it
agentos schedule resume daily-briefing

# Change the time (e.g., 7:30 AM)
agentos schedule delete daily-briefing
agentos schedule create \
  --name daily-briefing \
  --cron "0 30 7 * * 1-5" \
  --agent briefing-agent \
  --task "$(cat task-prompt.txt)" \
  --permissions "net.search:r,memory.semantic:rw,channel.slack:w"
```

## Cost tracking

Each briefing run is tracked:

```bash
agentos cost report --agent briefing-agent --last 7d
```

## What the briefing looks like

```
Good morning! Here's your daily briefing for Tuesday, April 15.

**Yesterday's Context**
- You had 3 completed tasks: code review, deployment, team sync
- 2 items carried over: API documentation, performance investigation

**Today's Focus**
Based on your task history and priorities...

**Tech Pulse** (from web search)
- [3 relevant headlines with summaries]

**Action Items**
- [ ] Review PR #42 (flagged yesterday)
- [ ] Follow up on performance investigation

Have a productive day!
```
