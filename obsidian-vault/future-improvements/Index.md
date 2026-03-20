---
title: Future Improvements Index
tags:
  - roadmap
  - security
  - future-improvements
date: 2026-03-19
status: active
---

# Future Improvements Index

> Tracks security hardening, performance improvements, and architectural enhancements planned for post-v1 implementation.

---

## Security Hardening

| # | Plan | Effort | Priority | Status | Phases |
|---|------|--------|----------|--------|--------|
| 1 | [[DNS SSRF Fix Plan]] | 3d | High | planned | 4 phases: resolver impl, client wiring, cleanup, integration tests |

### DNS SSRF Fix Phases

| Phase | Title | Effort | Status | Link |
|-------|-------|--------|--------|------|
| 01 | Implement `SsrfAwareDnsResolver` | 4h | planned | [[01-ssrf-resolver-impl]] |
| 02 | Wire resolver into WebFetch and HttpClientTool | 2h | planned | [[02-wire-resolver-into-clients]] |
| 03 | Cleanup and documentation | 1h | planned | [[03-cleanup-and-docs]] |
| 04 | Integration tests | 3h | planned | [[04-integration-tests]] |

---

## Notes

- This directory is for improvements that are planned but not yet scheduled for a specific release.
- Plans here are fully specified and ready for implementation -- they can be picked up and executed independently.
- For items scheduled in the current release cycle, see [[Index|Next Steps Index]].
