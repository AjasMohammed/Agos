---
trigger: always_on
glob:
description: Enforces a research-first approach before writing code for complex logic, third-party integrations, and unfamiliar domains.
---

# Research-First Rule

> **Do NOT write code for complex or third-party modules without researching first.**

---

## When This Rule Applies

Research is **mandatory** before writing any code that involves:

- **Third-party libraries or SDKs** — APIs, auth providers, payment gateways, cloud services, database drivers, etc.
- **Complex algorithms or domain logic** — cryptography, concurrency patterns, state machines, protocol implementations
- **Unfamiliar language features** — macros, unsafe blocks, advanced type system features, FFI
- **System-level integrations** — OS APIs, hardware interfaces, networking protocols, file system operations
- **Architecture decisions** — choosing between libraries, designing module boundaries, selecting patterns

---

## Research Process

### Step 1 — Understand the Problem

- Clearly define what the code needs to accomplish before looking at solutions
- Identify constraints: performance, compatibility, security, platform support

### Step 2 — Read Official Documentation

- **Always** check the official docs for the library/API/tool being used
- Use documentation lookup tools (Context7, MCP servers, official sites) to get **current** API signatures, configuration options, and usage patterns
- Never rely on memorized or outdated API knowledge — APIs change between versions

### Step 3 — Check Version Compatibility

- Verify the exact version of the library installed or specified in the project
- Read the changelog/migration guide if the version differs from what you're familiar with
- Look for breaking changes between major versions

### Step 4 — Review Examples & Patterns

- Find working examples from official docs, repos, or community resources
- Identify the idiomatic way to use the library — don't fight the framework
- Check for known gotchas, common mistakes, or anti-patterns

### Step 5 — Check Existing Codebase

- Search the current project for existing usage of the same library or pattern
- Follow established conventions already in the codebase — consistency matters
- Reuse existing wrappers, utilities, or abstractions if they exist

### Step 6 — Plan Before Coding

- For non-trivial work, outline the approach before writing implementation code
- Identify the files that need to change and the order of changes
- Consider error cases, edge cases, and failure modes upfront

---

## What NOT To Do

- **Don't guess API signatures** — look them up. Wrong function names, parameter orders, or return types waste time
- **Don't assume default behavior** — read what the defaults actually are
- **Don't copy patterns from a different version** — v2 code may not work in v3
- **Don't skip error handling research** — understand what errors a library can throw and how to handle them
- **Don't use deprecated APIs** — check if the function/method is still recommended
- **Don't start coding a complex feature without understanding the domain** — rushed implementations create tech debt

---

## Exceptions — When You Can Skip Research

- Simple, well-understood operations (basic CRUD, string manipulation, standard data structures)
- Code you've already researched and verified in this same session
- Trivial changes (renaming, reformatting, moving code, fixing typos)
- Using only language built-ins with no external dependencies

---

## Summary

| Situation                       | Action                                                          |
| ------------------------------- | --------------------------------------------------------------- |
| Using a new third-party library | **Research first** — read docs, check version, find examples    |
| Complex algorithm or pattern    | **Research first** — understand the approach, review references |
| Unfamiliar API or SDK           | **Research first** — verify signatures, check current docs      |
| Simple CRUD / basic logic       | Code directly — no research needed                              |
| Trivial refactor or rename      | Code directly — no research needed                              |

> **5 minutes of research prevents 30 minutes of debugging wrong assumptions.**
