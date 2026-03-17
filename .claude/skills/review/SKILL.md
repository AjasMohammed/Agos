# Code Review Skill
1. Read the relevant plan/spec document if referenced
2. Spawn a sub-agent to review implementation against the spec
3. List all findings with severity (critical/warning/info)
4. Apply fixes for critical and warning items
5. Run: cargo build && cargo test && cargo clippy -- -D warnings && cargo fmt --check
6. If any step fails, fix and re-run until clean
7. Report summary of changes made
