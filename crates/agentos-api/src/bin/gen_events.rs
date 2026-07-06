//! Emit the WS frame tags + subscribable event catalog to a JSON file — the
//! generated source for the panel's `events.gen.ts` mirror.
//!
//! Usage: `cargo run -p agentos-api --bin gen-events -- [output-path]`
//! Default output: `crates/agentos-api/events.json`.
//!
//! Sources (single points of truth):
//! - Frame tags: `CLIENT_FRAME_TAGS` / `SERVER_FRAME_TAGS` in `ws/protocol.rs`,
//!   pinned to the enums by exhaustive-match tests.
//! - Event catalog: `SUBSCRIBABLE_EVENTS` in the kernel's `event_bus.rs` (the
//!   same table `parse_event_type` reads), grouped by `EventType::category()`
//!   with the resource string from `permission_for_category`.

use agentos_api::ws::protocol::{CLIENT_FRAME_TAGS, SERVER_FRAME_TAGS};
use agentos_kernel::event_bus::SUBSCRIBABLE_EVENTS;
use agentos_kernel::event_permissions::permission_for_category;
use agentos_types::EventCategory;

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "crates/agentos-api/events.json".to_string());

    // Group by category, preserving first-appearance order from the table.
    let mut cats: Vec<(EventCategory, Vec<&str>)> = Vec::new();
    for (name, ty) in SUBSCRIBABLE_EVENTS {
        let category = ty.category();
        match cats.iter_mut().find(|(c, _)| *c == category) {
            Some((_, events)) => events.push(name),
            None => cats.push((category, vec![name])),
        }
    }

    let catalog: Vec<serde_json::Value> = cats
        .iter()
        .map(|(category, events)| {
            serde_json::json!({
                "value": format!("{category:?}"),
                "resource": permission_for_category(*category),
                "events": events,
            })
        })
        .collect();

    let doc = serde_json::json!({
        "clientFrames": CLIENT_FRAME_TAGS,
        "serverFrames": SERVER_FRAME_TAGS,
        "catalog": catalog,
    });

    let json = serde_json::to_string_pretty(&doc).expect("serialize events document");
    std::fs::write(&out, format!("{json}\n")).unwrap_or_else(|e| {
        eprintln!("failed to write {out}: {e}");
        std::process::exit(1);
    });

    eprintln!("wrote events catalog to {out}");
}
