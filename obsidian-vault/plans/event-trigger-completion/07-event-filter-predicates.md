---
title: "Phase 07 — Event Filter Predicates"
tags:
  - kernel
  - event-system
  - plan
  - v3
date: 2026-03-13
status: planned
effort: 4h
priority: medium
---
# Phase 07 — Event Filter Predicates

> Implement the filter predicate DSL so subscriptions like `cpu_percent > 85` and `severity == Critical` actually evaluate against event payloads instead of always passing.

---

## Why This Phase

The event bus currently ignores filter predicates — `evaluate_subscriptions()` has a comment like "Phase 2: always pass" in the filter check. This means every subscription matches every event of its type, regardless of filter conditions. Without working filters, agents get flooded with irrelevant events. The spec (§9) defines filters like `cpu_percent > 90`, `severity == Critical`, `tool_id IN ['http-client', 'shell-exec']` — all currently non-functional.

---

## Current State

| What | Status |
|------|--------|
| `EventSubscription.filter: Option<EventFilter>` | Defined — but `EventFilter` is likely just a `String` |
| Filter evaluation in `evaluate_subscriptions()` | **Stubbed** — always returns true |
| CLI parses `--filter "cpu_percent > 90"` | Passes string through, never evaluated |

---

## Target State

A simple predicate evaluator that:
- Parses filter strings like `field == "value"`, `field > number`, `field IN [list]`
- Supports `AND` conjunction (no `OR` needed for v1)
- Evaluates predicates against `event.payload` (which is `serde_json::Value`)
- Returns `true` (match) or `false` (skip)
- On parse error: logs a warning, returns `true` (fail-open) to avoid silently dropping events

---

## Subtasks

### 1. Define the filter predicate types

**File:** `crates/agentos-kernel/src/event_bus.rs` (or a new `event_filter.rs` if cleaner)

```rust
#[derive(Debug, Clone)]
pub enum FilterOp {
    Eq,          // ==
    NotEq,       // !=
    Gt,          // >
    Gte,         // >=
    Lt,          // <
    Lte,         // <=
    In,          // IN [list]
    Contains,    // contains (substring match)
}

#[derive(Debug, Clone)]
pub enum FilterValue {
    String(String),
    Number(f64),
    Bool(bool),
    List(Vec<String>),
}

#[derive(Debug, Clone)]
pub struct FilterPredicate {
    pub field: String,       // JSON path into event.payload, e.g., "cpu_percent"
    pub op: FilterOp,
    pub value: FilterValue,
}

#[derive(Debug, Clone)]
pub struct EventFilterExpr {
    pub predicates: Vec<FilterPredicate>,  // AND-joined
}
```

### 2. Implement the filter parser

**File:** `crates/agentos-kernel/src/event_bus.rs` (or `event_filter.rs`)

Parse a filter string into `EventFilterExpr`:

```rust
pub fn parse_filter(filter_str: &str) -> Result<EventFilterExpr, String> {
    // Split on " AND " (case-insensitive)
    // For each clause, parse: field op value
    //
    // Examples:
    //   "cpu_percent > 85"         → field="cpu_percent", op=Gt, value=Number(85.0)
    //   "severity == Critical"     → field="severity", op=Eq, value=String("Critical")
    //   "tool_id IN ['a', 'b']"    → field="tool_id", op=In, value=List(["a", "b"])
    //   "severity == Critical AND cpu_percent > 90"  → two predicates
    //
    // Parsing approach:
    // 1. Split on " AND "
    // 2. For each clause, find operator token (==, !=, >=, <=, >, <, IN)
    // 3. Left of operator = field name (trimmed)
    // 4. Right of operator = value (trimmed, parsed by type)
    //    - Quoted string → String
    //    - Number → Number
    //    - true/false → Bool
    //    - [list] → List
}
```

### 3. Implement the filter evaluator

**File:** `crates/agentos-kernel/src/event_bus.rs` (or `event_filter.rs`)

Evaluate an `EventFilterExpr` against a `serde_json::Value` payload:

```rust
pub fn evaluate_filter(filter: &EventFilterExpr, payload: &serde_json::Value) -> bool {
    // All predicates must match (AND semantics)
    filter.predicates.iter().all(|pred| evaluate_predicate(pred, payload))
}

fn evaluate_predicate(pred: &FilterPredicate, payload: &serde_json::Value) -> bool {
    // 1. Extract field value from payload using pred.field as JSON key
    //    (support nested paths like "task.agent_id" via dot notation)
    let field_value = get_json_field(payload, &pred.field);

    // 2. If field doesn't exist in payload, predicate fails (returns false)
    let field_value = match field_value {
        Some(v) => v,
        None => return false,
    };

    // 3. Compare field_value against pred.value using pred.op
    match (&pred.op, &pred.value) {
        (FilterOp::Eq, FilterValue::String(s)) => {
            field_value.as_str().map(|v| v == s).unwrap_or(false)
        }
        (FilterOp::Gt, FilterValue::Number(n)) => {
            field_value.as_f64().map(|v| v > *n).unwrap_or(false)
        }
        (FilterOp::In, FilterValue::List(list)) => {
            field_value.as_str().map(|v| list.contains(&v.to_string())).unwrap_or(false)
        }
        // ... other combinations
        _ => false,
    }
}

fn get_json_field<'a>(value: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut current = value;
    for key in path.split('.') {
        current = current.get(key)?;
    }
    Some(current)
}
```

### 4. Wire the evaluator into `evaluate_subscriptions()`

**File:** `crates/agentos-kernel/src/event_bus.rs`

**Where:** Replace the "always pass" filter check with actual evaluation.

```rust
// In evaluate_subscriptions(), where filter is checked:
if let Some(ref filter_str) = subscription.filter {
    match parse_filter(filter_str) {
        Ok(expr) => {
            if !evaluate_filter(&expr, &event.payload) {
                // Filter didn't match — skip this subscription
                // Optionally audit: EventFilterRejected
                continue;
            }
        }
        Err(parse_err) => {
            // Log warning, but deliver anyway (fail-open)
            tracing::warn!(
                "Failed to parse filter for subscription {}: {}",
                subscription.id, parse_err
            );
        }
    }
}
```

### 5. Parse filter at subscription creation time (optimization)

**File:** `crates/agentos-kernel/src/event_bus.rs`

Currently the filter is stored as a `String` (or `Option<String>`). For efficiency, parse the filter once at subscription creation time and store the parsed `EventFilterExpr` alongside it. This avoids re-parsing on every event.

If `EventSubscription.filter` is currently `Option<String>`, consider changing to:

```rust
pub struct EventSubscription {
    // ... existing fields ...
    pub filter_raw: Option<String>,           // Original filter string
    pub filter_parsed: Option<EventFilterExpr>, // Parsed at creation time
}
```

Populate `filter_parsed` in the `subscribe()` method.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-kernel/src/event_bus.rs` | Add filter types, parser, evaluator; wire into `evaluate_subscriptions()` |
| `crates/agentos-types/src/event.rs` | Possibly update `EventSubscription` struct to hold parsed filter (or keep parse in event_bus) |

---

## Dependencies

None — the filter evaluator is self-contained. But Phase 08 (dynamic subscriptions) builds on this.

---

## Test Plan

1. **Parser tests:**
   - `"cpu_percent > 85"` → `FilterPredicate { field: "cpu_percent", op: Gt, value: Number(85.0) }`
   - `"severity == Critical"` → `FilterPredicate { field: "severity", op: Eq, value: String("Critical") }`
   - `"tool_id IN ['http-client', 'shell-exec']"` → `FilterPredicate { field: "tool_id", op: In, value: List([...]) }`
   - `"cpu_percent > 85 AND severity == Critical"` → two predicates

2. **Evaluator tests:**
   - Payload `{"cpu_percent": 90}` against `"cpu_percent > 85"` → true
   - Payload `{"cpu_percent": 70}` against `"cpu_percent > 85"` → false
   - Payload `{"severity": "Critical"}` against `"severity == Critical"` → true
   - Payload `{"tool_id": "http-client"}` against `"tool_id IN ['http-client', 'shell-exec']"` → true
   - Missing field → false

3. **Nested path test:** Payload `{"task": {"agent_id": "abc"}}` against `"task.agent_id == abc"` → true

4. **Integration test:** Create a subscription with filter, emit matching and non-matching events, verify only matching events trigger.

5. **Parse error test:** Invalid filter string → warning logged, event still delivered (fail-open).

---

## Verification

```bash
cargo build -p agentos-kernel
cargo test -p agentos-kernel

# Check filter module exists and has tests
grep -n "parse_filter" crates/agentos-kernel/src/event_bus.rs
grep -n "evaluate_filter" crates/agentos-kernel/src/event_bus.rs
grep -rn "#\[test\]" crates/agentos-kernel/src/event_bus.rs | grep -i filter
```

---

## Related

- [[Event Trigger Completion Plan]] — Master plan
- [[08-dynamic-subscriptions-and-role-defaults]] — Phase 08 builds on working filters
- [[agentos-event-trigger-system]] — Original spec §9 (Event Filters & Conditions)
