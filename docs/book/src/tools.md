# Tools

Agent Tools are the "programs" of AgentOS. They have no UI — each tool is a machine-readable
manifest plus a typed interface, designed for LLM consumption.

## How a tool call works

1. The LLM declares intent (e.g. *"read file report.txt"*).
2. The kernel matches it to a tool (`file-reader`).
3. The kernel checks the agent's capability token for the required permission (`fs.user_data:r`).
4. If authorized, the tool runs — sandboxed where applicable.
5. The result is wrapped in typed delimiters and injected back into the agent's context.

Every step is audited. Agents start with **zero** permissions; nothing runs without a valid
capability token.

## Built-in tools

AgentOS ships dozens of core tools compiled into the kernel as native Rust, spanning:

- **File system** — `file-reader`, `file-writer`, `file-editor`, `file-delete`, `file-move`,
  `file-diff`, `file-glob`, `file-grep` (all reject `..` path traversal).
- **Memory** — `memory-search`, `memory-write`, `memory-read`, `memory-stats`, the
  `memory-block-*` named blocks, and `archival-insert`/`archival-search`. See [Memory](./memory.md).
- **Procedural** — `procedure-create`, `procedure-search`, `procedure-list`.
- **Network** — `web-search` (Brave → Tavily → Serper → DuckDuckGo fallback, with an SSRF
  guard), `web-fetch`.
- **Shell / exec** — `shell-exec`, `script` (sandboxed via bubblewrap `--unshare-all`).
- **Agent / meta** — `agent-message`, `task-delegate`, `agent-manual` (the live tool
  inventory), `skill-prompt`.
- **HAL** — host introspection: process, network sockets, mounts, services (Linux-only,
  degrade gracefully elsewhere).

Use the `agent-manual` tool at runtime for the authoritative, live list and full per-tool
schemas, or `agentos tool list` from the CLI.

## Trust tiers

Every tool manifest carries a `trust_tier`:

| Tier | Signature requirement | Behavior |
|------|----------------------|----------|
| `Core` | none (distribution-trusted) | Runs in-process under `trust_aware` policy. |
| `Verified` / `Community` | Ed25519 `author_pubkey` + `signature` over canonical JSON | Sandboxed; signature verified at load. |
| `Blocked` | — | Hard-rejected by the kernel. |

Offline signing: `agentos tool keygen`, `agentos tool sign`, `agentos tool verify`.

## Risk classes & approval

Manifests also declare a `risk_class` (`ReadonlyScoped`, `ReadonlyExternal`, `WriteScoped`,
`ExecCapable`, `ControlPlane`, `Interactive`). The kernel's approval mode (see
[Configuration](./configuration.md) `[approval]`) uses the risk class to decide whether to
auto-approve a call or escalate it for human review. Unknown tools default to `ExecCapable`
(fail-closed).

## WASM & external tools

Custom tools can be compiled to `.wasm` and installed at runtime via a manifest, executed
under Wasmtime. Tools can also be imported from external **MCP** servers — see
[MCP Integrations](./mcp.md). MCP tools register as `Community` tier and are subject to the
same capability and permission enforcement as native tools.

## Sandbox policy

`[kernel] sandbox_policy` controls execution: `trust_aware` (default — Core in-process,
Community/Verified sandboxed), `always`, or `never` (development only).
