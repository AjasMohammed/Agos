# Security Model

Security in AgentOS is non-negotiable. The threat model assumes prompt injection, tool privilege escalation, rogue tasks, and supply chain attacks. Every layer of the system has defense mechanisms.

---

## Defense in Depth

AgentOS uses seven layers of security:

| Layer | Mechanism                       | Description                                                                          |
| ----- | ------------------------------- | ------------------------------------------------------------------------------------ |
| **1** | Capability-Based Access Control | Every resource access requires an unforgeable, scoped, kernel-signed token           |
| **2** | Tool Sandboxing                 | Every tool runs under seccomp-BPF constraints                                        |
| **3** | Intent Verification             | Kernel validates every intent against the task's capability token _before_ execution |
| **4** | Output Sanitization             | Tool outputs are wrapped in typed delimiters, never injected raw into LLM context    |
| **5** | Immutable Audit Log             | Every intent, tool execution, and agent message is logged (append-only)              |
| **6** | Secrets Isolation               | API keys never appear in env vars, config files, or agent context                    |
| **7** | Agent Identity Signing          | Agent messages are signed with kernel-issued identity tokens                         |

---

## Secrets Vault

The secrets vault is the most sensitive subsystem. It ensures API keys, tokens, and passwords are never exposed.

### How Secrets Are Stored

```
User enters secret → agentctl transmits over Unix domain socket
    → SecretsVault encrypts with AES-256-GCM
    → Master key derived from passphrase via Argon2id
    → Encrypted blob stored in vault DB (SQLite)
    → At runtime: kernel retrieves and decrypts in memory
    → Key is zeroed from memory after use (Rust zeroize crate)
```

### Security Guarantees

- **API keys are never passed as CLI arguments** — they are entered via hidden interactive input, so they never appear in shell history
- **API keys are never stored in environment variables or `.env` files**
- **API keys are never visible in config files, docker-compose.yml, or any plaintext format**
- **Secrets are zeroed from memory** immediately after use using the `zeroize` crate
- **No tool, agent, or CLI command can read a raw secret value** — only the kernel's internal LLM adapters access decrypted values, and only at initialization time

### Secret Scoping

Secrets can be scoped to control who can use them:

| Scope          | Description                                     |
| -------------- | ----------------------------------------------- |
| `global`       | Any agent or tool can use this secret (default) |
| `agent:<name>` | Only the specified agent can use this secret    |
| `tool:<name>`  | Only the specified tool can use this secret     |

```bash
agentctl secret set OPENAI_API_KEY                          # global scope
agentctl secret set SLACK_TOKEN --scope agent:notifier      # agent-scoped
agentctl secret set DB_PASSWORD --scope tool:database-query # tool-scoped
```

---

## Capability Tokens

Every task receives a **CapabilityToken** issued by the kernel. This token is:

- **HMAC-SHA256 signed** — by the kernel's signing key (stored in the vault)
- **Unforgeable** — cannot be created outside the kernel
- **Scoped** — encodes the exact permissions the task is allowed to exercise
- **Task-bound** — tied to a specific TaskID and AgentID
- **Time-limited** — has an expiration time

### Token Structure

```rust
pub struct CapabilityToken {
    pub task_id: TaskID,
    pub agent_id: AgentID,
    pub allowed_tools: BTreeSet<ToolID>,
    pub allowed_intents: EnumSet<IntentType>,
    pub issued_at: Instant,
    pub expires_at: Instant,
    pub signature: HmacSha256Signature,  // kernel-signed
}
```

### Permission Check Flow

```
Agent emits intent
    → Kernel checks CapabilityToken signature (HMAC verification)
    → Checks agent's PermissionMatrix against requested resource
    → If denied: returns PermissionDenied error, logs to audit
    → If allowed: forwards intent to tool with scoped token
```

Permission checks happen at the Rust type level — there is no code path to bypass them.

---

## Permission System

Every agent has a **Permission Matrix** controlling resource access. Permissions use the Linux-style `rwx` model extended for AI-native resources.

### Permission Format

```
<resource_class>:<operations>
```

Where operations are:

- `r` — read
- `w` — write
- `x` — execute

**Examples:**

- `fs.user_data:rw` — read and write files
- `network.logs:r` — read network logs only
- `process.kill:x` — allowed to kill processes
- `agent.message:rx` — receive and send agent messages

### All Agents Start With Zero Permissions

By default, a newly connected agent has **no permissions**. Every permission must be explicitly granted:

```bash
agentctl perm grant analyst fs.user_data:r    # can now read files
agentctl perm grant analyst memory.semantic:rw # can read and write memory
```

### Time-Limited Permissions

Permissions can auto-expire:

```bash
agentctl perm grant analyst fs.system_logs:r --expires 2h
```

### Permission Profiles

Reusable sets of permissions for common agent roles:

```bash
agentctl perm profile create ops-agent \
  --description "Standard permissions for operational agents" \
  --permissions "network.logs:r,process.list:r,fs.app_logs:r"

agentctl perm profile assign analyst ops-agent
```

### Roles (RBAC)

Roles provide persistent, named permission sets that survive kernel restarts:

```bash
agentctl role create analyst-role \
  --description "Analyst agent role" \
  --permissions "fs.user_data:r,memory.semantic:rw"

agentctl role assign analyst analyst-role
```

---

## Tool Sandboxing (seccomp-BPF)

Tools that execute external processes are sandboxed using Linux seccomp-BPF:

### How It Works

1. Tool manifest declares sandbox constraints (network, fs_write, gpu, max_memory, max_cpu, syscalls)
2. `SandboxExecutor` creates a child process with a seccomp-BPF filter
3. Only whitelisted syscalls are allowed
4. Network access is blocked unless `network = true` in the manifest
5. Filesystem writes are blocked unless `fs_write = true`
6. If a tool attempts a forbidden syscall, the process is killed immediately

### `shell-exec` Isolation

The `shell-exec` tool uses `bwrap` (bubblewrap) for additional namespace and filesystem isolation:

- Mount namespace isolation — the tool sees only a minimal filesystem
- Path-based restrictions — only designated directories are accessible
- No access to the host filesystem beyond sandbox boundaries

---

## Audit Log

Every significant action in AgentOS is recorded in an append-only audit log:

| Event Types Logged                          |
| ------------------------------------------- |
| Task creation and completion                |
| Tool executions (input + output)            |
| Permission grants and revocations           |
| Secret access (metadata only, never values) |
| Agent connections and disconnections        |
| Agent-to-agent messages                     |
| Schedule and background task events         |
| Capability token issuance                   |

### Viewing Audit Logs

```bash
agentctl audit logs --last 100
```

The audit log is an append-only SQLite database. Only the kernel can write to it — no tool, agent, or external process can modify or delete entries.

---

## Deployment Security Acceptance

Before any deployment, the security acceptance suite **must pass in full**. This is a hard deployment gate — any failure blocks launch.

### How to Run

```bash
# Run the full security acceptance suite
cargo test -p agentos-kernel --test security_acceptance_test

# Verify all 7 scenarios are present
cargo test -p agentos-kernel --test security_acceptance_test -- --list 2>&1 | grep 'test ' | wc -l
# Expected output: 7
```

### The 7 Mandatory Scenarios

| # | Scenario | Component | What It Validates |
|---|----------|-----------|-------------------|
| A | Unsigned A2A message rejected | `AgentMessageBus` | Messages without a signature are refused at delivery |
| B | Forged signature rejected | `AgentMessageBus` | Tampered signatures fail Ed25519 verification |
| C | Secret scope denial enforced | `SecretsVault` | Agent B cannot access a secret scoped to Agent A |
| D | High-risk action escalated | `RiskClassifier` + `EscalationManager` | `Delegate` intents require hard approval before execution |
| E | Prompt injection detected | `InjectionScanner` | Known injection payloads (role override, system prompt exfil, delimiter injection) are flagged |
| F | Blocked trust tier rejected | `ToolRegistry` | Tools with `trust_tier = "blocked"` fail registration with `ToolBlocked` error |
| G | Invalid tool signature rejected | `ToolRegistry` / signing | Community-tier tools with invalid Ed25519 signatures fail registration with `ToolSignatureInvalid` |

### Expected Pass Criteria

All 7 tests must report `ok`. A passing run looks like:

```
running 7 tests
test scenario_a_reject_unsigned_message ... ok
test scenario_b_reject_forged_signature ... ok
test scenario_c_secret_scope_denial ... ok
test scenario_d_escalate_high_risk_action ... ok
test scenario_e_detect_prompt_injection ... ok
test scenario_f_block_tool_blocked_tier ... ok
test scenario_g_reject_tool_invalid_signature ... ok

test result: ok. 7 passed; 0 failed; 0 ignored
```

### What to Do If a Scenario Fails

| Failure | Root Cause | Resolution |
|---------|-----------|------------|
| Scenario A/B | A2A signature enforcement disabled or bypassed | Check `agent_message_bus.rs::send_direct()` — signature verification must run unconditionally |
| Scenario C | Secret scope check missing or skipped | Check `vault.rs::issue_proxy_token()` — scope must be compared against requestor's AgentID |
| Scenario D | Risk classifier not returning `HardApproval` for delegate intents | Check `risk_classifier.rs` — `IntentType::Delegate` must map to `ActionRiskLevel::HardApproval` |
| Scenario E | Injection scanner patterns out of date or scanner not called | Check `injection_scanner.rs` patterns; ensure scanner is called in `run_loop.rs` on LLM output |
| Scenario F/G | Tool registry accepting blocked/unsigned tools | Check `tool_registry.rs::register()` — trust tier and signature validation must run before any registration |

**A failure in any scenario is a hard block. Do not deploy until all 7 pass.**
