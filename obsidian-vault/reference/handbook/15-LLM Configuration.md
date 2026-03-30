---
title: LLM Configuration
tags:
  - reference
  - llm
  - configuration
  - v3
date: 2026-03-17
status: complete
---

# LLM Configuration

> AgentOS supports five LLM provider backends: Ollama (local), OpenAI, Anthropic, Gemini, and a custom OpenAI-compatible endpoint. Each agent is connected to exactly one backend.

---

## Supported Providers

| Provider | Type | API Style | Auth |
|---|---|---|---|
| `ollama` | Local / self-hosted | Ollama REST (`/api/chat`) | None |
| `openai` | Cloud | OpenAI Chat Completions | API key via vault |
| `anthropic` | Cloud | Anthropic Messages API | API key via vault |
| `gemini` | Cloud | Google Generative Language | API key via vault |
| `custom` | Any OpenAI-compatible | OpenAI Chat Completions | Optional API key |

---

## Provider Configuration

### Ollama

Ollama runs locally and requires no API key.

| Config key | Default | Override |
|---|---|---|
| `[ollama] host` | `http://localhost:11434` | `AGENTOS_OLLAMA_HOST` env var or production config |
| `[ollama] default_model` | `llama3.2` | Specified per-agent at connect time |

Endpoint resolution precedence:
1. `AGENTOS_OLLAMA_HOST` environment variable
2. `[ollama] host` in the active config file

Streaming: native NDJSON stream via `/api/chat`.

### OpenAI

API key stored in the AgentOS vault under the key `openai_api_key` (or any name you specify at `agentctl agent connect` time).

| Config key | Default | Override |
|---|---|---|
| `[llm] openai_base_url` | `https://api.openai.com/v1` | `AGENTOS_OPENAI_BASE_URL` env var |

Endpoint resolution precedence:
1. `AGENTOS_OPENAI_BASE_URL` environment variable
2. `--base-url` flag at `agentctl agent connect` time
3. `[llm] openai_base_url` in the active config file

### Anthropic

API key stored in the vault. The adapter sends it as the `x-api-key` header.

| Config key | Default | Override |
|---|---|---|
| `[llm] anthropic_base_url` | `https://api.anthropic.com/v1` | Production config or custom `--base-url` |

Anthropic uses a top-level `system` field (not a message role). The adapter separates system-role context entries automatically before sending.

### Gemini

API key stored in the vault. Sent as a query parameter.

| Config key | Default | Override |
|---|---|---|
| `[llm] gemini_base_url` | `https://generativelanguage.googleapis.com/v1beta` | Production config |

Gemini uses `user` / `model` roles. System prompt entries are passed in the `systemInstruction` field.

### Custom

Any OpenAI-compatible endpoint (vLLM, LM Studio, llama.cpp server, enterprise LLM gateway, etc.).

| Config key | Default | Override |
|---|---|---|
| `[llm] custom_base_url` | _(none in dev)_ | `AGENTOS_LLM_URL` env var or `--base-url` flag |

API key is optional. If not provided, requests are sent without authentication.

---

## Connecting Agents by Provider

Use `agentctl agent connect` to register a new agent backed by a specific LLM.

### Ollama (local)

```bash
agentctl agent connect --provider ollama --model llama3.2 --name local
```

### OpenAI

```bash
agentctl agent connect --provider openai --model gpt-4o --name researcher
```

API key must already be stored in the vault (value entered interactively):

```bash
agentctl secret set openai_api_key
# Enter value for 'openai_api_key' (input hidden): ▌
```

### Anthropic

```bash
agentctl agent connect --provider anthropic --model claude-sonnet-4-6 --name coder
```

Vault key: `anthropic_api_key`.

### Gemini

```bash
agentctl agent connect --provider gemini --model gemini-2.5-pro --name writer
```

Vault key: `gemini_api_key`.

### Custom OpenAI-compatible

```bash
agentctl agent connect \
  --provider custom \
  --model my-model \
  --name local-gpu \
  --base-url http://192.168.1.100:8080/v1
```

---

## Environment Variables

| Variable | Provider | Purpose |
|---|---|---|
| `AGENTOS_OLLAMA_HOST` | Ollama | Override the Ollama server URL |
| `AGENTOS_LLM_URL` | Custom | Override the custom provider base URL |
| `AGENTOS_OPENAI_BASE_URL` | OpenAI | Override OpenAI base URL (for proxies/gateways) |
| `RUST_LOG` | All | Set log level (e.g. `RUST_LOG=debug`) |

Environment variables take precedence over values in `config/default.toml` or `config/production.toml`.

---

## LLMCore Trait

All five adapters implement the `LLMCore` trait defined in `crates/agentos-llm/src/traits.rs`. This is the interface the kernel uses to run inference. You do not need to implement it to use AgentOS, but understanding the contract helps when reading logs or debugging.

| Method | Purpose |
|---|---|
| `infer(context)` | Send the full context window, get a complete response |
| `infer_stream(context, tx)` | Stream tokens incrementally via an mpsc channel |
| `health_check()` | Ping the backend; returns `Healthy`, `Degraded`, or `Unhealthy` |
| `capabilities()` | Return `ModelCapabilities` for this model |
| `provider_name()` | String identifier for logging (e.g. `"ollama"`, `"anthropic"`) |
| `model_name()` | The model string passed at connect time |

The `infer_stream` default implementation falls back to `infer` and sends the full result as a single token event. Ollama has a native streaming implementation that delivers tokens incrementally.

---

## Model Capabilities

Each adapter reports its `ModelCapabilities`, which the kernel uses to decide what features are available for a given agent.

| Provider | Context Window | Images | Tool Calls | JSON Mode |
|---|---|---|---|---|
| Ollama | 8,192 tokens | No | No | Yes |
| OpenAI | 128,000 tokens | Yes | Yes | Yes |
| Anthropic | 200,000 tokens | Yes | Yes | No (uses instructions) |
| Gemini | 1,000,000 tokens | Yes | Yes | Yes |
| Custom | 32,768 tokens | No | No | No |

> Custom provider defaults are conservative. The actual capabilities depend on the backend model. These values are static defaults in the adapter.

---

## API Key Handling

API keys are stored in the AgentOS vault (AES-256-GCM encrypted) and loaded at agent connection time. They are held in memory as `SecretString` (from the `secrecy` crate), which zeroes the memory on drop and prevents the key from appearing in debug output or logs.

Keys are never written to `config/default.toml` or `config/production.toml`. Always use the vault (values are entered interactively with hidden input — never as CLI arguments):

```bash
agentctl secret set anthropic_api_key
agentctl secret set openai_api_key
agentctl secret set gemini_api_key
```

---

---

## LLM Resilience: Fallback and Retry

### FallbackAdapter

The `FallbackAdapter` wraps multiple `LLMCore` providers and tries them in order. If the primary provider fails (any error — network, rate limit, server error), the adapter automatically retries the same request against the next provider in the list.

```
Primary → fails → Secondary → fails → Tertiary → ...
                                            ↓ all fail
                                     returns last error
```

**Key behaviours:**

- **Health check:** `health_check()` returns `Healthy` if *any* provider is healthy. The system is healthy as long as at least one backend is up.
- **Capabilities:** Reported from the first (primary) provider.
- **Streaming:** Uses an intermediate buffer channel when falling over mid-stream. Partial tokens from a failing provider are discarded before the next provider is tried, so the caller never sees a mixed stream.
- **Model name:** Reports the primary provider's model name.

The FallbackAdapter is constructed programmatically in application code (not via config). Example (Rust):

```rust
use agentos_llm::{FallbackAdapter, AnthropicAdapter, OllamaAdapter};

let primary = Arc::new(AnthropicAdapter::new(...));
let secondary = Arc::new(OllamaAdapter::new(...));
let fallback = FallbackAdapter::new(vec![primary, secondary])?;
```

### RetryPolicy

The `RetryPolicy` controls how individual provider adapters handle transient failures (rate limits, timeouts, temporary server errors) before the error propagates to the `FallbackAdapter`.

**Default retry policy:**

| Parameter | Default | Description |
|-----------|---------|-------------|
| `max_retries` | 3 | Maximum retry attempts after the initial request |
| `base_delay` | 1s | Starting delay before the first retry |
| `max_delay` | 60s | Upper cap on any single delay |
| `backoff_factor` | 2.0× | Exponential multiplier applied per attempt |

**Delay formula:** `delay = min(base_delay × backoff_factor^attempt + jitter, max_delay)`

Jitter is ±10% of the calculated delay, derived from the thread ID and clock nanoseconds to decorrelate concurrent callers hitting the same rate limit.

**Retryable HTTP status codes:** `408`, `429`, `500`, `502`, `503`, `504`, `529`

**Non-retryable:** `400`, `401`, `403`, `404` and other client errors. These return immediately and are NOT counted as circuit breaker failures.

**`Retry-After` header:** If the server returns a `Retry-After: <seconds>` header, that value is used directly as the delay (capped at `max_delay`). This ensures correct rate-limit backoff for providers that emit this header (e.g. Anthropic 529 overload responses).

### CircuitBreaker

The `CircuitBreaker` protects a provider from being hammered when it is known to be down.

**Default circuit breaker settings:**

| Parameter | Default | Description |
|-----------|---------|-------------|
| `failure_threshold` | 5 | Consecutive failures before the breaker trips |
| `cooldown` | 30s | Time before a half-open probe attempt is allowed |

**States:**

| State | Condition | Behaviour |
|-------|-----------|-----------|
| **Closed** | `consecutive_failures < threshold` | All requests pass through |
| **Open** | `consecutive_failures >= threshold` | Requests immediately return `CircuitBreaker is open` error |
| **Half-open** | Open + `cooldown` elapsed | One probe attempt allowed. Success → Closed; Failure → Open |

The breaker resets to Closed on any successful response. Client errors (4xx other than 408/429) do not increment the failure counter.

---

## Related

- [[16-Configuration Reference]] — full `[ollama]` and `[llm]` config sections
- [[09-Secrets and Vault]] — vault storage for API keys
- [[14-Audit Log]] — `LLMInferenceStarted`, `LLMInferenceCompleted`, `AgentConnected` events
