use crate::kernel::Kernel;
use std::path::Path;

/// Every `tools/core/*.toml` manifest embedded into the binary at build time.
///
/// Embedding the **whole directory** (rather than a hand-maintained include
/// list) closes a class of deployment bugs: a manifest authored under
/// `tools/core/` but forgotten from the embed list never reached a fresh
/// shipped-binary data dir, so its `risk_class` could not be resolved and the
/// tool was gated by the `RiskClass` default instead of its authored class —
/// losing the distinction between `control_plane` and everything else.
/// With directory embedding, every shipped manifest is always seeded.
#[derive(rust_embed::RustEmbed)]
#[folder = "../../tools/core/"]
pub(crate) struct EmbeddedCoreManifests;

impl Kernel {
    /// Install bundled core tool manifests into the runtime directory, seeding
    /// **every** embedded `tools/core/*.toml` so no shipped tool's manifest
    /// (and thus `risk_class`) can be missing on a fresh data dir.
    ///
    /// Not "if not already present" — an on-disk copy whose bytes differ from
    /// the embedded one is overwritten, so a manifest fix reaches existing
    /// installs on the next boot and not only fresh data dirs.
    pub(crate) fn install_core_manifests(core_dir: &Path) -> Result<(), anyhow::Error> {
        let mut updated = 0usize;
        for filename in EmbeddedCoreManifests::iter() {
            // rust-embed yields forward-slash paths; flatten to the basename so
            // we never write outside `core_dir`.
            let base = filename.rsplit('/').next().unwrap_or(&filename);
            let dest = core_dir.join(base);
            let asset = EmbeddedCoreManifests::get(&filename).ok_or_else(|| {
                anyhow::anyhow!("embedded manifest '{filename}' vanished at runtime")
            })?;
            // Core manifests are shipped artifacts: overwrite whenever the
            // embedded bytes differ so manifest fixes (e.g. a corrected
            // `risk_class`) reach existing installs on the next boot instead
            // of only fresh data dirs.
            let on_disk = std::fs::read(&dest).ok();
            let needs_write = on_disk.as_deref() != Some(asset.data.as_ref());
            if needs_write {
                if on_disk.is_some() {
                    updated += 1;
                    tracing::info!(manifest = %base, "Refreshing stale core tool manifest");
                }
                // Never fatal: a read-only / root-owned `core_tools_dir`
                // (packaged install, container layer) must still boot with the
                // manifests already on disk.
                if let Err(e) = std::fs::write(&dest, asset.data.as_ref()) {
                    tracing::warn!(
                        manifest = %base,
                        error = %e,
                        "Could not refresh core tool manifest; keeping the on-disk copy"
                    );
                }
            }
        }
        if updated > 0 {
            tracing::info!(
                updated,
                "Core tool manifests refreshed from embedded copies"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::RiskClass;

    /// Regression guard for the deployment gap where a control-plane tool's
    /// manifest is authored under `tools/core/` but never embedded in
    /// `CORE_MANIFESTS`. On a fresh data dir, `install_core_manifests` seeds
    /// only the embedded subset; if `skill-create.toml` is missing, its
    /// `risk_class` is never registered and the tool loses its `ControlPlane`
    /// classification — dropping the always-escalate gate that `control_plane`
    /// carries over the `ExecCapable` default.
    ///
    /// This boots the install into an EMPTY dir (the shipped-binary path) and
    /// asserts the manifest lands with `risk_class = ControlPlane`.
    #[test]
    fn install_core_manifests_seeds_skill_create_with_control_plane() {
        let tmp = tempfile::TempDir::new().unwrap();
        Kernel::install_core_manifests(tmp.path()).expect("install must succeed");

        let path = tmp.path().join("skill-create.toml");
        assert!(
            path.exists(),
            "skill-create.toml must be embedded in CORE_MANIFESTS so it reaches \
             a fresh data dir — otherwise the control-plane approval gate is bypassed"
        );

        let loaded =
            agentos_tools::loader::load_manifest(&path).expect("manifest must parse and verify");
        assert_eq!(loaded.manifest.manifest.name, "skill-create");
        assert_eq!(
            loaded.manifest.risk_class,
            RiskClass::ControlPlane,
            "skill-create must register as ControlPlane so every skill-authoring \
             call is gated by the approval hook"
        );
    }

    /// SEC-02: EVERY `tools/core/*.toml` must declare `risk_class` EXPLICITLY.
    ///
    /// `ToolManifest.risk_class` is `#[serde(default)]`, so an omitted key
    /// deserializes silently — the parsed struct cannot tell "absent" from
    /// "authored as the default value". The default now fails closed
    /// (`ExecCapable`), but relying on it means a tool's approval gate is
    /// decided by a Rust default rather than by a human reading the manifest.
    /// Check the raw TOML source, which is the only place absence is visible.
    #[test]
    fn every_core_manifest_declares_risk_class_explicitly() {
        let mut checked = 0usize;
        let mut missing = Vec::new();
        for name in EmbeddedCoreManifests::iter() {
            let asset = EmbeddedCoreManifests::get(&name).unwrap();
            let text = std::str::from_utf8(asset.data.as_ref()).unwrap_or("");
            checked += 1;
            // Top-level key only — a `[risk_class]` table is a different (and
            // previously-shipped) bug, so match the assignment form.
            if !text
                .lines()
                .any(|l| l.trim_start().starts_with("risk_class"))
            {
                missing.push(name.to_string());
            }
        }

        assert!(
            missing.is_empty(),
            "core manifests missing an explicit `risk_class` — they would inherit \
             the RiskClass default instead of an authored classification: {missing:?}"
        );
        assert!(
            checked >= 100,
            "expected to scan every core manifest, scanned only {checked} — \
             did the embed folder path break?"
        );
    }

    /// Every `risk_class_by_action` entry a shipped manifest declares must name
    /// `readonly_external` AND an action the tool's own `payload_schema` accepts.
    ///
    /// The class bound is enforced at load by `verify_manifest`, so a violation
    /// there is a boot failure, not a silent downgrade. The *key* bound is not
    /// enforced anywhere and cannot be: an unknown action simply never matches,
    /// so `list_adapter` for `list_adapters` fails open into "still prompts
    /// forever" — the exact annoyance the table exists to remove, and invisible
    /// because nothing errors. This test is that check.
    #[test]
    fn per_action_risk_overrides_are_readonly_and_name_real_actions() {
        let mut with_overrides = Vec::new();
        for name in EmbeddedCoreManifests::iter() {
            let asset = EmbeddedCoreManifests::get(&name).unwrap();
            let text = std::str::from_utf8(asset.data.as_ref()).unwrap_or("");
            let Ok(manifest) = agentos_tools::parse_manifest(text) else {
                continue;
            };
            if manifest.risk_class_by_action.is_empty() {
                continue;
            }
            let tool = manifest.manifest.name.clone();
            with_overrides.push(tool.clone());

            let declared: Vec<&str> = manifest
                .payload_schema
                .as_ref()
                .and_then(|s| s.pointer("/properties/action/enum"))
                .and_then(|e| e.as_array())
                .map(|e| e.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            assert!(
                !declared.is_empty(),
                "{tool}: declares risk_class_by_action but its payload_schema has                  no action enum — the keys can never match a real call"
            );

            for (action, class) in &manifest.risk_class_by_action {
                assert!(
                    matches!(class, agentos_types::RiskClass::ReadonlyExternal),
                    "{tool}: risk_class_by_action[{action}] = {class:?}; only \
                     readonly_external is permitted (readonly_scoped is Allow \
                     under `deny` too, so it is a bypass, not less friction)"
                );
                assert!(
                    declared.contains(&action.as_str()),
                    "{tool}: risk_class_by_action names action '{action}', which is                      not in its payload_schema enum {declared:?} — it would never                      match, and the action would keep prompting silently"
                );
            }
        }

        // Regression guard: these six were downgraded because their read-only
        // actions were prompting exactly like their state-changing ones (one
        // observed session burned 13 approvals in 15 minutes). Dropping a table
        // in a manifest rewrite restores that with no other signal.
        for expected in [
            "wifi",
            "bluetooth",
            "audio",
            "webcam",
            "raw-usb",
            "display-config",
        ] {
            assert!(
                with_overrides.iter().any(|t| t == expected),
                "{expected} lost its risk_class_by_action table; its read-only                  actions now prompt like its writes. Present: {with_overrides:?}"
            );
        }
    }

    /// Generalized guard (W1): EVERY `tools/core/*.toml` that declares a
    /// privileged `risk_class` (control_plane / exec_capable) must reach a
    /// fresh data dir via `install_core_manifests`. Otherwise its risk class
    /// can't be resolved at runtime and the ApprovalHook fails open to the
    /// ExecCapable default — which auto-approves under `approval = auto`.
    /// This is the workspace-wide version of the skill-create regression.
    #[test]
    fn install_seeds_every_privileged_manifest() {
        let tmp = tempfile::TempDir::new().unwrap();
        Kernel::install_core_manifests(tmp.path()).expect("install must succeed");

        let mut checked = 0usize;
        let mut missing = Vec::new();
        for name in EmbeddedCoreManifests::iter() {
            let base = name.rsplit('/').next().unwrap_or(&name).to_string();
            let asset = EmbeddedCoreManifests::get(&name).unwrap();
            let text = std::str::from_utf8(asset.data.as_ref()).unwrap_or("");
            // Cheap source check — avoids a full manifest parse for fixtures
            // that may require signatures. We only care that privileged
            // manifests are physically seeded.
            let privileged = text.contains("risk_class = \"control_plane\"")
                || text.contains("risk_class = \"exec_capable\"");
            if !privileged {
                continue;
            }
            checked += 1;
            if !tmp.path().join(&base).exists() {
                missing.push(base);
            }
        }

        assert!(
            missing.is_empty(),
            "privileged manifests not seeded to a fresh data dir: {missing:?}"
        );
        assert!(
            checked >= 10,
            "expected to find many privileged manifests, found {checked} — \
             did the embed folder path break?"
        );
    }
}
