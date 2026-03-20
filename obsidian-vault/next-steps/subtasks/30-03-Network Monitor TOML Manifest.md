---
title: Network Monitor TOML Manifest
tags: [tools, network, next-steps]
date: 2026-03-18
status: complete
effort: 10m
priority: high
---

# Network Monitor TOML Manifest

> Create the missing `tools/core/network-monitor.toml` so the `network-monitor` tool is discoverable via the standard tool manifest loader.

## What to Do

1. Create `tools/core/network-monitor.toml` following the same structure as `http-client.toml`
2. Set `permissions = ["network.logs:r"]`
3. Set `network = false` (it reads sysinfo, not outbound network)

## Files Changed

| File | Change |
|------|--------|
| `tools/core/network-monitor.toml` | Create new manifest |

## Prerequisites

None.

## Verification

`cargo test -p agentos-tools` — manifest loader should now find this file.
