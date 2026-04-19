# Why I Built an AI Operating System in Rust (And Why Python Frameworks Were Not Enough)

*A post-mortem on the design decisions behind AgentOS — and what I learned building a production-grade AI agent runtime from scratch.*

---

When I started building AI agents seriously, I used the same tools everyone else uses: LangChain, then AutoGen, then a handful of newer Python frameworks. They all worked — until the moment I needed them to work in production.

Not "production" as in "it runs on a server." I mean production as in: *auditable, isolated, accountable, recoverable.* The kind of production a bank or healthcare company or critical infrastructure operator cares about.

That's when I realized the Python ecosystem had a foundational problem, and the only way to solve it properly was to start over.

## The Problem With Existing Frameworks

The issue isn't that LangChain is bad. It's that LangChain was designed for *experimentation*, not *operations*. The same is true of most of the agent frameworks built in the last two years.

Here's what I kept running into:

**1. No security model.** Most frameworks treat tool access as binary — the agent either has it or doesn't. There's no concept of least-privilege, no capability scoping, no audit trail of what the agent actually did. In a world where agents can execute code, read files, and call external APIs, "the agent is allowed to do anything" is not a security model.

**2. No crash recovery.** An agent mid-task is stateful. It has context, partial results, tool call history. If the process dies — or even if you just need to pause it — that state is gone. You start over. Every framework I tried had this problem.

**3. No memory architecture.** "Memory" in most frameworks means "stuff we stuff into the context window." That's not memory. Real memory is tiered: short-term working context, episodic history of past interactions, semantic knowledge that persists across sessions. Without this, every task starts from zero.

**4. No resource accounting.** When you run agents at scale, you care about cost. Not just API costs, but computational cost, time cost, fairness between competing agents. None of the frameworks I tried had any concept of budget enforcement or cost attribution.

**5. Python's runtime guarantees.** I want to be careful here — Python is a fine language. But for a runtime that needs to execute untrusted tool code, filter syscalls, zero secrets from memory, and run with minimal overhead, Rust's ownership model and zero-cost abstractions are the right tool.

## What an AI OS Actually Means

I kept using the word "framework" for what I was building, but it didn't fit. Frameworks are libraries you use. What I was building was more like an *operating system* — a runtime environment that manages resources, enforces policy, and provides services to the programs running inside it.

The analogy maps closely:

- **LLMs are the CPU** — they reason and decide
- **Tools are the programs** — installed, versioned, sandboxed
- **Intent is the syscall** — structured declarations replace raw function calls
- **The kernel mediates everything** — capability tokens, audit log, scheduler

Once I thought about it this way, the design became obvious. You build a kernel. You build a scheduler. You build a permission system. You build a vault. You build an audit log. You add an LLM adapter layer on top. The LLM calls into the kernel via intents; the kernel validates, routes, executes, and logs.

## Why Rust

The short answer: Rust was the only language that could give me everything I needed without compromise.

I needed:
- **No GC pauses** — an agent runtime can't pause for garbage collection mid-inference
- **Memory safety without a runtime** — tools run in isolation; I can't afford a language runtime leaking across boundaries
- **`zeroize` support** — vault keys and API tokens need to be zeroed from memory after use; Rust's `zeroize` crate does this correctly at compile time
- **Seccomp-BPF** — I wanted to apply syscall filtering at the kernel level for tool execution; this is idiomatic in Rust on Linux
- **Compile-time correctness** — an OS-like runtime has complex state machines; Rust's type system makes illegal states unrepresentable in a way that Python can't match

The downside is real: Rust has a smaller ecosystem of AI tools and integrations than Python. I've had to build more from scratch. But the result is a runtime I can actually trust in production.

## The Features That Took Longest

### Capability Tokens

The permission system was the hardest part to get right. The goal: every tool call must be accompanied by an unforgeable, scoped capability token. The token says exactly what the agent is allowed to do — which tools, which resources, with what permissions — and the kernel validates it before every execution.

This means a compromised agent can't exceed its granted permissions, even if the LLM is convinced by a prompt injection attack to try. The security boundary is enforced in Rust, not in the LLM's judgment.

### 3-Tier Memory

Working memory, episodic memory, semantic memory — each is backed by SQLite with FTS5 full-text search and vector embeddings (MiniLM-L6-v2, running locally, no network). The agent can recall what it did last week, what facts it's learned, and what procedures work for recurring tasks. All of it persists across restarts.

### Task Checkpointing

An agent that runs for 45 minutes and then crashes loses all its work. Checkpointing writes the complete task state — context window, tool call history, partial results — to SQLite after every iteration. If the process dies, `agentos task resume <id>` picks up exactly where it left off.

### The Audit Trail

129 event types. Every tool call, every permission check, every secret access, every agent communication, every cost attribution — all written to an append-only SQLite log with hash-chain verification. You can prove what happened and when. This is the feature that unlocks regulated industries.

## What It Looks Like Today

AgentOS is around 172,000 lines of Rust across 26 crates. It has:
- A web UI (Axum + HTMX) for dashboard, task management, and audit viewing
- 27 CLI commands covering every operation
- 10+ channel adapters (Slack, Discord, Telegram, Teams, Matrix...)
- A REST API with OpenAI-compatible endpoints for drop-in compatibility
- Hardware abstraction for GPIO, GPU, audio, and IoT
- Built-in pipeline orchestration and multi-agent coordination

And it deploys as a single binary or a Docker container in two commands.

## What I'd Do Differently

**Ship earlier.** I spent too long building features before having anything runnable by someone else. The codebase grew large before it had users, which meant I was optimizing things nobody had validated.

**Pick one use case first.** AgentOS can do a lot of things. That's a liability when you're trying to explain it to someone. The security model and audit trail are the genuine differentiators — I should have led with those from day one.

**Build the community earlier.** Open source without community is just a big private codebase. The blog post should have come before the 100th feature.

## Get Started

```bash
git clone https://github.com/agentos/agentos.git
cd agentos
docker compose up -d
open http://localhost:8080
```

Or follow the [5-minute quickstart](../../README.md) to build from source.

I'd love to hear what you build with it. The project is Apache 2.0 licensed.

---

*AgentOS is an open-source AI agent runtime written in Rust. GitHub: [agentos/agentos](https://github.com/agentos/agentos). If you work in a security-sensitive environment and need production-grade agent infrastructure, reach out.*
