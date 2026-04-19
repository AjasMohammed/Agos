---
title: Memory and context architecture for AI agent operating systems
tags: [plan, memory, context]
---
# Memory and context architecture for AI agent operating systems

**The central challenge of building an AgentOS is not intelligence—it's context.** Every production agent system today converges on the same insight: LLM performance degrades sharply when context windows are overloaded with irrelevant tools, stale memories, and unstructured history. The solution is a tiered memory architecture with dynamic context assembly—treating the context window like RAM that must be carefully managed, not a bucket to fill. Research from 2024–2026 demonstrates that **selective context loading outperforms full-context approaches by 20–90% on accuracy while cutting token costs by 85–98%**, and every major AI lab has independently arrived at variations of this architecture. This report synthesizes the concrete, implementable techniques that define the state of the art across ten dimensions of agent context management.

---

## The case against loading everything into context

The most counterintuitive finding in agent systems research is that **giving an LLM more information often makes it perform worse**. The "Less is More" paper (Paramanayakam et al., 2024) demonstrated this quantitatively: on a GeoEngine benchmark with 46 tools, Llama3.1-8b failed completely, but reducing to 19 relevant tools yielded successful completion with **43% faster execution and 19% lower power consumption**. Further optimization to 3–5 tools per query achieved 70% time reduction.

This effect compounds at scale. Chroma Research's "Context Rot" evaluation of 18 frontier LLMs found that **every single model gets worse as input length increases**, even on simple tasks. The degradation is not limited to context window boundaries—it occurs continuously. The Manus production agent system reported "Context Confusion" with 100+ tools: the model hallucinates parameters and calls wrong tools. Even Claude Code automatically enables tool search when MCP tool descriptions would consume more than 10% of the context window.

The quantitative evidence is unambiguous. RAG-MCP demonstrated that basic retrieval boosts tool selection accuracy from **13% to 43%** in large toolsets. MCP-Zero achieved **98% token reduction** while preserving accuracy. Anthropic's Tool Search Tool improved Opus 4 accuracy from 49% to 74% while saving 85% of tokens. The pattern is clear: selective, dynamic context assembly dramatically outperforms static context loading.

---

## A four-tier memory architecture grounded in cognitive science

The CoALA framework (Sumers et al., 2024) provides the definitive theoretical foundation for agent memory, drawing from cognitive psychology's memory taxonomy and mapping it onto LLM agent systems. Every successful production system implements some version of this hierarchy.

**Working memory** maps directly to the LLM's context window. It is limited-capacity, always accessible, and serves as the central hub between the model, long-term storage, and the environment. In Soar's cognitive architecture, working memory is a dynamic graph of current perceptions, goals, and reasoning states. In ACT-R, it consists of dedicated buffers—each holding exactly one chunk—creating a serial bottleneck that forces prioritization. For an AgentOS, working memory should be a **structured, compiled view** (Google ADK's insight) assembled from underlying state, not raw message history.

**Episodic memory** stores records of specific past events. MemGPT/Letta implements this as "Recall Storage"—evicted conversation history searchable via `conversation_search` and `conversation_search_date`. Zep/Graphiti takes this further with a bi-temporal knowledge graph that tracks four timestamps per edge: when the system learned a fact and when it was actually true in the world. This **bi-temporal modeling** enables sophisticated temporal queries like "what did we believe about X last month?" and is critical for enterprise audit trails.

**Semantic memory** holds factual knowledge about the world, independent of when it was learned. Mem0 implements this through a hybrid datastore (vector + graph + key-value) with a two-phase pipeline: an extraction phase where an LLM identifies salient facts from conversations, and an update phase where a conflict detector determines ADD, UPDATE, DELETE, or NONE operations against existing memories. On the LOCOMO benchmark, Mem0 achieves **26% higher accuracy than OpenAI Memory with 91% lower latency and 90% fewer tokens**.

**Procedural memory** stores how to do things—skills, SOPs, and tool usage patterns—and is the most underexplored tier. The Voyager system demonstrated the power of code-as-skills: executable JavaScript functions stored with natural language descriptions, retrievable via semantic search. Skills are compositional (they can call each other), version-controllable, and testable. Reflexion stores verbal self-hints from past failures as episodic guidance for future attempts (+22% on AlfWorld, +20% on HotPotQA). ExpeL extracts cross-task generalizable insights from success/failure pairs. The PRAXIS system indexes state-action-result exemplars by environmental state, enabling retrieval by matching the agent's current situation to past experiences.

The critical distinction between episodic and procedural memory: episodic answers "what happened when I tried X?", while procedural answers "what is the best way to do X?". **Episodic memory records specific instances; procedural memory generalizes across them.** An AgentOS should support consolidation pathways: episodic memories of repeated successes should be distilled into procedural skills through extraction and abstraction.

---

## Dynamic tool discovery eliminates context bloat

The MCP ecosystem now has over 5,000 servers, and each tool definition consumes 200–500+ tokens. Loading even a fraction into context creates the "tool sprawl" problem: token bloat crowds out reasoning space, attention spreads thinly over many options, and the model increasingly selects wrong tools or hallucinates parameters.

The most production-ready solution is **RAG over tool descriptions**. The implementation pipeline follows a consistent pattern across research: construct tool documents (name + description + parameters + synthetic usage questions), embed them using weighted averaging (ScaleMCP's TDWA strategy selectively weights different fields), index in a vector store, and retrieve top-k candidates at query time with cross-encoder reranking. ScaleMCP demonstrated **Recall@5 of 0.94** and agent task completion up to 94.4% across 5,000 financial metric MCP servers with sub-200ms retrieval latency.

Three architectural patterns have proven effective in production:

The **meta-tool pattern** equips agents with a single always-available `tool_search` function that queries the registry on demand. When the agent identifies a capability gap, it searches for and loads specific tools. This is the approach Anthropic implemented in Claude Code and their Tool Search Tool, yielding accuracy improvements of 25+ percentage points.

The **hierarchical action space** (pioneered by the Manus agent system, rewritten five times in six months) keeps ~20 core tools always loaded at Level 1, uses CLI/bash for utility operations at Level 2 (keeping definitions out of context), and delegates complex logic to code generation at Level 3. This mirrors AnyTool's three-tier hierarchical API retriever, which achieves **+35.4% pass rate** over ToolLLM.

The **active tool request pattern** (MCP-Zero) lets the LLM itself generate structured tool requirement specifications when it identifies a capability gap, using hierarchical vector routing for coarse-to-fine retrieval. This achieved 98% token reduction while standard tool-calling methods showed exponential growth in token costs with collection size.

A complementary technique leverages **tool-calling inertia** (AutoTool, 2025): tool invocations follow predictable sequential patterns with ~66% entropy reduction, enabling directed graphs of transition probabilities to predict likely next tools and bypass LLM inference for predictable transitions. This reduces inference costs by up to 30%.

---

## Context compression preserves signal while cutting noise

When full context cannot be avoided, compression techniques can dramatically reduce token counts. The LLMLingua family (Microsoft Research) achieves **up to 20x compression** with minimal performance loss by using a small language model's perplexity to identify and remove non-essential tokens. LLMLingua-2 reformulated this as token classification using a BERT-level encoder, running **3–6x faster** with better out-of-domain generalization—and it's integrated into both LangChain and LlamaIndex.

Gist tokens (Mu et al., NeurIPS 2023) take a different approach, training LMs to compress prompts into virtual tokens via modified attention masks during instruction finetuning—achieving **26x compression** and 40% FLOPs reduction. The Infini-Transformer (Google, 2024) achieves 100x compression via incremental linear attention for memory updates, and the 500xCompressor pushes to the extreme of compressing extensive contexts into a single special token.

For agent-specific workloads, ACON (Agent Context Optimization, 2025) reduces peak token usage by **26–54%** while preserving over 95% accuracy, using guideline optimization via failure analysis. MemAgent uses RL to dynamically compress context by learning what to overwrite in fixed-size memory slots, scaling from 8K training to 3.5M-token documents with under 5% degradation.

A critical finding for RAG-based systems: extractive compression using rerankers often **improves** accuracy while achieving 2–10x compression, because filtering noise helps more than including everything. On 2WikiMultihopQA, extractive reranker-based compression achieved +7.89 F1 points at 4.5x compression.

---

## What LLMs actually attend to shapes context design

The "Lost in the Middle" phenomenon (Liu et al., 2024) fundamentally constrains how context should be structured. Performance is highest when relevant information appears at the **beginning or end** of context and drops by **more than 30%** when critical information sits in the middle. This primacy-recency bias holds even for explicitly long-context models.

Mechanistic research reveals the primacy effect stems from autoregressive attention properties and "attention sinks," while the recency effect aligns with short-term memory demands in training data. Larger models show reduced U-shaped curves, and bidirectional encoder-decoders (T5) show flatter serial position effects—but the pattern persists across architectures.

Practical implications are direct: place system instructions first, the current task last, and avoid burying critical information in the middle. Use supplementary context in middle positions. Research shows using only **70–80% of context window capacity** is optimal, with degradation appearing at 80–90% utilization. Output token generation speed also decreases with more input tokens—a hidden cost often overlooked.

For token budget allocation, production-tested baselines suggest: system instructions at **10–15%**, tool descriptions at **15–20%**, retrieved knowledge at **30–40%**, conversation history at **15–25%**, and current task at **5–10%**, always reserving **20–25%** for output generation. The TALE framework (ACL 2025) demonstrated that dynamically allocating token budgets based on problem complexity reduces costs by **45.3%** with minimal accuracy loss. Trigger summarization at 70–80% capacity using recursive summarization for evicted messages.

---

## RAG as the agent's retrieval backbone

The evolution from naive RAG to agentic RAG represents a fundamental shift: retrieval becomes an active agent capability rather than a passive pipeline. The most important development is **adaptive retrieval**—agents deciding when to retrieve at all. Self-RAG (ICLR 2024 Oral) trains the LLM to emit reflection tokens (`[Retrieve=Yes/No]`, `[ISREL]`, `[ISSUP]`, `[ISUSE]`) that control retrieval and self-critique. Probing-RAG attaches classifiers to intermediate transformer layers, **skipping retrieval in 57.5% of cases** while exceeding baselines by 6–8 accuracy points.

For an AgentOS, the recommended architecture uses **multi-index RAG** with separate indexes for different information types: tools, episodic memories, domain knowledge, user preferences, and procedural skills. A lightweight router examines incoming queries and determines which indexes to query using vector, keyword, or graph search. LlamaIndex's Composite Retrieval provides a production-ready implementation of this pattern with intelligent index selection and reranking.

Hybrid retrieval combining vector search with BM25 keyword search and graph traversal consistently outperforms any single method. The pipeline should run multiple retrievers in parallel, fuse results, apply cross-encoder reranking (which improves RAG accuracy by **20–35%** with 200–500ms latency), and feed top-k results to the LLM. Temporal-aware retrieval adds `valid_from`/`valid_until` filtering to prevent outdated information from reaching the model despite high embedding similarity.

GraphRAG deserves special attention for agent systems. HippoRAG 2 (ICML 2025), inspired by hippocampal indexing theory, combines knowledge graphs with Personalized PageRank to achieve **7% improvement** over state-of-the-art embedding models on associative memory tasks. Microsoft's GraphRAG excels at open-ended questions requiring global perspective, and combining it with vector retrieval covers over 90% of knowledge QA needs. Graph-based approaches reduce hallucination rates by up to 90%.

---

## How production systems actually manage context

The major AI labs have converged on remarkably similar architectures despite independent development, validating core design principles.

**Anthropic** leads in context engineering philosophy. Their Tool Search Tool marks tools with `defer_loading: true` so they're not loaded initially—Claude discovers them on demand, improving accuracy by 25+ percentage points while saving 85% of tokens. Server-side compaction summarizes conversation contents when approaching limits. Context editing automatically clears stale tool calls and results, reducing token consumption by **84% in 100-turn evaluations**. Their "just-in-time context" philosophy maintains lightweight identifiers (file paths, queries, links) and dynamically loads data at runtime.

**Google ADK** treats context as a first-class architectural primitive with four explicit layers: Working Context (compiled view rebuilt per invocation), Session (durable event log), Memory (long-lived searchable knowledge), and Artifacts (large binary/textual data). Context compaction uses an LLM to summarize older events over a sliding window. Context caching identifies stable prefixes (system prompts, tool definitions) and reuses cached attention mechanisms, reducing time-to-first-token and costs by orders of magnitude.

**Letta (MemGPT)** implements the most explicitly OS-inspired architecture. Agents self-manage memory using tools: `memory_replace`, `memory_insert`, `memory_rethink` for in-context blocks; `archival_memory_insert`/`search` for long-term storage. Memory blocks are discrete, labeled, size-limited units (default 2K characters) that agents actively edit. Sleep-time compute offloads memory consolidation to asynchronous agents, preventing response-time degradation.

**OpenAI's Agents SDK** provides context trimming (drop older turns) and context compression (summarize older turns) as configurable strategies. The `RunContextWrapper` enables structured state objects that persist across runs. However, there is no built-in long-term memory—the SDK intentionally remains lightweight, expecting external solutions like Mem0.

**LangGraph** uses reducer-driven state schemas where reducer functions control how state updates are merged, preventing data loss in multi-agent systems. Checkpointing saves state at every graph superstep with production backends (PostgreSQL, MongoDB, Redis). The `trim_messages` utility handles token-based truncation.

---

## Cognitive architectures as design blueprints

Classical cognitive architectures provide proven design patterns that map directly onto AgentOS components. **Soar's** decision cycle (state → operator proposal → selection → application) mirrors ReAct loops, and its chunking mechanism—compiling complex multi-step reasoning into automatic production rules—directly parallels caching successful plans as procedural memory. When Soar reaches an impasse, it creates substates for deeper reasoning, analogous to an agent spawning sub-agents for complex tasks.

**ACT-R's** activation-based retrieval offers a directly implementable memory scoring formula: `score = base_activation(recency, frequency) + spreading_activation(context_similarity) + noise`. Each memory chunk's accessibility depends on how recently and frequently it was accessed, plus how contextually relevant it is to the current goal. This maps onto the composite scoring functions used by production systems like CrewAI, which blends `recency_weight`, `semantic_weight`, and `importance_weight` with configurable half-life decay.

**Global Workspace Theory** provides perhaps the most relevant framing. Multiple specialized modules operate in parallel, competing for access to a limited-capacity global workspace—and the context window **is** that workspace. Information that wins competition (through relevance scoring) gets broadcast to all downstream processes. The bottleneck is a feature: it forces prioritization. For an AgentOS, this means treating different memory types as parallel modules that propose relevant information, with a selection mechanism determining what enters context.

---

## Recommended architecture for AgentOS

Based on this research, the optimal memory and context management layer combines these components into a unified system:

- **Tool Registry with Vector Index**: Embed all tool descriptions using weighted averaging (name + description + parameters + synthetic usage queries). Equip agents with a single `tool_search` meta-tool for on-demand discovery. Keep ~15–20 core tools always loaded; everything else loads dynamically. Track tool usage patterns via transition graphs for predictive loading.

- **Four-Tier Memory Store**: Working memory as structured, labeled blocks in context (Letta pattern). Episodic memory in a vector database with rich metadata (timestamps, user_id, outcomes). Semantic memory in a knowledge graph with bi-temporal modeling (Graphiti pattern). Procedural memory as a code-based skill library with semantic retrieval (Voyager pattern).

- **Memory Operations Pipeline**: Extract salient facts via LLM analysis (Mem0 pattern). Evaluate against existing memories for conflicts. Consolidate through ADD/UPDATE/DELETE/MERGE operations. Retrieve via hybrid search combining vector similarity, keyword matching, graph traversal, and temporal filtering.

- **Context Assembly Engine**: Compile a fresh context view per invocation from underlying state (Google ADK insight). Place system instructions first, current task last, supplementary context in middle. Trigger summarization at 70–80% capacity. Use extractive compression via reranking for RAG results. Reserve 20–25% of window for output.

- **Adaptive Retrieval Gate**: Before any retrieval, assess whether it's needed (Self-RAG pattern). Route to appropriate indexes based on query type. Retrieve 20–30 candidates, rerank to top 5–7 via cross-encoder. Support self-reflection loops for iterative refinement.

## Conclusion

The convergence across research labs and production systems points to a clear architectural thesis: **agent intelligence is bounded by context quality, not model capability**. The most effective systems treat the context window as a scarce resource requiring active management—not a passive container. Three insights stand out as non-obvious. First, compression and filtering often *improve* accuracy rather than merely trading it for efficiency, because noise removal helps more than comprehensive inclusion. Second, letting agents self-manage their memory (the Letta pattern) produces better results than framework-imposed heuristics, because the model can make semantic judgments about importance that rule-based systems cannot. Third, the tool discovery problem is fundamentally a retrieval problem—the same RAG techniques that work for document QA work for finding the right tool from thousands of candidates, with sub-200ms latency at scale. An AgentOS built on these principles—tiered memory with cognitive-science-informed scoring, dynamic tool discovery via semantic retrieval, compiled context views assembled per invocation, and adaptive retrieval gating—would represent the current state of the art in agent context management.