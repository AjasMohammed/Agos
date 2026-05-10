use crate::kernel::Kernel;
use agentos_bus::KernelResponse;
use agentos_llm::CatalogEntry;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::IpAddr;
use url::Url;

/// Header names that must never appear in `extra_headers`. These carry credentials
/// or identity material and have a dedicated path (`api_key_env` + `auth_header`/
/// `auth_prefix`). Putting them in `extra_headers` would persist plaintext secrets
/// to `providers.toml`.
const FORBIDDEN_EXTRA_HEADER_NAMES: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "api-key",
    "x-api-key",
    "x-auth-token",
    "cookie",
    "set-cookie",
];

/// Returns true if the byte rejects a header field-name token. Allows the
/// RFC 7230 tchar set: alphanum + `! # $ % & ' * + - . ^ _ ` | ~`.
fn is_safe_header_name_byte(b: u8) -> bool {
    matches!(b,
        b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' |
        b'!' | b'#' | b'$' | b'%' | b'&' | b'\'' | b'*' | b'+' | b'-' |
        b'.' | b'^' | b'_' | b'`' | b'|' | b'~'
    )
}

/// Returns Err if the value contains CR, LF, NUL, or any other control byte
/// that could enable header smuggling. Tabs are tolerated since some APIs
/// accept them.
fn reject_control_chars(label: &str, value: &str) -> Result<(), String> {
    for &b in value.as_bytes() {
        if b == b'\r' || b == b'\n' || b == b'\0' || (b < 0x20 && b != b'\t') {
            return Err(format!(
                "{label} contains a control character (CR/LF/NUL); refusing to accept"
            ));
        }
    }
    Ok(())
}

fn validate_header_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Header name cannot be empty".into());
    }
    if !name.bytes().all(is_safe_header_name_byte) {
        return Err(format!(
            "Header name '{name}' contains illegal characters (allowed: alphanum + !#$%&'*+-.^_`|~)"
        ));
    }
    Ok(())
}

/// SSRF guard for catalog `base_url`. Rejects literal private/loopback/link-local
/// IPs (including IPv4-mapped IPv6 forms) and well-known internal hostnames.
/// `allow_private` short-circuits the check for legitimate localhost providers
/// (lmstudio, ollama, vllm).
fn validate_base_url_target(base_url: &str, allow_private: bool) -> Result<(), String> {
    let parsed = Url::parse(base_url).map_err(|e| format!("base_url is not a valid URL: {e}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!(
            "base_url scheme must be http or https (got '{}')",
            parsed.scheme()
        ));
    }
    let host = parsed
        .host()
        .ok_or_else(|| "base_url is missing a host".to_string())?;
    if allow_private {
        return Ok(());
    }
    // IP literal — apply the same private-range rules used by web_fetch. Use
    // the typed `url::Host` so IPv6 (and IPv4-mapped IPv6 like
    // `::ffff:169.254.169.254`) are caught regardless of textual normalisation.
    let host_string = host.to_string();
    match host {
        url::Host::Ipv4(v4) => {
            if is_private_ip(&IpAddr::V4(v4)) {
                return Err(format!(
                    "base_url host '{host_string}' is private/loopback/link-local. \
                     Pass --allow-private-hosts (or set allow_private_hosts = true) for local providers like lmstudio/ollama."
                ));
            }
            return Ok(());
        }
        url::Host::Ipv6(v6) => {
            if is_private_ip(&IpAddr::V6(v6)) {
                return Err(format!(
                    "base_url host '{host_string}' is private/loopback/link-local. \
                     Pass --allow-private-hosts for local providers."
                ));
            }
            return Ok(());
        }
        url::Host::Domain(_) => {}
    }
    let host = host_string.as_str();
    // Hostname — reject obvious internal names. We do not perform DNS at
    // validation time (forward-lookup TOCTOU; resolver may be offline) — the
    // probe path enforces this via runtime resolution if we add it later.
    let host_lc = host.to_ascii_lowercase();
    let internal = matches!(
        host_lc.as_str(),
        "localhost" | "ip6-localhost" | "ip6-loopback"
    ) || host_lc.ends_with(".localhost")
        || host_lc.ends_with(".local")
        || host_lc.ends_with(".internal")
        || host_lc.ends_with(".lan")
        || host_lc.ends_with(".intranet");
    if internal {
        return Err(format!(
            "base_url host '{host}' looks internal. \
             Pass --allow-private-hosts for local providers."
        ));
    }
    Ok(())
}

/// Self-contained copy of the SSRF private-range matcher in
/// `agentos-tools::ssrf`. Duplicated here to avoid a kernel→tools dependency
/// cycle. Keep in sync with that copy.
fn is_private_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_unspecified()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_multicast()
                || {
                    let o = v4.octets();
                    o[0] == 100 && o[1] >= 64 && o[1] < 128
                }
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_private_ip(&IpAddr::V4(v4));
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

impl Kernel {
    /// List all available LLM providers: built-in (native) providers and
    /// catalog providers loaded from `config/providers.toml`.
    pub(crate) async fn cmd_list_providers(&self) -> KernelResponse {
        let mut entries = Vec::new();

        // Built-in providers
        let builtins = [
            ("openai", "OpenAI", "OPENAI_API_KEY"),
            ("anthropic", "Anthropic", "ANTHROPIC_API_KEY"),
            ("gemini", "Gemini", "GEMINI_API_KEY"),
            ("ollama", "Ollama (local)", ""),
        ];

        for (name, display_name, api_key_env) in &builtins {
            let key_set = if api_key_env.is_empty() {
                true // Local providers don't need an API key
            } else {
                std::env::var(api_key_env)
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .is_some()
            };
            entries.push(json!({
                "name": name,
                "display_name": display_name,
                "source": "built-in",
                "api_key_env": api_key_env,
                "api_key_set": key_set,
                "default_model": "",
            }));
        }

        // Catalog providers
        let catalog_entries: Vec<agentos_llm::CatalogEntry> = self
            .provider_catalog
            .read()
            .expect("provider_catalog lock poisoned")
            .list()
            .into_iter()
            .cloned()
            .collect();
        for entry in &catalog_entries {
            let key_set = if entry.api_key_env.is_empty() {
                true // Local providers don't need an API key
            } else {
                std::env::var(&entry.api_key_env)
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .is_some()
            };
            entries.push(json!({
                "name": entry.name,
                "display_name": entry.display_name,
                "source": "catalog",
                "api_key_env": entry.api_key_env,
                "api_key_set": key_set,
                "default_model": entry.default_model,
                "compatible_with": entry.compatible_with,
                "models": entry.models,
            }));
        }

        KernelResponse::ProviderList(entries)
    }

    /// Update the base URL for a named catalog provider, persisting the change
    /// back to `providers.toml`.
    pub(crate) async fn cmd_set_provider_url(&self, name: String, url: String) -> KernelResponse {
        // Reject empty/whitespace URLs — an empty catalog entry would cause every
        // agent that resolves to this provider to fail with a reqwest builder error.
        if url.trim().is_empty() {
            return KernelResponse::Error {
                message: "Provider URL cannot be empty. Provide a full URL like 'http://host:port'"
                    .to_string(),
            };
        }
        // SSRF guard. Honor the entry's `allow_private_hosts` so legitimate
        // local providers (lmstudio, ollama) keep working on a URL change.
        let allow_private = self
            .provider_catalog
            .read()
            .expect("provider_catalog lock poisoned")
            .lookup(&name)
            .and_then(|e| e.allow_private_hosts)
            .unwrap_or(false);
        if let Err(msg) = validate_base_url_target(&url, allow_private) {
            return KernelResponse::Error { message: msg };
        }
        // Update in-memory catalog
        let updated = self
            .provider_catalog
            .write()
            .expect("provider_catalog lock poisoned")
            .set_base_url(&name, url.clone());

        if !updated {
            return KernelResponse::Error {
                message: format!(
                    "Provider '{}' not found in catalog. Run 'agentos provider list' to see available providers.",
                    name
                ),
            };
        }

        // Persist to file
        if let Some(path) = &self.catalog_path {
            let catalog_snapshot = self
                .provider_catalog
                .read()
                .expect("provider_catalog lock poisoned")
                .clone_inner();
            let path = path.clone();
            let result =
                tokio::task::spawn_blocking(move || catalog_snapshot.save_to_file(&path)).await;

            match result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    return KernelResponse::Error {
                        message: format!("URL updated in memory but could not save to file: {}", e),
                    }
                }
                Err(e) => {
                    return KernelResponse::Error {
                        message: format!(
                            "URL updated in memory but file write task panicked: {}",
                            e
                        ),
                    }
                }
            }
        }

        tracing::info!(provider = %name, url = %url, "Provider base URL updated");
        KernelResponse::Success { data: None }
    }

    /// Insert or replace a catalog entry. Validates required fields, persists
    /// the catalog to disk, and is safe to call before any agent connects.
    pub(crate) async fn cmd_add_provider(&self, entry_json: Value) -> KernelResponse {
        let entry: CatalogEntry = match serde_json::from_value(entry_json) {
            Ok(e) => e,
            Err(e) => {
                return KernelResponse::Error {
                    message: format!("Invalid CatalogEntry JSON: {}", e),
                }
            }
        };
        if let Err(msg) = validate_entry(&entry) {
            return KernelResponse::Error { message: msg };
        }
        // Warn if the user opted to persist any extra_headers — they land in
        // plaintext `providers.toml`. Auth-shaped names already rejected by
        // validate_extra_headers.
        if let Some(map) = &entry.extra_headers {
            if !map.is_empty() {
                tracing::warn!(
                    provider = %entry.name,
                    header_count = map.len(),
                    "extra_headers will be persisted in plaintext to providers.toml"
                );
            }
        }
        let replaced = self
            .provider_catalog
            .write()
            .expect("provider_catalog lock poisoned")
            .upsert(entry.clone());

        if let Some(err) = self.persist_catalog().await {
            return err;
        }

        tracing::info!(
            provider = %entry.name,
            base_url = %entry.base_url,
            replaced,
            "Provider catalog entry {}",
            if replaced { "replaced" } else { "added" }
        );
        KernelResponse::Success {
            data: Some(json!({
                "name": entry.name,
                "replaced": replaced,
            })),
        }
    }

    /// Remove a provider from the catalog.
    pub(crate) async fn cmd_remove_provider(&self, name: String) -> KernelResponse {
        let removed = self
            .provider_catalog
            .write()
            .expect("provider_catalog lock poisoned")
            .remove(&name);
        if removed.is_none() {
            return KernelResponse::Error {
                message: format!(
                    "Provider '{}' not found in catalog. Run 'agentos provider list'.",
                    name
                ),
            };
        }
        if let Some(err) = self.persist_catalog().await {
            return err;
        }
        tracing::info!(provider = %name, "Provider removed from catalog");
        KernelResponse::Success { data: None }
    }

    /// GET `<base_url><models_path>` and replace the catalog entry's models list.
    pub(crate) async fn cmd_probe_provider_models(&self, name: String) -> KernelResponse {
        let entry = match self
            .provider_catalog
            .read()
            .expect("provider_catalog lock poisoned")
            .lookup(&name)
            .cloned()
        {
            Some(e) => e,
            None => {
                return KernelResponse::Error {
                    message: format!("Provider '{}' not found in catalog.", name),
                }
            }
        };
        let snapshot_base_url = entry.base_url.clone();
        let key = if entry.api_key_env.is_empty() {
            None
        } else {
            std::env::var(&entry.api_key_env)
                .ok()
                .filter(|s| !s.trim().is_empty())
                .map(secrecy::SecretString::new)
        };
        let core =
            agentos_llm::CustomCore::new(key, entry.default_model.clone(), entry.base_url.clone())
                .with_catalog_overrides(&entry);

        let models = match core.probe_models().await {
            Ok(v) => v,
            Err(e) => {
                return KernelResponse::Error {
                    message: format!("Probe failed for '{}': {}", name, e),
                }
            }
        };

        // Re-verify the entry under the write lock — a concurrent
        // `cmd_add_provider` could have replaced it with a different base_url
        // while the probe was in flight, in which case the discovered models
        // belong to the *old* provider and must not be written.
        {
            let mut guard = self
                .provider_catalog
                .write()
                .expect("provider_catalog lock poisoned");
            match guard.lookup(&name) {
                None => {
                    return KernelResponse::Error {
                        message: format!("Provider '{}' was removed during probe", name),
                    };
                }
                Some(current) if current.base_url != snapshot_base_url => {
                    return KernelResponse::Error {
                        message: format!(
                            "Provider '{}' base_url changed during probe (was '{}', now '{}'); discarding probed models",
                            name, snapshot_base_url, current.base_url
                        ),
                    };
                }
                Some(_) => {}
            }
            guard.set_models(&name, models.clone());
        }
        if let Some(err) = self.persist_catalog().await {
            return err;
        }

        KernelResponse::Success {
            data: Some(json!({ "name": name, "models": models })),
        }
    }

    /// Snapshot the catalog and write it back to `catalog_path` on a blocking
    /// task. Returns `None` on success or an `Error` response on failure.
    async fn persist_catalog(&self) -> Option<KernelResponse> {
        let path = match &self.catalog_path {
            Some(p) => p.clone(),
            None => return None,
        };
        let snapshot = self
            .provider_catalog
            .read()
            .expect("provider_catalog lock poisoned")
            .clone_inner();
        let result = tokio::task::spawn_blocking(move || snapshot.save_to_file(&path)).await;
        match result {
            Ok(Ok(())) => None,
            Ok(Err(e)) => Some(KernelResponse::Error {
                message: format!(
                    "Catalog updated in memory but could not save to file: {}",
                    e
                ),
            }),
            Err(e) => Some(KernelResponse::Error {
                message: format!("Catalog file write task panicked: {}", e),
            }),
        }
    }
}

fn validate_entry(entry: &CatalogEntry) -> Result<(), String> {
    if entry.name.trim().is_empty() {
        return Err("Provider name cannot be empty".into());
    }
    if entry.name.len() > 64 {
        return Err("Provider name too long (max 64 chars)".into());
    }
    if !entry
        .name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err("Provider name must be alphanumeric with `-`, `_`, or `.` only".into());
    }
    if entry.base_url.trim().is_empty() {
        return Err("base_url cannot be empty".into());
    }
    let allow_private = entry.allow_private_hosts.unwrap_or(false);
    validate_base_url_target(&entry.base_url, allow_private)?;

    let dialect = entry.compatible_with.trim().to_ascii_lowercase();
    if !matches!(
        dialect.as_str(),
        "openai" | "anthropic" | "gemini" | "ollama"
    ) {
        return Err(format!(
            "compatible_with must be one of: openai, anthropic, gemini, ollama (got '{}')",
            entry.compatible_with
        ));
    }

    // Header injection guards on every user-supplied header-shaped field.
    if let Some(name) = &entry.auth_header {
        validate_header_name(name)?;
    }
    if let Some(prefix) = &entry.auth_prefix {
        reject_control_chars("auth_prefix", prefix)?;
    }
    if let Some(path) = &entry.chat_path {
        reject_control_chars("chat_path", path)?;
        if !path.starts_with('/') && !path.starts_with('?') {
            return Err(format!(
                "chat_path must start with '/' or '?' (got '{}')",
                path
            ));
        }
    }
    if let Some(path) = &entry.models_path {
        reject_control_chars("models_path", path)?;
        if !path.starts_with('/') && !path.starts_with('?') {
            return Err(format!(
                "models_path must start with '/' or '?' (got '{}')",
                path
            ));
        }
    }
    if let Some(map) = &entry.extra_headers {
        validate_extra_headers(map)?;
    }
    Ok(())
}

/// Reject auth-shaped header names (would leak secrets to plaintext
/// `providers.toml`) and any header name/value containing control bytes
/// that could enable header smuggling. Case-insensitive on names.
fn validate_extra_headers(map: &HashMap<String, String>) -> Result<(), String> {
    for (k, v) in map {
        validate_header_name(k)?;
        let lc = k.to_ascii_lowercase();
        if FORBIDDEN_EXTRA_HEADER_NAMES.contains(&lc.as_str()) {
            return Err(format!(
                "extra_headers must not contain credential headers ('{k}'). \
                 Use --api-key-env + --auth-header/--auth-prefix for auth credentials."
            ));
        }
        reject_control_chars(&format!("extra_headers value for '{k}'"), v)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_entry() -> CatalogEntry {
        CatalogEntry {
            name: "groq".into(),
            display_name: "Groq".into(),
            base_url: "https://api.groq.com/openai/v1".into(),
            api_key_env: "GROQ_API_KEY".into(),
            compatible_with: "openai".into(),
            default_model: "llama-3.3-70b-versatile".into(),
            ..Default::default()
        }
    }

    #[test]
    fn validate_entry_accepts_basic() {
        assert!(validate_entry(&ok_entry()).is_ok());
    }

    #[test]
    fn validate_entry_rejects_empty_name() {
        let mut e = ok_entry();
        e.name = String::new();
        assert!(validate_entry(&e).is_err());
    }

    #[test]
    fn validate_entry_rejects_bad_chars_in_name() {
        let mut e = ok_entry();
        e.name = "../etc/passwd".into();
        assert!(validate_entry(&e).is_err());
    }

    #[test]
    fn validate_entry_rejects_non_http_scheme() {
        let mut e = ok_entry();
        e.base_url = "file:///etc/passwd".into();
        assert!(validate_entry(&e).is_err());
    }

    #[test]
    fn validate_entry_rejects_unknown_dialect() {
        let mut e = ok_entry();
        e.compatible_with = "cohere".into();
        assert!(validate_entry(&e).is_err());
    }

    #[test]
    fn validate_entry_rejects_aws_metadata_ip() {
        let mut e = ok_entry();
        e.base_url = "http://169.254.169.254/latest".into();
        let err = validate_entry(&e).unwrap_err();
        assert!(err.to_lowercase().contains("private") || err.contains("169.254"));
    }

    #[test]
    fn validate_entry_rejects_loopback_without_flag() {
        let mut e = ok_entry();
        e.base_url = "http://127.0.0.1:1234/v1".into();
        assert!(validate_entry(&e).is_err());
    }

    #[test]
    fn validate_entry_accepts_loopback_with_allow_private() {
        let mut e = ok_entry();
        e.base_url = "http://127.0.0.1:1234/v1".into();
        e.allow_private_hosts = Some(true);
        assert!(validate_entry(&e).is_ok());
    }

    #[test]
    fn validate_entry_rejects_localhost_hostname() {
        let mut e = ok_entry();
        e.base_url = "http://localhost:1234/v1".into();
        assert!(validate_entry(&e).is_err());
    }

    #[test]
    fn validate_entry_rejects_ipv4_mapped_ipv6_metadata() {
        let mut e = ok_entry();
        e.base_url = "http://[::ffff:169.254.169.254]/latest".into();
        assert!(validate_entry(&e).is_err());
    }

    #[test]
    fn validate_entry_rejects_dotinternal_hostname() {
        let mut e = ok_entry();
        e.base_url = "https://llm.internal/v1".into();
        assert!(validate_entry(&e).is_err());
    }

    #[test]
    fn validate_entry_rejects_crlf_in_chat_path() {
        let mut e = ok_entry();
        e.chat_path = Some("/chat\r\nX-Injected: 1".into());
        assert!(validate_entry(&e).is_err());
    }

    #[test]
    fn validate_entry_rejects_crlf_in_auth_prefix() {
        let mut e = ok_entry();
        e.auth_prefix = Some("Bearer \r\nX: 1 ".into());
        assert!(validate_entry(&e).is_err());
    }

    #[test]
    fn validate_entry_rejects_bad_chat_path() {
        let mut e = ok_entry();
        e.chat_path = Some("chat".into());
        assert!(validate_entry(&e).is_err());
    }

    #[test]
    fn validate_entry_rejects_authorization_in_extra_headers() {
        let mut e = ok_entry();
        let mut h = HashMap::new();
        h.insert("Authorization".into(), "Bearer sk-secret".into());
        e.extra_headers = Some(h);
        let err = validate_entry(&e).unwrap_err();
        assert!(err.to_lowercase().contains("credential"));
    }

    #[test]
    fn validate_entry_rejects_x_api_key_in_extra_headers() {
        let mut e = ok_entry();
        let mut h = HashMap::new();
        h.insert("x-api-key".into(), "sk-secret".into());
        e.extra_headers = Some(h);
        assert!(validate_entry(&e).is_err());
    }

    #[test]
    fn validate_entry_accepts_normal_extra_headers() {
        let mut e = ok_entry();
        let mut h = HashMap::new();
        h.insert("X-Tenant".into(), "engineering".into());
        e.extra_headers = Some(h);
        assert!(validate_entry(&e).is_ok());
    }

    #[test]
    fn validate_entry_rejects_bad_header_name() {
        let mut e = ok_entry();
        let mut h = HashMap::new();
        h.insert("X Bad Header".into(), "v".into());
        e.extra_headers = Some(h);
        assert!(validate_entry(&e).is_err());
    }
}
