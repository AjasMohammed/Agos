use crate::kernel::Kernel;
use std::path::Path;

impl Kernel {
    pub(crate) const CORE_MANIFESTS: &[(&'static str, &'static str)] = &[
        (
            "file-reader.toml",
            include_str!("../../../tools/core/file-reader.toml"),
        ),
        (
            "file-writer.toml",
            include_str!("../../../tools/core/file-writer.toml"),
        ),
        (
            "memory-search.toml",
            include_str!("../../../tools/core/memory-search.toml"),
        ),
        (
            "memory-write.toml",
            include_str!("../../../tools/core/memory-write.toml"),
        ),
        (
            "data-parser.toml",
            include_str!("../../../tools/core/data-parser.toml"),
        ),
        (
            "agent-list.toml",
            include_str!("../../../tools/core/agent-list.toml"),
        ),
        (
            "agent-manual.toml",
            include_str!("../../../tools/core/agent-manual.toml"),
        ),
        (
            "agent-message.toml",
            include_str!("../../../tools/core/agent-message.toml"),
        ),
        (
            "agent-self.toml",
            include_str!("../../../tools/core/agent-self.toml"),
        ),
        (
            "archival-insert.toml",
            include_str!("../../../tools/core/archival-insert.toml"),
        ),
        (
            "archival-search.toml",
            include_str!("../../../tools/core/archival-search.toml"),
        ),
        ("audio.toml", include_str!("../../../tools/core/audio.toml")),
        (
            "bluetooth.toml",
            include_str!("../../../tools/core/bluetooth.toml"),
        ),
        (
            "datetime.toml",
            include_str!("../../../tools/core/datetime.toml"),
        ),
        (
            "display-config.toml",
            include_str!("../../../tools/core/display-config.toml"),
        ),
        (
            "episodic-list.toml",
            include_str!("../../../tools/core/episodic-list.toml"),
        ),
        (
            "escalation-status.toml",
            include_str!("../../../tools/core/escalation-status.toml"),
        ),
        (
            "file-delete.toml",
            include_str!("../../../tools/core/file-delete.toml"),
        ),
        (
            "file-diff.toml",
            include_str!("../../../tools/core/file-diff.toml"),
        ),
        (
            "file-editor.toml",
            include_str!("../../../tools/core/file-editor.toml"),
        ),
        (
            "file-glob.toml",
            include_str!("../../../tools/core/file-glob.toml"),
        ),
        (
            "file-grep.toml",
            include_str!("../../../tools/core/file-grep.toml"),
        ),
        (
            "file-move.toml",
            include_str!("../../../tools/core/file-move.toml"),
        ),
        (
            "hardware-info.toml",
            include_str!("../../../tools/core/hardware-info.toml"),
        ),
        (
            "http-client.toml",
            include_str!("../../../tools/core/http-client.toml"),
        ),
        (
            "log-reader.toml",
            include_str!("../../../tools/core/log-reader.toml"),
        ),
        (
            "memory-block-delete.toml",
            include_str!("../../../tools/core/memory-block-delete.toml"),
        ),
        (
            "memory-block-list.toml",
            include_str!("../../../tools/core/memory-block-list.toml"),
        ),
        (
            "memory-block-read.toml",
            include_str!("../../../tools/core/memory-block-read.toml"),
        ),
        (
            "memory-block-write.toml",
            include_str!("../../../tools/core/memory-block-write.toml"),
        ),
        (
            "memory-delete.toml",
            include_str!("../../../tools/core/memory-delete.toml"),
        ),
        (
            "memory-read.toml",
            include_str!("../../../tools/core/memory-read.toml"),
        ),
        (
            "memory-stats.toml",
            include_str!("../../../tools/core/memory-stats.toml"),
        ),
        (
            "network-monitor.toml",
            include_str!("../../../tools/core/network-monitor.toml"),
        ),
        (
            "printer.toml",
            include_str!("../../../tools/core/printer.toml"),
        ),
        (
            "raw-usb.toml",
            include_str!("../../../tools/core/raw-usb.toml"),
        ),
        (
            "procedure-create.toml",
            include_str!("../../../tools/core/procedure-create.toml"),
        ),
        (
            "procedure-delete.toml",
            include_str!("../../../tools/core/procedure-delete.toml"),
        ),
        (
            "procedure-list.toml",
            include_str!("../../../tools/core/procedure-list.toml"),
        ),
        (
            "procedure-search.toml",
            include_str!("../../../tools/core/procedure-search.toml"),
        ),
        (
            "process-manager.toml",
            include_str!("../../../tools/core/process-manager.toml"),
        ),
        (
            "shell-exec.toml",
            include_str!("../../../tools/core/shell-exec.toml"),
        ),
        (
            "spawn-agent.toml",
            include_str!("../../../tools/core/spawn-agent.toml"),
        ),
        (
            "await-agents.toml",
            include_str!("../../../tools/core/await-agents.toml"),
        ),
        (
            "task-delegate.toml",
            include_str!("../../../tools/core/task-delegate.toml"),
        ),
        (
            "task-list.toml",
            include_str!("../../../tools/core/task-list.toml"),
        ),
        (
            "task-status.toml",
            include_str!("../../../tools/core/task-status.toml"),
        ),
        ("think.toml", include_str!("../../../tools/core/think.toml")),
        (
            "usb-storage.toml",
            include_str!("../../../tools/core/usb-storage.toml"),
        ),
        (
            "webcam.toml",
            include_str!("../../../tools/core/webcam.toml"),
        ),
        (
            "web-fetch.toml",
            include_str!("../../../tools/core/web-fetch.toml"),
        ),
        (
            "skill-prompt.toml",
            include_str!("../../../tools/core/skill-prompt.toml"),
        ),
        // `skill-create` MUST be embedded: the ApprovalHook resolves a tool's
        // risk_class by looking up its manifest in the runtime tool_registry,
        // defaulting to ExecCapable when absent. Under approval mode `auto`,
        // ExecCapable auto-approves — which would let an agent author + install
        // skills with NO human review. Embedding the manifest guarantees the
        // top-level `risk_class = control_plane` reaches the registry on a
        // fresh data dir so every skill-create call is gated.
        (
            "skill-create.toml",
            include_str!("../../../tools/core/skill-create.toml"),
        ),
    ];

    /// Install bundled core tool manifests into the runtime directory if not already present.
    pub(crate) fn install_core_manifests(core_dir: &Path) -> Result<(), anyhow::Error> {
        for (filename, content) in Self::CORE_MANIFESTS {
            let dest = core_dir.join(filename);
            if !dest.exists()
                || std::fs::metadata(&dest)
                    .map(|m| m.len() == 0)
                    .unwrap_or(false)
            {
                std::fs::write(&dest, content)?;
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
}
