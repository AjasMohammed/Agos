use rust_embed::Embed;
use std::path::Path;

#[derive(Embed)]
#[folder = "../../config/"]
#[prefix = "config/"]
struct ConfigAssets;

#[derive(Embed)]
#[folder = "../../skills/core/"]
#[prefix = "skills/core/"]
struct SkillAssets;

#[derive(Embed)]
#[folder = "../../plugins/core/"]
#[prefix = "plugins/core/"]
struct PluginAssets;

#[derive(Embed)]
#[folder = "../../pipelines/core/"]
#[prefix = "pipelines/core/"]
struct PipelineAssets;

/// Extract embedded assets to a data directory if they don't already exist.
/// This is called on first run to seed the working directory.
pub fn extract_assets_if_needed(data_dir: &Path) -> std::io::Result<()> {
    let config_dir = data_dir.join("config");
    if !config_dir.exists() {
        std::fs::create_dir_all(&config_dir)?;
        for file in ConfigAssets::iter() {
            if let Some(content) = ConfigAssets::get(&file) {
                let path = data_dir.join(file.as_ref());
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&path, content.data.as_ref())?;
            }
        }
    }

    let skills_dir = data_dir.join("skills/core");
    if !skills_dir.exists() {
        std::fs::create_dir_all(&skills_dir)?;
        for file in SkillAssets::iter() {
            if let Some(content) = SkillAssets::get(&file) {
                let path = data_dir.join(file.as_ref());
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&path, content.data.as_ref())?;
            }
        }
    }

    // Core plugin manifests — the kernel discovers `<data_dir>/plugins/{core,user}`,
    // so without this seed a real install shows an empty Plugins page.
    let plugins_dir = data_dir.join("plugins/core");
    if !plugins_dir.exists() {
        std::fs::create_dir_all(&plugins_dir)?;
        for file in PluginAssets::iter() {
            if let Some(content) = PluginAssets::get(&file) {
                let path = data_dir.join(file.as_ref());
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&path, content.data.as_ref())?;
            }
        }
    }

    // Starter pipeline templates. Nothing loads these automatically — they are
    // on disk so `agentos pipeline install <data_dir>/pipelines/core/<f>.yaml`
    // works on a binary install with no repo checkout.
    //
    // Checked per FILE, not per directory, unlike the blocks above: a template
    // added in a later release must still reach an install that already has the
    // directory, and a write that fails half way (ENOSPC) must be retried on the
    // next boot rather than leaving the install permanently half-seeded.
    // Existing files are never rewritten — the README teaches copy-then-edit,
    // but an operator who edited one in place keeps their edit.
    for file in PipelineAssets::iter() {
        let path = data_dir.join(file.as_ref());
        if path.exists() {
            continue;
        }
        if let Some(content) = PipelineAssets::get(&file) {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, content.data.as_ref())?;
        }
    }

    Ok(())
}
