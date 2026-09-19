# Five-Minute Tour

The README quick start, with what you should see at each step. Linux x86_64; macOS via Docker; Windows via WSL2.

## 1. Install

Pick one.

```bash
# Signed release binary (from the first tagged release onward). The script
# downloads the asset for your platform, verifies its minisign signature
# against the public key pinned in the script (same key as
# packaging/signing/agentos-release.pub), and installs to ~/.local/bin.
curl -fsSL https://raw.githubusercontent.com/AjasMohammed/Agos/main/scripts/install.sh | bash

# From source. Needs Rust 1.91+; ~10 minutes and ~13 GB RAM the first time.
cargo install --git https://github.com/AjasMohammed/Agos agentos-cli

# Container. Read-only rootfs, non-root user, Ollama and Jaeger sidecars.
git clone https://github.com/AjasMohammed/Agos.git && cd Agos
cp .env.example .env && docker compose up -d
```

Check:

```
$ agentos --version
agentos 1.0.0-rc.1
```

## 2. Start the kernel

```bash
export AGENTOS_VAULT_PASSPHRASE='choose-a-passphrase'
agentos start
```

Expected (trimmed):

```
🚀 Booting AgentOS kernel...
✅ Kernel started
AgentOS is running. Use another terminal to run agentos commands.
```

The first boot of the full build downloads the ~23 MB MiniLM embedding model. If that takes longer than `embedder_init_timeout_secs` (default 120 s), boot continues without vector search and says so. Set `[memory] disable_embedder = true` in your config to skip it entirely.

By default nothing listens on the network: the REST API is off and the health endpoint binds `127.0.0.1:9091`. The CLI talks to the kernel over a Unix socket.

## 3. Connect an agent

In a second terminal:

```bash
agentos agent connect --provider ollama --model llama3.2 --name assistant
```

Any provider in `config/providers.toml` works; for a hosted one, store the key first, scoped to the agent that needs it:

```bash
agentos secret set ANTHROPIC_API_KEY --scope agent:assistant
agentos agent connect --provider anthropic --model claude-sonnet-5 --name assistant
```

Agents start with **zero permissions**. Grant what this one needs:

```bash
agentos perm grant assistant fs.user_data:rw
```

Try it from the terminal:

```bash
agentos task run --agent assistant "Write a haiku about audit logs to haiku.txt"
```

The agent's files live under `data/agents/assistant/`; it cannot read outside that directory unless you grant a workspace path.

## 4. Connect Telegram

Create a bot with [@BotFather](https://t.me/BotFather) and copy the token.

```bash
agentos secret set TELEGRAM_BOT_TOKEN --scope global      # prompts for the value
agentos channel connect --kind telegram --display-name "my-bot" \
  --credential-key TELEGRAM_BOT_TOKEN --active-agent assistant
```

Expected:

```
Channel connected: my-bot (id: <uuid>)
```

## 5. Pair yourself

Send `/start` to the bot in Telegram. It answers with a 6-character code and nothing else; unpaired senders never reach an agent.

```bash
agentos channel pair approve ABC123
```

```
Pairing approved — '<your telegram id>' is now allowlisted.
```

`agentos channel pair list` shows approved senders; `agentos channel pair revoke <channel-id> <sender-id>` removes one.

## 6. Talk

Send "hi". The reply comes from `assistant`. Ask for something with side effects ("delete haiku.txt") and the bot posts an approval card: **Approve** / **Deny**, with the tool name, its risk class, and a redacted preview of the arguments. Nothing runs until you answer; unanswered requests auto-deny after 5 minutes.

Everything that just happened is in the audit log:

```bash
agentos audit logs --last 20
```

## Where next

- [06 — Security Model](06-security.md): what each control does, and the threat-class tests.
- [07 — Configuration](07-configuration.md): approval modes (`auto`, `ask_edit`, `ask_always`, `deny`), workspace grants, provider setup.
- [Benchmarks](benchmarks.md): what the daemon costs at idle and how to make it lighter.
