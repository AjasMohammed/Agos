use agentos_bus::client::BusClient;
use agentos_bus::message::{KernelCommand, KernelResponse};
use clap::Subcommand;
use serde_json::{json, Value};
use std::collections::HashMap;

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
pub enum ProviderCommands {
    /// List all available LLM providers (built-in + catalog)
    List,
    /// Override the base URL for a catalog provider (persisted to providers.toml)
    SetUrl {
        /// Provider name (e.g. lmstudio, groq)
        name: String,
        /// New base URL (e.g. http://localhost:5678/v1)
        url: String,
    },
    /// Add or replace a provider in the catalog. Connects any HTTP LLM with
    /// configurable auth scheme, endpoint paths, and capabilities.
    Add {
        /// Catalog name (lowercase, alphanumeric + `-_.`)
        #[arg(long)]
        name: String,
        /// Human-readable display name (defaults to `name`)
        #[arg(long)]
        display_name: Option<String>,
        /// Base URL, e.g. `https://api.example.com/v1`
        #[arg(long)]
        base_url: String,
        /// Environment variable holding the API key. Empty = no auth.
        #[arg(long, default_value = "")]
        api_key_env: String,
        /// Wire format: openai, anthropic, gemini, ollama
        #[arg(long, default_value = "openai")]
        compatible_with: String,
        /// Default model id (used when agent connects without `--model`)
        #[arg(long)]
        default_model: String,
        /// Available model ids (comma separated)
        #[arg(long, value_delimiter = ',')]
        models: Vec<String>,
        /// Vision-capable model ids (comma separated)
        #[arg(long, value_delimiter = ',')]
        vision_models: Vec<String>,
        /// Override context window in tokens
        #[arg(long)]
        context_window: Option<u64>,
        /// Override max output tokens
        #[arg(long)]
        max_output_tokens: Option<u64>,
        /// Override `supports_images` (true|false). Unset = use adapter default.
        #[arg(long, value_name = "BOOL")]
        supports_images: Option<bool>,
        /// Override `supports_tool_calling`. Unset = adapter default (true).
        #[arg(long, value_name = "BOOL")]
        supports_tool_calling: Option<bool>,
        /// Override `supports_streaming`. Unset = adapter default (true).
        #[arg(long, value_name = "BOOL")]
        supports_streaming: Option<bool>,
        /// Override `supports_prompt_caching`. Unset = adapter default (false).
        #[arg(long, value_name = "BOOL")]
        supports_prompt_caching: Option<bool>,
        /// Permit private/loopback/link-local `base_url`. Required for
        /// localhost providers like lmstudio, ollama, vllm.
        #[arg(long)]
        allow_private_hosts: bool,
        /// Auth header name (default `Authorization`). Use `api-key` for Azure.
        #[arg(long)]
        auth_header: Option<String>,
        /// Auth value prefix (default `"Bearer "`). Use `""` for raw key.
        #[arg(long)]
        auth_prefix: Option<String>,
        /// Chat completions path (default `/chat/completions`)
        #[arg(long)]
        chat_path: Option<String>,
        /// Models list path (default `/models`)
        #[arg(long)]
        models_path: Option<String>,
        /// Extra static headers, repeatable: --header "X-Foo=bar"
        #[arg(long = "header", value_parser = parse_header_kv)]
        extra_headers: Vec<(String, String)>,
    },
    /// Remove a provider from the catalog
    Remove {
        /// Provider name
        name: String,
    },
    /// Probe `<base_url><models_path>` and refresh the `models` list
    Probe {
        /// Provider name
        name: String,
    },
}

fn parse_header_kv(s: &str) -> Result<(String, String), String> {
    let (k, v) = s
        .split_once('=')
        .ok_or_else(|| format!("Header must be 'name=value', got '{s}'"))?;
    let k = k.trim();
    if k.is_empty() {
        return Err("Header name cannot be empty".to_string());
    }
    // Reject CR/LF/NUL/control bytes early — kernel-side validation will also
    // catch these, but failing at parse-time gives a clearer error.
    for piece in [k, v] {
        for b in piece.as_bytes() {
            if matches!(b, b'\r' | b'\n' | 0) || (*b < 0x20 && *b != b'\t') {
                return Err(format!("Header '{k}' contains a control byte; refusing"));
            }
        }
    }
    // Trim key only; preserve value verbatim (RFC 7230 OWS is allowed).
    Ok((k.to_string(), v.to_string()))
}

pub async fn handle(client: &mut BusClient, command: ProviderCommands) -> anyhow::Result<()> {
    match command {
        ProviderCommands::SetUrl { name, url } => {
            let response = client
                .send_command(KernelCommand::SetProviderUrl {
                    name: name.clone(),
                    url: url.clone(),
                })
                .await?;
            match response {
                KernelResponse::Success { .. } => {
                    println!("Provider '{}' base URL updated to '{}'", name, url);
                }
                KernelResponse::Error { message } => eprintln!("Error: {}", message),
                _ => eprintln!("Unexpected response"),
            }
        }
        ProviderCommands::List => {
            let response = client.send_command(KernelCommand::ListProviders).await?;
            match response {
                KernelResponse::ProviderList(providers) => {
                    if providers.is_empty() {
                        println!("No providers available.");
                        return Ok(());
                    }
                    println!(
                        "{:<15} {:<20} {:<10} {:<30} {:<5}",
                        "NAME", "DISPLAY NAME", "SOURCE", "DEFAULT MODEL", "KEY"
                    );
                    println!("{}", "-".repeat(80));
                    for p in &providers {
                        let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("-");
                        let display_name = p
                            .get("display_name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("-");
                        let source = p.get("source").and_then(|v| v.as_str()).unwrap_or("-");
                        let default_model = p
                            .get("default_model")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let api_key_env =
                            p.get("api_key_env").and_then(|v| v.as_str()).unwrap_or("");
                        let key_set = p
                            .get("api_key_set")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);

                        let key_indicator = if api_key_env.is_empty() {
                            "-".to_string() // No key needed
                        } else if key_set {
                            "yes".to_string()
                        } else {
                            "no".to_string()
                        };

                        println!(
                            "{:<15} {:<20} {:<10} {:<30} {:<5}",
                            name, display_name, source, default_model, key_indicator
                        );
                    }
                }
                KernelResponse::Error { message } => eprintln!("Error: {}", message),
                _ => eprintln!("Unexpected response"),
            }
        }
        ProviderCommands::Add {
            name,
            display_name,
            base_url,
            api_key_env,
            compatible_with,
            default_model,
            models,
            vision_models,
            context_window,
            max_output_tokens,
            supports_images,
            supports_tool_calling,
            supports_streaming,
            supports_prompt_caching,
            allow_private_hosts,
            auth_header,
            auth_prefix,
            chat_path,
            models_path,
            extra_headers,
        } => {
            let mut entry = json!({
                "name": name,
                "display_name": display_name.clone().unwrap_or_else(|| name.clone()),
                "base_url": base_url,
                "api_key_env": api_key_env,
                "compatible_with": compatible_with,
                "default_model": default_model,
                "models": models,
                "vision_models": vision_models,
            });
            let obj = entry.as_object_mut().unwrap();
            if let Some(v) = context_window {
                obj.insert("context_window".into(), Value::from(v));
            }
            if let Some(v) = max_output_tokens {
                obj.insert("max_output_tokens".into(), Value::from(v));
            }
            if let Some(v) = supports_images {
                obj.insert("supports_images".into(), Value::Bool(v));
            }
            if let Some(v) = supports_tool_calling {
                obj.insert("supports_tool_calling".into(), Value::Bool(v));
            }
            if let Some(v) = supports_streaming {
                obj.insert("supports_streaming".into(), Value::Bool(v));
            }
            if let Some(v) = supports_prompt_caching {
                obj.insert("supports_prompt_caching".into(), Value::Bool(v));
            }
            if allow_private_hosts {
                obj.insert("allow_private_hosts".into(), Value::Bool(true));
            }
            if let Some(v) = auth_header {
                obj.insert("auth_header".into(), Value::String(v));
            }
            if let Some(v) = auth_prefix {
                obj.insert("auth_prefix".into(), Value::String(v));
            }
            if let Some(v) = chat_path {
                obj.insert("chat_path".into(), Value::String(v));
            }
            if let Some(v) = models_path {
                obj.insert("models_path".into(), Value::String(v));
            }
            if !extra_headers.is_empty() {
                let map: HashMap<String, String> = extra_headers.into_iter().collect();
                obj.insert(
                    "extra_headers".into(),
                    serde_json::to_value(map).unwrap_or(Value::Null),
                );
            }

            let response = client
                .send_command(KernelCommand::AddProvider { entry_json: entry })
                .await?;
            match response {
                KernelResponse::Success { data } => {
                    let replaced = data
                        .as_ref()
                        .and_then(|d| d.get("replaced"))
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    println!(
                        "Provider '{}' {}",
                        name,
                        if replaced { "replaced" } else { "added" }
                    );
                }
                KernelResponse::Error { message } => eprintln!("Error: {}", message),
                _ => eprintln!("Unexpected response"),
            }
        }
        ProviderCommands::Remove { name } => {
            let response = client
                .send_command(KernelCommand::RemoveProvider { name: name.clone() })
                .await?;
            match response {
                KernelResponse::Success { .. } => println!("Provider '{}' removed", name),
                KernelResponse::Error { message } => eprintln!("Error: {}", message),
                _ => eprintln!("Unexpected response"),
            }
        }
        ProviderCommands::Probe { name } => {
            let response = client
                .send_command(KernelCommand::ProbeProviderModels { name: name.clone() })
                .await?;
            match response {
                KernelResponse::Success { data } => {
                    let models = data
                        .as_ref()
                        .and_then(|d| d.get("models"))
                        .and_then(Value::as_array)
                        .map(|arr| {
                            arr.iter()
                                .filter_map(Value::as_str)
                                .map(String::from)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    println!("Provider '{}' models ({}):", name, models.len());
                    for m in &models {
                        println!("  {}", m);
                    }
                }
                KernelResponse::Error { message } => eprintln!("Error: {}", message),
                _ => eprintln!("Unexpected response"),
            }
        }
    }
    Ok(())
}
