---
title: Agentic Tool Loop Flow
tags:
  - flow
  - tools
  - agentic
date: 2026-03-18
status: planned
---

# Agentic Tool Loop Flow

> Data and control flow through a complete pure agentic loop — from task receipt to completion — using the full post-plan tool inventory.

---

## Diagram

```mermaid
flowchart TD
    RECV[Task Received\nfrom kernel scheduler]

    subgraph PLAN["Phase 1 — Plan"]
        THINK[think\nExplicit reasoning step\naudit-logged]
        MAN[agent-manual\nDiscover available tools,\npermissions, events]
        TIME[datetime\nGet current UTC time\nfor deadlines / TTLs]
        PSEARCH[procedure-search\nCheck for existing\nhow-to procedures]
    end

    subgraph CONTEXT["Phase 2 — Build Context"]
        MSEARCH[memory-search\nRecall relevant\nsemantic knowledge]
        ASEARCH[archival-search\nRetrieve archived\nlong-term facts]
        BREAD[memory-block-read\nLoad labeled\nworking memory]
    end

    subgraph EXECUTE["Phase 3 — Execute"]
        FREAD[file-reader\nRead files]
        FWRITE[file-writer / file-editor\nWrite / patch files]
        FGLOB[file-glob / file-grep\nSearch files]
        FDIFF[file-diff\nCompare versions]
        SHELL[shell-exec\nRun processes\n— last resort —]
        HTTP[http-client\nRaw HTTP requests]
        WFETCH[web-fetch\nFetch + extract\nweb page text]
        DATA[data-parser\nParse JSON/CSV/TOML]
    end

    subgraph COORD["Phase 4 — Coordinate"]
        ALIST[agent-list\nDiscover available\npeer agents]
        AMSG[agent-message\nSend message\nto named agent]
        TDELG[task-delegate\nDelegate sub-task\nnon-blocking]
        TSTATUS[task-status\nPoll delegated\ntask outcome]
        TLIST[task-list\nReview own\ntask queue]
    end

    subgraph STORE["Phase 5 — Persist Results"]
        MWRITE[memory-write\nStore new knowledge\nin semantic memory]
        MDEL[memory-delete\nRemove stale\nmemory entries]
        BWRITE[memory-block-write\nUpdate labeled\nworking memory]
        PCREATE[procedure-create\nRecord successful\nstep-by-step procedure]
        MSTATS[memory-stats\nCheck tier usage\nbefore writing]
    end

    RECV --> THINK
    THINK --> MAN
    THINK --> TIME
    THINK --> PSEARCH

    MAN --> MSEARCH
    PSEARCH --> MSEARCH
    MSEARCH --> ASEARCH
    ASEARCH --> BREAD

    BREAD --> FREAD
    BREAD --> WFETCH
    FREAD --> FGLOB
    FGLOB --> FGREP

    FREAD --> FWRITE
    FWRITE --> FDIFF
    WFETCH --> DATA
    HTTP --> DATA

    FDIFF -->|Need peer| ALIST
    DATA -->|Need peer| ALIST
    ALIST --> AMSG
    ALIST --> TDELG
    TDELG -->|Poll| TSTATUS
    TLIST -->|Review queue| TDELG

    TSTATUS -->|Outcome| MWRITE
    FWRITE --> MWRITE
    MSTATS --> MWRITE
    MWRITE --> MDEL
    MWRITE --> BWRITE
    BWRITE --> PCREATE

    PCREATE -->|Done| DONE[Task Complete]
    MWRITE -->|Done| DONE
    SHELL -->|Done| DONE

    style THINK fill:#f39c12,color:#fff
    style TIME fill:#f39c12,color:#fff
    style ALIST fill:#8e44ad,color:#fff
    style TDELG fill:#8e44ad,color:#fff
    style TSTATUS fill:#8e44ad,color:#fff
    style TLIST fill:#8e44ad,color:#fff
    style WFETCH fill:#3498db,color:#fff
    style FDIFF fill:#3498db,color:#fff
    style MDEL fill:#e74c3c,color:#fff
    style MSTATS fill:#27ae60,color:#fff
    style PCREATE fill:#27ae60,color:#fff
```

**Legend:**
- Orange — cognitive scaffolding (think, datetime)
- Purple — coordination tier (new)
- Blue — content processing (new)
- Red — corrective memory operations (previously hidden)
- Green — memory persistence (previously hidden)

---

## Steps Walkthrough

### 1. Plan Phase

Every task begins with `think` — an explicit reasoning step that records intent in the audit log. Then `agent-manual` is queried to confirm tool availability, `datetime` anchors time-sensitive decisions, and `procedure-search` checks if a known procedure already exists for the task type.

### 2. Context Phase

The agent loads relevant memories: semantic facts via `memory-search`, archived long-term knowledge via `archival-search`, and labeled working state via `memory-block-read`. This builds the information base before any writes.

### 3. Execute Phase

The agent acts: reads/writes/searches files, fetches web content as text (not raw HTML), diffs versions to verify changes, and parses structured data. `shell-exec` is reserved for capabilities not covered by first-class tools (compilers, interpreters, etc.).

### 4. Coordinate Phase

If the task requires peers, the agent calls `agent-list` to discover who is available, sends messages or delegates sub-tasks, and polls `task-status` until sub-tasks resolve. `task-list` allows review of the current queue before creating new tasks.

### 5. Persist Phase

On completion, the agent stores new knowledge (`memory-write`), removes outdated entries (`memory-delete`), updates working state (`memory-block-write`), and — if the procedure was non-trivial and reusable — records it in `procedure-create`. `memory-stats` is checked first to avoid bloating memory tiers.

---

## Related

- [[Agentic Workflow Compatibility Plan]] — design rationale
- [[30-Pure Agentic Workflow Compatibility]] — implementation checklist
