# Web Researcher Agent

An agent that searches the web for a topic, synthesizes findings into a structured report, and saves everything to persistent memory for future recall.

**Showcases:** web-search tool, file-writer, episodic memory, task checkpointing, audit trail.

## What it does

1. Connects an agent with web-search and file-write permissions
2. Runs a research task with a configurable topic
3. The agent searches multiple sources, synthesizes findings
4. Output is written to a markdown report file
5. Key facts are stored in AgentOS semantic memory for future retrieval
6. The full audit trail records every tool call and search query

## Prerequisites

```bash
# Build AgentOS
cargo build --workspace --release

# Start the kernel (in a separate terminal)
./target/release/agentos start

# Set your LLM API key (Anthropic recommended for best results)
export ANTHROPIC_API_KEY=your_key_here

# Optional: set Brave Search API key for better search results
export BRAVE_API_KEY=your_key_here
```

## Run

```bash
cd examples/web-researcher
bash run.sh "Rust async runtime performance vs Go goroutines 2024"
```

Or with Docker:

```bash
docker compose up -d
docker exec agentos-kernel bash /examples/web-researcher/run.sh \
  "Rust async runtime performance vs Go goroutines 2024"
```

## Output

- `./output/report.md` — structured research report
- Findings stored in AgentOS semantic memory (searchable with `agentos scratchpad search`)
- Full audit log: `agentos audit logs --last 50`

## Recall previous research

```bash
# Search memory for past research
agentos scratchpad search "Rust async performance"

# Or query the semantic memory tier directly
agentos task run --agent researcher \
  "What do we know about Rust async performance from previous research?"
```
