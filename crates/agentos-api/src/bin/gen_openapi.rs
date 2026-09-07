//! Emit the OpenAPI 3.1 document to a JSON file (the committed API contract).
//!
//! Usage: `cargo run -p agentos-api --bin gen-openapi -- [output-path]`
//! Default output: `crates/agentos-api/openapi.json`.
//!
//! CI runs this and fails if the result differs from the committed file, so the
//! spec can never silently drift from the handlers.

use agentos_api::openapi::ApiDoc;
use utoipa::OpenApi;

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "crates/agentos-api/openapi.json".to_string());

    let json = ApiDoc::openapi()
        .to_pretty_json()
        .expect("serialize OpenAPI document");

    std::fs::write(&out, format!("{json}\n")).unwrap_or_else(|e| {
        eprintln!("failed to write {out}: {e}");
        std::process::exit(1);
    });

    eprintln!("wrote OpenAPI spec to {out}");
}
