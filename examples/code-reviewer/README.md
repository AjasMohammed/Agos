# Code Reviewer Agent

An agent that performs automated security and quality code review on a directory, produces a structured report, and stores findings in memory for trend tracking across reviews.

**Showcases:** file-reader tool, shell-exec, structured output, audit trail, escalation workflows (flags high-severity findings for human approval).

## What it does

1. Scans a target directory for source files
2. Reviews each file for: security issues, code quality, error handling, test coverage gaps
3. Produces a structured review report in markdown
4. Flags critical security issues as escalations requiring human sign-off
5. Stores findings in memory so future reviews can detect regressions
6. Full audit trail records every file read and finding logged

## Prerequisites

```bash
cargo build --workspace --release
./target/release/agentos start
export ANTHROPIC_API_KEY=your_key_here
```

## Run

```bash
cd examples/code-reviewer

# Review a specific directory
bash run.sh ../../crates/agentos-vault/src

# Review current directory
bash run.sh .

# Review with custom output path
OUTPUT_FILE=./my-review.md bash run.sh ../../crates/agentos-tools/src
```

## Output

- `./output/review-<timestamp>.md` — structured review report
- Critical findings escalated to: `agentos escalation list`
- Past reviews searchable: `agentos scratchpad search "security findings"`

## Trend tracking

```bash
# Compare with previous review
agentos task run --agent reviewer \
  "Compare the latest code review findings with previous reviews. Are we improving?"
```
