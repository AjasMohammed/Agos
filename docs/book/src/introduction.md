# Introduction

> An agentic operating environment built in Rust, designed ground-up for LLMs and AI agents — not for humans.

AgentOS is a **minimalist, LLM-native operating system**. Unlike agent frameworks that
wrap an LLM around an existing operating system, AgentOS is built from first principles
around a single idea: the LLM is the user.

## Core principles

- **LLMs are the CPU** — they process, reason, and decide.
- **Tools are the programs** — installed, versioned, and sandboxed.
- **Intent is the syscall** — structured declarations replace raw function calls.
- **Security is non-negotiable** — capability tokens, an encrypted vault, an append-only
  audit log, and OS-level sandboxing are load-bearing, not optional features.

Two supporting principles round out the design:

- **Agents are social** — every connected agent is aware of the others and can delegate,
  message, and coordinate.
- **Multi-LLM by default** — connect Ollama, OpenAI, Anthropic, Gemini, and any
  OpenAI-compatible provider simultaneously.

## How it runs

AgentOS runs as a kernel process on your machine. The `agentos` CLI talks to it over a
Unix domain socket. You connect one or more LLM agents, grant them scoped permissions,
and submit tasks. The kernel schedules each task, validates every tool call against a
signed capability token, executes authorized tools (sandboxed where applicable), and
injects the result back into the agent's context window — writing an audit entry for
every step.

```
agentos CLI ──(Unix socket)──> Inference Kernel
                                  ├─ Task scheduler · Context manager · Agent registry
                                  ├─ Capability engine · Secrets vault · Audit log
                                  ├─ LLM adapters (Ollama / OpenAI / Anthropic / Gemini)
                                  └─ Tool registry + sandbox (seccomp-BPF / bwrap / WASM)
```

## The Linux analogy

| Linux                    | AgentOS                              |
| ------------------------ | ------------------------------------ |
| Kernel                   | Inference Kernel                     |
| Process                  | Agent Task                           |
| System call              | Semantic Intent                      |
| Program / ELF binary     | Agent Tool (manifest + binary)       |
| Shell                    | `agentos` CLI                        |
| Filesystem               | Semantic / memory store              |
| User & group permissions | Permission matrix (rwx per resource) |
| Password / SSH key       | Encrypted secrets vault              |
| Package manager (apt)    | Tool registry / MCP catalog          |
| cron                     | Agent scheduler                      |

## Where to go next

- **[Quickstart](./quickstart.md)** — install and run your first task in minutes.
- **[Deployment](./deploy/index.md)** — systemd, Docker, Kubernetes, and the gateway bot.
- **[Configuration](./configuration.md)** — the full TOML reference.
- **[Security](./security.md)** — the candid statement of what is and isn't a boundary.
- **[Architecture](./architecture.md)** — how the kernel and crates fit together.
