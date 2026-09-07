use dialoguer::{theme::ColorfulTheme, Confirm, FuzzySelect, Input};
use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;

/// Provider choice for the wizard.
pub(crate) struct ProviderOption {
    name: &'static str,
    display: &'static str,
    env_var: &'static str,
    default_model: &'static str,
}

const PROVIDERS: &[ProviderOption] = &[
    ProviderOption {
        name: "anthropic",
        display: "Anthropic (Claude)",
        env_var: "ANTHROPIC_API_KEY",
        default_model: "claude-opus-4-6",
    },
    ProviderOption {
        name: "openai",
        display: "OpenAI (GPT)",
        env_var: "OPENAI_API_KEY",
        default_model: "gpt-4o",
    },
    ProviderOption {
        name: "google",
        display: "Google (Gemini)",
        env_var: "GEMINI_API_KEY",
        default_model: "gemini-2.0-flash",
    },
    ProviderOption {
        name: "deepseek",
        display: "DeepSeek",
        env_var: "DEEPSEEK_API_KEY",
        default_model: "deepseek-chat",
    },
    ProviderOption {
        name: "groq",
        display: "Groq (fast Llama/Mixtral)",
        env_var: "GROQ_API_KEY",
        default_model: "llama-3.3-70b-versatile",
    },
    ProviderOption {
        name: "mistral",
        display: "Mistral AI",
        env_var: "MISTRAL_API_KEY",
        default_model: "mistral-large-latest",
    },
    ProviderOption {
        name: "xai",
        display: "xAI (Grok)",
        env_var: "XAI_API_KEY",
        default_model: "grok-3",
    },
    ProviderOption {
        name: "ollama",
        display: "Ollama (local, no API key needed)",
        env_var: "",
        default_model: "llama3.2",
    },
];

pub async fn handle() -> anyhow::Result<()> {
    let theme = ColorfulTheme::default();

    println!();
    println!("  ╔════════════════════════════════════╗");
    println!("  ║       AgentOS Setup Wizard         ║");
    println!("  ╚════════════════════════════════════╝");
    println!();
    println!("  This wizard configures AgentOS. API keys are NEVER written to disk.");
    println!("  You will be told which environment variables to set.\n");

    // Step 1: Select primary LLM provider
    let provider_names: Vec<&str> = PROVIDERS.iter().map(|p| p.display).collect();
    let provider_idx = FuzzySelect::with_theme(&theme)
        .with_prompt("Select your primary LLM provider")
        .items(&provider_names)
        .default(0)
        .interact()?;
    let provider = &PROVIDERS[provider_idx];

    // Step 2: Default model
    let model: String = Input::with_theme(&theme)
        .with_prompt("Default model")
        .default(provider.default_model.to_string())
        .interact_text()?;

    // Step 3: Optional fallback provider
    let add_fallback = Confirm::with_theme(&theme)
        .with_prompt("Configure a fallback provider? (used if primary is unavailable)")
        .default(false)
        .interact()?;

    let fallback_provider = if add_fallback {
        let fallback_idx = FuzzySelect::with_theme(&theme)
            .with_prompt("Select fallback provider")
            .items(&provider_names)
            .default(1)
            .interact()?;
        Some(&PROVIDERS[fallback_idx])
    } else {
        None
    };

    // Step 4: Agent name
    let agent_name: String = Input::with_theme(&theme)
        .with_prompt("Default agent name")
        .default("assistant".to_string())
        .interact_text()?;

    // Step 5: Data directory
    let data_dir: String = Input::with_theme(&theme)
        .with_prompt("Data directory (vault, audit, memory databases)")
        .default("data".to_string())
        .interact_text()?;

    // Step 6: Write config
    println!();
    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.green} {msg}")
            .unwrap(),
    );
    spinner.enable_steady_tick(Duration::from_millis(80));
    spinner.set_message("Writing configuration...");

    write_config(provider, &model, fallback_provider, &agent_name, &data_dir)?;

    spinner.finish_with_message("Configuration written.");

    // Step 7: Create data directories
    let spinner2 = ProgressBar::new_spinner();
    spinner2.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.green} {msg}")
            .unwrap(),
    );
    spinner2.enable_steady_tick(Duration::from_millis(80));
    spinner2.set_message("Creating data directories...");
    std::fs::create_dir_all(&data_dir)?;
    spinner2.finish_with_message(format!("Data directory '{}' ready.", data_dir));

    // Summary and environment variable instructions
    println!();
    println!("  Setup complete!\n");
    println!("  Provider : {} (model: {})", provider.display, model);
    if let Some(fb) = fallback_provider {
        println!("  Fallback : {}", fb.display);
    }
    println!("  Agent    : {}", agent_name);
    println!("  Data dir : {}", data_dir);

    println!("\n  Action required — set these environment variables:");
    if !provider.env_var.is_empty() {
        println!("    export {}=\"your-api-key\"", provider.env_var);
    }
    if let Some(fb) = fallback_provider {
        if !fb.env_var.is_empty() {
            println!("    export {}=\"your-api-key\"", fb.env_var);
        }
    }
    if provider.env_var.is_empty()
        && fallback_provider
            .map(|f| f.env_var.is_empty())
            .unwrap_or(true)
    {
        println!("    (no API keys required for local providers)");
    }

    // Vault passphrase — the kernel fail-closes if the vault DB exists but no
    // passphrase is available. Surface this here so first-time users don't hit
    // a confusing error on first `agentos start`.  Empty strings count as unset
    // (a common defensive `export VAR=` doesn't actually configure anything).
    let passphrase_unset = std::env::var("AGENTOS_VAULT_PASSPHRASE")
        .map(|v| v.trim().is_empty())
        .unwrap_or(true);
    if passphrase_unset {
        let managed_pp = std::path::Path::new(&data_dir).join("vault/secrets.passphrase");
        if !managed_pp.exists() {
            println!();
            println!("  Vault passphrase — required before the first `agentos start`:");
            println!("    Recommended (systemd / Docker / shared hosts):");
            println!(
                "      printf '%s' 'choose-a-strong-passphrase' > {}",
                managed_pp.display()
            );
            println!("      chmod 600 {}", managed_pp.display());
            println!("      (kernel reads the file at boot; see deploy/agentos.service");
            println!("       for the systemd LoadCredential= pattern.)");
            println!("    Or, for one-off testing only (leaks via shell history):");
            println!("      export AGENTOS_VAULT_PASSPHRASE='choose-a-strong-passphrase'");
        }
    }

    println!();
    println!("  Next steps:");
    println!("    agentos doctor        — verify configuration");
    println!("    agentos init          — start the kernel");
    println!("    agentos agent connect — register your agent");
    println!();

    Ok(())
}

pub(crate) fn write_config(
    provider: &ProviderOption,
    model: &str,
    fallback: Option<&ProviderOption>,
    agent_name: &str,
    data_dir: &str,
) -> anyhow::Result<()> {
    write_config_to(
        provider,
        model,
        fallback,
        agent_name,
        data_dir,
        std::path::Path::new("config/default.toml"),
    )
}

/// Write config to an explicit path (used by tests to avoid CWD races).
pub(crate) fn write_config_to(
    provider: &ProviderOption,
    model: &str,
    fallback: Option<&ProviderOption>,
    agent_name: &str,
    data_dir: &str,
    config_path: &std::path::Path,
) -> anyhow::Result<()> {
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // If an existing config exists, patch it; otherwise start fresh.
    // Error out if the existing file has a syntax error to avoid silent data loss.
    let existing = std::fs::read_to_string(config_path).unwrap_or_default();
    let mut doc: toml_edit::DocumentMut = if existing.trim().is_empty() {
        toml_edit::DocumentMut::new()
    } else {
        existing.parse().map_err(|e| {
            anyhow::anyhow!(
                "Existing config at {} has a syntax error: {}. \
                 Run `agentos doctor` first.",
                config_path.display(),
                e
            )
        })?
    };

    // [llm]
    if doc.get("llm").is_none() {
        doc["llm"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    let llm = doc["llm"].as_table_mut().unwrap();
    llm["primary"] = toml_edit::value(format!("{}/{}", provider.name, model));

    if let Some(fb) = fallback {
        llm["fallbacks"] = toml_edit::Item::Value(toml_edit::Value::Array({
            let mut arr = toml_edit::Array::new();
            arr.push(format!("{}/{}", fb.name, fb.default_model));
            arr
        }));
    } else {
        llm["fallbacks"] = toml_edit::Item::Value(toml_edit::Value::Array(toml_edit::Array::new()));
    }

    // Store the env var name (never the key itself).
    if !provider.env_var.is_empty() {
        llm["api_key_env"] = toml_edit::value(provider.env_var);
    }

    // [vault]
    if doc.get("vault").is_none() {
        doc["vault"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    doc["vault"]["db_path"] = toml_edit::value(format!("{}/vault.db", data_dir));

    // [audit]
    if doc.get("audit").is_none() {
        doc["audit"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    doc["audit"]["db_path"] = toml_edit::value(format!("{}/audit.db", data_dir));

    // [kernel]
    if doc.get("kernel").is_none() {
        doc["kernel"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    doc["kernel"]["default_agent"] = toml_edit::value(agent_name);

    std::fs::write(config_path, doc.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_write_config_creates_file() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config").join("default.toml");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();

        let provider = &PROVIDERS[0]; // anthropic
        write_config_to(
            provider,
            "claude-opus-4-6",
            None,
            "assistant",
            "data",
            &config_path,
        )
        .unwrap();

        let content = std::fs::read_to_string(&config_path).unwrap();
        assert!(content.contains("anthropic/claude-opus-4-6"));
        assert!(content.contains("ANTHROPIC_API_KEY"));
        assert!(
            !content.contains("your-api-key"),
            "raw key must never be written"
        );
    }

    #[test]
    fn test_write_config_parse_error_returns_err() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("config")).unwrap();
        let config_path = tmp.path().join("config").join("default.toml");
        std::fs::write(&config_path, "this = [invalid toml").unwrap();

        let _original_dir = std::env::current_dir().unwrap();
        let provider = &PROVIDERS[0];
        let result = write_config_to(
            provider,
            "claude-opus-4-6",
            None,
            "assistant",
            "data",
            &config_path,
        );
        assert!(result.is_err(), "should fail on malformed existing config");
    }
}
