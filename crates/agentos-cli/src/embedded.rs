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

    Ok(())
}
