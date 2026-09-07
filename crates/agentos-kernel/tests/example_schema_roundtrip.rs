/// Phase-5: parametrised test that every core manifest's examples validate
/// against its declared payload_schema via the real SchemaRegistry.
///
/// This test MUST live in `agentos-kernel` because `jsonschema` is a kernel-only
/// dependency (agentos-tools has no jsonschema dep and cannot depend on the
/// kernel — that would be a cycle). Running through the real SchemaRegistry
/// means a bad example fails exactly as it would at kernel boot.
use agentos_kernel::{schema_registry::SchemaRegistry, tool_registry::ToolRegistry};
use std::path::Path;

fn load_core_registry() -> ToolRegistry {
    let core = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/core");
    let user = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/__none__");
    ToolRegistry::load_from_dirs(&core, &user).expect("core manifests must load")
}

#[test]
fn all_core_examples_validate_against_schema() {
    let registry = load_core_registry();
    let mut failed: Vec<String> = Vec::new();

    for tool in registry.list_all() {
        if tool.manifest.examples.is_empty() {
            continue;
        }
        let name = &tool.manifest.manifest.name;

        // Run examples through the real SchemaRegistry validator so validation
        // is identical to boot-time.
        let mut reg = SchemaRegistry::new();
        if let Some(ref schema) = tool.manifest.payload_schema {
            if let Err(e) = reg.register(name, schema.clone()) {
                failed.push(format!("{name}: schema registration failed: {e}"));
                continue;
            }
        }
        for (i, ex) in tool.manifest.examples.iter().enumerate() {
            if let Some(ref _schema) = tool.manifest.payload_schema {
                if let Err(e) = reg.validate(name, &ex.payload) {
                    failed.push(format!(
                        "{name} example[{i}]: validation failed: {e}\n  payload: {}",
                        ex.payload
                    ));
                }
            }
        }
    }

    if !failed.is_empty() {
        panic!(
            "Example/schema roundtrip failures ({}):\n{}",
            failed.len(),
            failed.join("\n")
        );
    }
}

#[test]
fn example_bearing_manifests_declare_schema() {
    // Closes the `if let Some(schema) = ... { register_with_tier(...) }` hole:
    // examples are only validated at boot when payload_schema is Some.
    let registry = load_core_registry();
    let bad: Vec<String> = registry
        .list_all()
        .into_iter()
        .filter(|t| !t.manifest.examples.is_empty() && t.manifest.payload_schema.is_none())
        .map(|t| t.manifest.manifest.name.clone())
        .collect();

    if !bad.is_empty() {
        panic!(
            "Manifests with examples but no payload_schema (examples bypass validation at boot): {:?}",
            bad
        );
    }
}

#[test]
fn band1_tools_have_examples() {
    let registry = load_core_registry();
    let band1 = [
        "file-reader",
        "file-writer",
        "file-glob",
        "file-grep",
        "file-editor",
        "shell-exec",
        "memory-write",
        "memory-search",
        "context-memory-read",
        "web-search",
        "web-fetch",
        "spawn-agent",
        "schedule-once",
        "channel-send",
    ];
    let missing: Vec<&str> = band1
        .iter()
        .filter(|name| {
            registry
                .get_by_name(name)
                .map(|t| t.manifest.examples.is_empty())
                .unwrap_or(true)
        })
        .copied()
        .collect();

    if !missing.is_empty() {
        panic!(
            "Band-1 tools missing examples (high-priority for first-call accuracy): {:?}",
            missing
        );
    }
}
