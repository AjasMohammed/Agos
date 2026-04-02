---
title: "Phase 1.3: Marketplace UI"
tags:
  - web
  - registry
  - v3
  - plan
  - phase-1
date: 2026-03-30
status: planned
effort: 3d
priority: medium
---

# Phase 1.3: Marketplace UI

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a web frontend for the existing `agentos-registry` API — browse, search, and review tools and skills.

**Architecture:** New routes in `agentos-web` at `/marketplace` that query the `agentos-registry` REST API (port 8090). Server-rendered HTML with HTMX for interactivity. Adds reviews/ratings tables to the registry SQLite database.

**Tech Stack:** Axum (agentos-web), HTMX, Pico CSS, agentos-registry (extended)

---

## Why This Phase

The `agentos-registry` crate already has a working REST API with tool storage, versioning, and Ed25519 signing. But there's no way to discover tools except via CLI. OpenClaw's ClawHub has 13,729 skills. A marketplace UI turns the registry from infrastructure into a community asset.

## Current → Target State

**Current:** `agentos-registry` serves tools via REST at `/v1/tools/*`. No web UI. No ratings. No skill support.

**Target:** `/marketplace` in `agentos-web` with search, categories, trust tier badges, reviews. Registry extended with `artifact_type` (tool/skill), reviews table, and enhanced search.

## Files Changed

| File | Action | Purpose |
|------|--------|---------|
| `crates/agentos-web/src/handlers/marketplace.rs` | Create | Marketplace page handlers |
| `crates/agentos-web/src/router.rs` | Modify | Add `/marketplace` routes |
| `crates/agentos-web/src/templates/marketplace/` | Create | HTML templates (list, detail, review) |
| `crates/agentos-registry/src/main.rs` | Modify | Add reviews endpoints |
| `crates/agentos-registry/src/db.rs` | Modify | Add reviews table, skill artifact type |

## Dependencies

- **Requires:** Phase 1.1 (REST API), Phase 2.1 (Skills — for listing skills alongside tools)
- **Blocks:** Nothing

---

## Detailed Tasks

### Task 1: Extend Registry with Reviews Table

- [ ] Add `reviews` table to registry SQLite schema:
```sql
CREATE TABLE IF NOT EXISTS reviews (
    id INTEGER PRIMARY KEY,
    tool_name TEXT NOT NULL,
    author_key TEXT NOT NULL,
    rating INTEGER NOT NULL CHECK(rating BETWEEN 1 AND 5),
    body TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (tool_name) REFERENCES tools(name)
);
```

- [ ] Add `artifact_type TEXT NOT NULL DEFAULT 'tool'` column to tools table
- [ ] Add REST endpoints: `GET /v1/tools/{name}/reviews`, `POST /v1/tools/{name}/reviews`
- [ ] Add `GET /v1/stats` endpoint (total tools, skills, downloads)
- [ ] Commit

### Task 2: Marketplace List Page

- [ ] Create handler `marketplace::list` that fetches tools from registry API with search/filter params
- [ ] Create template `marketplace/list.html` with: search bar, category filter, trust tier filter, tool cards (name, description, tier badge, download count, average rating)
- [ ] Add route `GET /marketplace` to router
- [ ] Commit

### Task 3: Marketplace Detail Page

- [ ] Create handler `marketplace::detail` that fetches tool info + reviews
- [ ] Create template `marketplace/detail.html` with: name, description, author, version history, trust tier badge with color, README content, install command, reviews list
- [ ] Add route `GET /marketplace/{name}` to router
- [ ] Commit

### Task 4: Review Submission

- [ ] Create handler `marketplace::submit_review` that POSTs to registry
- [ ] Add review form to detail page (rating stars, body textarea)
- [ ] HTMX: submit review without page reload, append to reviews list
- [ ] Add route `POST /marketplace/{name}/review` to router
- [ ] Commit

## Verification

```bash
cargo build --workspace
cargo test -p agentos-registry
cargo test -p agentos-web
cargo clippy --workspace -- -D warnings
```
