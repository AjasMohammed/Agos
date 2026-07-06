use crate::kernel::Kernel;
use std::path::Path;

/// Every `tools/core/*.toml` manifest embedded into the binary at build time.
///
/// Embedding the **whole directory** (rather than a hand-maintained include
/// list) closes a class of deployment bugs: a manifest authored under
/// `tools/core/` but forgotten from the embed list never reached a fresh
/// shipped-binary data dir, so its `risk_class` could not be resolved and the
/// ApprovalHook fell back to `ExecCapable` (auto-approve under `approval =
/// auto`) — silently removing the human-review gate on `control_plane` tools.
/// With directory embedding, every shipped manifest is always seeded.
#[derive(rust_embed::RustEmbed)]
#[folder = "../../tools/core/"]
struct EmbeddedCoreManifests;

impl Kernel {
    /// Install bundled core tool manifests into the runtime directory if not
    /// already present. Seeds **every** embedded `tools/core/*.toml`, so no
    /// shipped tool's manifest (and thus `risk_class`) can be missing on a
    /// fresh data dir.
    pub(crate) fn install_core_manifests(core_dir: &Path) -> Result<(), anyhow::Error> {
        for filename in EmbeddedCoreManifests::iter() {
            // rust-embed yields forward-slash paths; flatten to the basename so
            // we never write outside `core_dir`.
            let base = filename.rsplit('/').next().unwrap_or(&filename);
            let dest = core_dir.join(base);
            let needs_write = !dest.exists()
                || std::fs::metadata(&dest)
                    .map(|m| m.len() == 0)
                    .unwrap_or(false);
            if needs_write {
                let asset = EmbeddedCoreManifests::get(&filename).ok_or_else(|| {
                    anyhow::anyhow!("embedded manifest '{filename}' vanished at runtime")
                })?;
                std::fs::write(&dest, asset.data.as_ref())?;
            }
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
    /// only the embedded subset; if `skill-create.toml` is missing, the
    /// ApprovalHook can't find its `risk_class` and defaults to `ExecCapable`
    /// — which auto-approves under approval mode `auto`, silently removing the
    /// human-review gate on runtime skill authoring.
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
