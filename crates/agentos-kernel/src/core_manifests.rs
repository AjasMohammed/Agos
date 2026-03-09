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
