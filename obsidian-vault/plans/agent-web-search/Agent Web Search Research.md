---
title: Agent Web Search Research
tags:
  - research
  - search
  - agentic-workflow
  - tools
date: 2026-03-25
status: complete
effort: —
priority: high
---

# Agent Web Search Research

> Research findings from NotebookLM (AI Agent Frameworks & AgentOS 2025-2026 notebook) on the feasibility, reliability, and correct architecture for web search in AI agent systems.

---

## Key Findings

### 1. HTML Scraping Is Fragile — Not Production-Grade

HTML scraping relies on the specific DOM structure of a search engine's page. When a search engine updates its layout (even slightly), CSS selectors fail silently — the agent receives empty or malformed results with no error signal.

In contrast, official search APIs return consistent structured JSON regardless of visual design changes. Production-grade agentic systems (AgentOS, PydanticAI) use schema-first designs where every input is validated against strict models — a requirement that is significantly harder to satisfy with unstructured scraped HTML.

**Conclusion:** HTML scraping can exist as a fallback but must never be the primary search mechanism.

### 2. Snippets Alone Cause Semantic Drift and Hallucination Spirals

Search snippets are truncated, sometimes misleading, and optimized for human skimming — not LLM reasoning. Known failure modes when agents reason on snippets alone:

- **Semantic drift** — irrelevant snippets cause gradual loss of focus on the original goal
- **Hallucination spirals** — inconsistent or stale results cause agents to break down objectives into logically incoherent sub-tasks
- **Data integrity risk** — malicious content disguised as search data (prompt injection via search results)

**Conclusion:** Snippets are a URL discovery signal only. The agent must fetch the actual page content for anything it needs to reason on accurately.

### 3. Production Frameworks Use a Two-Phase Pattern

All major agentic frameworks implement a search→fetch pipeline:

| Framework | Approach |
|---|---|
| **AutoGPT / AutoGen** | Browser-native; headless browser navigates directly to pages, not just snippets |
| **CrewAI** | Dedicated "Web Scraping & Browsing" + "Search & Research" tools — two separate tools |
| **LangChain / LangGraph** | Document Loaders + Text Splitters ingest full page text into vector store for RAG |
| **MCP ecosystem** | JSON-RPC 2.0 search tool servers — structured results, then agents fetch full content |

**Conclusion:** The pattern is: `search` (discovery layer) → `web-fetch` (accuracy layer). This is universal across production systems.

### 4. SearXNG Is the Best Self-Hosted Option

SearXNG is a meta-search engine that aggregates multiple engines (Google, Bing, DDG, etc.) and exposes a stable JSON API. It handles:
- Rate limiting and bot detection internally (distributes across engines)
- Layout changes of individual engines (its parsers are maintained by the project)
- No API key requirement
- Privacy-respecting (no tracking)

Resource footprint is heavier than a custom tool (Python/Flask, ~150-300MB RAM) but far lighter than a full browser-based solution. As a sidecar daemon, it fits AgentOS's Unix socket IPC pattern.

**Conclusion:** SearXNG is the production target. Custom scraping is a stepping stone.

### 5. DuckDuckGo Has Two Useful Endpoints

| Endpoint | Type | Reliability | Returns |
|---|---|---|---|
| `https://api.duckduckgo.com/?q=...&format=json` | Official Instant Answer API | Very high — official, stable | Instant answers, related topics, Wikipedia summaries |
| `https://html.duckduckgo.com/html/?q=...` | HTML lite endpoint | Medium — designed for low-bandwidth, more scraper-tolerant than main site | Full search results as HTML |

The Instant Answer API does **not** return a ranked list of web results — it returns "instant answers" (definitions, Wikipedia extracts, etc.). For full web search results, the HTML endpoint must be scraped.

### 6. Validation Before Context Injection Is Essential

Production systems employ multiple layers before injecting search data into the agent's context window:

- **Critic/Reviewer agents** (LangGraph, AutoGen) — evaluate intermediate results; trigger refinement loops if insufficient
- **Injection scanner** (AgentOS) — already implemented; inspects all tool outputs for Unicode homoglyph attacks, prompt injection patterns
- **Schema validation** (PydanticAI, AgentOS) — type-safe validated outputs ensure consistent structure
- **Type safety** — automatic retries on invalid results before they reach the context window

**Conclusion:** AgentOS already has the injection scanner and schema validation needed. The search tool output must be structured JSON that flows through these existing gates — not raw HTML injected into the prompt.

---

## Architecture Recommendation

```
Phase 1 (now):    DDG Instant Answer API → stable, zero deps, factual queries
Phase 2 (next):   DDG HTML scraper → full ranked results, fragile but useful
Phase 3 (then):   Agent guidance for two-phase search→fetch pattern
Phase 4 (later):  SearXNG self-hosted → production-grade, multi-engine, no fragility
```

---

## Sources

- NotebookLM notebook: *AI Agent Frameworks & AgentOS 2025-2026*
- Query session 1: Feasibility, lightest-weight approaches, Rust service, API design (2026-03-25)
- Query session 2: Reliability, rate limiting, sidecar patterns, result formats (2026-03-25)
- Query session 3: HTML scraping accuracy, production framework patterns, validation (2026-03-25)

---

## Related

- [[Agent Web Search Plan]] — implementation plan
- [[01-ddg-instant-answer-tool]] — Phase 1
- [[02-html-search-scraper]] — Phase 2
- [[03-search-fetch-pipeline]] — Phase 3
- [[04-searxng-integration]] — Phase 4
