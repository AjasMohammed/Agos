---
title: LLM Adapter Redesign Data Flow
tags:
  - llm
  - v3
  - flow
date: 2026-03-24
status: planned
effort: N/A
priority: critical
---

# LLM Adapter Redesign Data Flow

> How inference requests, tool calls, and streaming events flow through the redesigned LLM adapter layer.

---

## Agentic Inference Loop (Non-Streaming)

```mermaid
sequenceDiagram
    participant K as Kernel (task_executor)
    participant A as LLMAdapter
    participant R as RetryMiddleware
    participant P as Provider API

    K->>A: infer_with_tools(context, tools, options)
    A->>A: format_messages(context) [provider-native]
    A->>A: format_tools(manifests) [provider-native]
    A->>A: estimate_tokens() [pre-flight check]
    alt Token budget exceeded
        A-->>K: Err(ContextOverflow)
    end
    A->>R: send_request(body)
    R->>P: POST /chat/completions
    alt 429/5xx
        R->>R: exponential_backoff(attempt, retry_after_header)
        R->>P: POST /chat/completions (retry)
    end
    P-->>R: 200 OK + JSON response
    R-->>A: parsed response
    A->>A: parse_stop_reason()
    A->>A: parse_tool_calls() [provider-native]
    A->>A: calculate_cost(usage, pricing)
    A-->>K: InferenceResult { text, tool_calls, stop_reason, cost, usage }

    alt stop_reason == ToolUse
        K->>K: execute_tool_calls(result.tool_calls)
        K->>K: format_tool_results_native(results)
        K->>A: infer_with_tools(updated_context, tools, options)
        Note over K,A: Loop continues until StopReason::EndTurn or max_iterations
    end
```

## Streaming Inference with Tool Calls

```mermaid
sequenceDiagram
    participant K as Kernel
    participant A as LLMAdapter
    participant P as Provider API
    participant W as Web UI (SSE)

    K->>A: infer_stream_with_tools(context, tools, tx)
    A->>P: POST /chat/completions (stream: true)

    loop SSE chunks
        P-->>A: delta chunk
        alt Text delta
            A->>K: InferenceEvent::Token("chunk")
            K->>W: SSE: chat-token
        else Tool call start
            A->>A: accumulate tool_call[index]
            A->>K: InferenceEvent::ToolCallStart { name, index }
            K->>W: SSE: chat-tool-start
        else Tool call argument delta
            A->>A: append to tool_call[index].arguments
        else Tool call complete (content_block_stop / finish_reason)
            A->>K: InferenceEvent::ToolCallComplete { call }
        else Usage update
            A->>K: InferenceEvent::Usage { tokens }
        else Done
            A->>K: InferenceEvent::Done(InferenceResult)
        end
    end
```

## Tool Result Injection (Provider-Native)

```mermaid
flowchart TD
    TR[Tool Results from Kernel] --> SW{Provider?}

    SW -->|OpenAI| OAI["role: tool\ntool_call_id: call_xxx\ncontent: result_json"]
    SW -->|Anthropic| ANT["role: user\ncontent: [{type: tool_result,\ntool_use_id: toolu_xxx,\ncontent: result_text}]"]
    SW -->|Gemini| GEM["role: user\nparts: [{functionResponse:\n{name: fn, response: {...}}}]"]
    SW -->|Ollama| OLL["role: tool\ncontent: result_json"]

    OAI --> CTX[Append to ContextWindow]
    ANT --> CTX
    GEM --> CTX
    OLL --> CTX

    CTX --> NEXT[Next infer_with_tools() call]
```

## Retry and Circuit Breaker Flow

```mermaid
flowchart TD
    REQ[Outgoing Request] --> CB{Circuit Open?}
    CB -->|Open| FAIL[Return CircuitOpen error]
    CB -->|Closed/Half-Open| SEND[Send to Provider]

    SEND --> RESP{Response?}
    RESP -->|2xx| OK[Parse + Return]
    RESP -->|429| RL[Read retry-after header]
    RESP -->|500/502/503| ERR[Increment failure count]
    RESP -->|400/401/403| PERM[Return permanent error]

    RL --> WAIT[Sleep backoff + jitter]
    ERR --> WAIT
    WAIT --> RETRY{Attempts < max?}
    RETRY -->|Yes| SEND
    RETRY -->|No| TRIP[Trip circuit breaker]
    TRIP --> FAIL

    OK --> RESET[Reset failure count]
```

## Cost Attribution Flow

```mermaid
flowchart LR
    INF[InferenceResult] --> USAGE[TokenUsage]
    USAGE --> CALC["calculate_cost(\nusage,\nmodel_pricing\n)"]
    CALC --> COST[InferenceCost]
    COST --> RES[Attached to InferenceResult]
    RES --> CT[Kernel CostTracker]
    CT --> AUDIT[AuditLog: CostAttribution]
    CT --> BUDGET{Budget check}
    BUDGET -->|Over| STOP[Pause/Downgrade]
    BUDGET -->|Under| CONT[Continue]
```

## Context Compilation Integration

```mermaid
flowchart TD
    TASK[Agent Task] --> CC[ContextCompiler]
    CC --> SYS[System prompt partition]
    CC --> TOOLS[Tool definitions partition]
    CC --> MEM[Memory retrieval partition]
    CC --> HIST[Conversation history]
    CC --> CURR[Current user message]

    SYS --> CTX[ContextWindow]
    TOOLS --> CTX
    MEM --> CTX
    HIST --> CTX
    CURR --> CTX

    CTX --> EST[Token Estimator]
    EST --> TRIM{Over budget?}
    TRIM -->|Yes| EVICT[Evict lowest-importance entries]
    EVICT --> CTX
    TRIM -->|No| ADAPT[LLMAdapter.infer_with_tools]

    ADAPT --> FMT[format_messages - provider native]
    FMT --> API[Provider API call]
```

---

## Related

- [[LLM Adapter Redesign Plan]]
- [[LLM Adapter Redesign Research]]
