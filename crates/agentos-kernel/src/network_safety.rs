use agentos_types::AgentOSError;

/// Validates that a webhook URL is safe to POST to, preventing SSRF attacks.
///
/// Rules enforced:
/// - Scheme must be `https` (prevents cleartext credential/payload exposure)
/// - Host must not be a loopback address (`localhost`, `127.x.x.x`, `::1`)
/// - Host must not be in RFC 1918 private ranges (10/8, 172.16/12, 192.168/16)
/// - Host must not be a link-local/cloud-metadata address (169.254.x.x)
/// - Host must not be an IPv6 link-local (fe80::/10), ULA (fc00::/7), or loopback (::1)
/// - IPv4-mapped IPv6 addresses (::ffff:x.x.x.x) are checked against IPv4 blocklists
///
/// Note: DNS rebinding attacks (where a safe hostname later resolves to a private IP)
/// are not mitigated here. For production deployments, perform a post-resolution IP
/// check after `tokio::net::lookup_host`.
pub fn validate_webhook_url(url: &str) -> Result<(), AgentOSError> {
    validate_webhook_url_inner(url).map_err(AgentOSError::SchemaValidation)
}

/// Internal check that returns a plain `String` error — used by `escalation.rs`
/// via a thin wrapper that converts to `AgentOSError`.
pub(crate) fn validate_webhook_url_str(url: &str) -> Result<(), String> {
    validate_webhook_url_inner(url)
}

/// Validates that a server URL is safe against SSRF attacks, allowing HTTP or HTTPS.
///
/// Unlike `validate_webhook_url`, this does NOT require HTTPS — it is intended for
/// adapter server URLs (e.g. self-hosted ntfy instances) where HTTP is legitimate.
/// All private/loopback IP blocklist rules still apply.
pub fn validate_server_url(url: &str) -> Result<(), AgentOSError> {
    validate_server_url_inner(url).map_err(AgentOSError::SchemaValidation)
}

fn validate_server_url_inner(url: &str) -> Result<(), String> {
    let after_scheme = if let Some(rest) = url.strip_prefix("https://") {
        rest
    } else if let Some(rest) = url.strip_prefix("http://") {
        rest
    } else {
        return Err(format!(
            "Server URL must use http or https scheme (got: '{}')",
            url.split("://").next().unwrap_or(url)
        ));
    };

    let host = if after_scheme.starts_with('[') {
        match after_scheme.find(']') {
            Some(close) => after_scheme[1..close].to_ascii_lowercase(),
            None => return Err("Server URL has malformed IPv6 literal address".to_string()),
        }
    } else {
        let end = after_scheme
            .find(['/', '?', '#', ':'])
            .unwrap_or(after_scheme.len());
        after_scheme[..end].to_ascii_lowercase()
    };

    if host.is_empty() {
        return Err("Server URL has no host".to_string());
    }
    if host == "localhost" || host.starts_with("127.") || host == "::1" {
        return Err(format!("Server URL targets a loopback address: '{host}'"));
    }
    if host == "0.0.0.0" || host == "::" {
        return Err(format!(
            "Server URL targets a wildcard/unspecified address: '{host}'"
        ));
    }
    if host.starts_with("169.254.") {
        return Err(format!(
            "Server URL targets a link-local/metadata address: '{host}'"
        ));
    }
    if host.starts_with("10.") || host.starts_with("192.168.") {
        return Err(format!("Server URL targets a private IP range: '{host}'"));
    }
    if host.starts_with("172.") {
        if let Some(second) = host.split('.').nth(1) {
            if let Ok(octet) = second.parse::<u8>() {
                if (16..=31).contains(&octet) {
                    return Err(format!("Server URL targets a private IP range: '{host}'"));
                }
            }
        }
    }
    if host.contains("metadata") {
        return Err(format!(
            "Server URL appears to target an instance metadata service: '{host}'"
        ));
    }
    if host.contains(':') {
        if host.starts_with("fe8")
            || host.starts_with("fe9")
            || host.starts_with("fea")
            || host.starts_with("feb")
        {
            return Err(format!(
                "Server URL targets an IPv6 link-local address: '{host}'"
            ));
        }
        if host.starts_with("fc") || host.starts_with("fd") {
            return Err(format!(
                "Server URL targets an IPv6 unique-local (private) address: '{host}'"
            ));
        }
        if let Some(ipv4_part) = host.strip_prefix("::ffff:") {
            return validate_ipv4_mapped(ipv4_part);
        }
    }
    Ok(())
}

fn validate_webhook_url_inner(url: &str) -> Result<(), String> {
    // Require HTTPS to prevent plaintext exposure of the notification payload
    if !url.starts_with("https://") {
        return Err(format!(
            "Webhook URL must use HTTPS scheme (got: '{}')",
            url.split("://").next().unwrap_or(url)
        ));
    }

    let after_scheme = &url["https://".len()..];

    // Extract the host, handling IPv6 bracket notation: [::1] or [::1]:8443
    let host = if after_scheme.starts_with('[') {
        // IPv6 literal — find the closing bracket
        match after_scheme.find(']') {
            Some(close) => after_scheme[1..close].to_ascii_lowercase(),
            None => return Err("Webhook URL has malformed IPv6 literal address".to_string()),
        }
    } else {
        // IPv4 or hostname — terminated by first `/`, `?`, `#`, or `:`
        let end = after_scheme
            .find(['/', '?', '#', ':'])
            .unwrap_or(after_scheme.len());
        after_scheme[..end].to_ascii_lowercase()
    };

    if host.is_empty() {
        return Err("Webhook URL has no host".to_string());
    }

    // Block loopback variants
    if host == "localhost" || host.starts_with("127.") || host == "::1" {
        return Err(format!("Webhook URL targets a loopback address: '{host}'"));
    }

    // Block the unspecified/any address (routes to loopback on many OSes)
    if host == "0.0.0.0" || host == "::" {
        return Err(format!(
            "Webhook URL targets a wildcard/unspecified address: '{host}'"
        ));
    }

    // Block cloud instance metadata service (AWS, GCP, Azure all use 169.254.169.254)
    if host.starts_with("169.254.") {
        return Err(format!(
            "Webhook URL targets a link-local/metadata address: '{host}'"
        ));
    }

    // Block RFC 1918 private ranges: 10.0.0.0/8 and 192.168.0.0/16
    if host.starts_with("10.") || host.starts_with("192.168.") {
        return Err(format!("Webhook URL targets a private IP range: '{host}'"));
    }

    // Block 172.16.0.0/12 (172.16.x.x – 172.31.x.x)
    if host.starts_with("172.") {
        if let Some(second) = host.split('.').nth(1) {
            if let Ok(octet) = second.parse::<u8>() {
                if (16..=31).contains(&octet) {
                    return Err(format!("Webhook URL targets a private IP range: '{host}'"));
                }
            }
        }
    }

    // Block hostnames that contain "metadata" (common internal naming convention)
    if host.contains("metadata") {
        return Err(format!(
            "Webhook URL appears to target an instance metadata service: '{host}'"
        ));
    }

    // ── IPv6-specific blocks ─────────────────────────────────────────────────
    // Only applies when the host is an IPv6 address (contains ':')
    if host.contains(':') {
        // Block IPv6 link-local (fe80::/10: fe80:: – febf::)
        if host.starts_with("fe8")
            || host.starts_with("fe9")
            || host.starts_with("fea")
            || host.starts_with("feb")
        {
            return Err(format!(
                "Webhook URL targets an IPv6 link-local address: '{host}'"
            ));
        }

        // Block IPv6 Unique Local Addresses (ULA, fc00::/7: fc:: – fdff::)
        if host.starts_with("fc") || host.starts_with("fd") {
            return Err(format!(
                "Webhook URL targets an IPv6 unique-local (private) address: '{host}'"
            ));
        }

        // Block IPv4-mapped IPv6 addresses (::ffff:x.x.x.x)
        // These bypass IPv4 blocklist checks above if not handled separately.
        if let Some(ipv4_part) = host.strip_prefix("::ffff:") {
            return validate_ipv4_mapped(ipv4_part);
        }
    }

    Ok(())
}

/// Validates the IPv4 address embedded in an `::ffff:` IPv4-mapped IPv6 address.
fn validate_ipv4_mapped(ipv4: &str) -> Result<(), String> {
    if ipv4.starts_with("127.")
        || ipv4 == "localhost"
        || ipv4 == "0.0.0.0"
        || ipv4.starts_with("169.254.")
        || ipv4.starts_with("10.")
        || ipv4.starts_with("192.168.")
    {
        return Err(format!(
            "Webhook URL targets a private/loopback address via IPv4-mapped IPv6: '::ffff:{ipv4}'"
        ));
    }
    if ipv4.starts_with("172.") {
        if let Some(second) = ipv4.split('.').nth(1) {
            if let Ok(octet) = second.parse::<u8>() {
                if (16..=31).contains(&octet) {
                    return Err(format!(
                        "Webhook URL targets a private address via IPv4-mapped IPv6: '::ffff:{ipv4}'"
                    ));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_http_scheme() {
        assert!(validate_webhook_url("http://example.com/notify").is_err());
    }

    #[test]
    fn allows_https_public_url() {
        assert!(validate_webhook_url("https://example.com/notify").is_ok());
    }

    #[test]
    fn blocks_localhost() {
        assert!(validate_webhook_url("https://localhost/notify").is_err());
    }

    #[test]
    fn blocks_loopback_ip() {
        assert!(validate_webhook_url("https://127.0.0.1/notify").is_err());
        assert!(validate_webhook_url("https://127.1.2.3/notify").is_err());
    }

    #[test]
    fn blocks_private_ranges() {
        assert!(validate_webhook_url("https://10.0.0.1/notify").is_err());
        assert!(validate_webhook_url("https://192.168.1.1/notify").is_err());
        assert!(validate_webhook_url("https://172.16.0.1/notify").is_err());
        assert!(validate_webhook_url("https://172.31.255.255/notify").is_err());
    }

    #[test]
    fn allows_172_32_plus() {
        assert!(validate_webhook_url("https://172.32.0.1/notify").is_ok());
    }

    #[test]
    fn blocks_metadata_service() {
        assert!(validate_webhook_url("https://169.254.169.254/latest/meta-data/").is_err());
    }

    #[test]
    fn blocks_unspecified_address() {
        assert!(validate_webhook_url("https://0.0.0.0/notify").is_err());
    }

    // ── IPv6 SSRF tests ──────────────────────────────────────────────────────

    #[test]
    fn blocks_ipv6_loopback_no_port() {
        assert!(validate_webhook_url("https://[::1]/notify").is_err());
    }

    #[test]
    fn blocks_ipv6_loopback_with_port() {
        // Previously the `:` inside `::1` caused host extraction to fail silently.
        assert!(validate_webhook_url("https://[::1]:8443/notify").is_err());
    }

    #[test]
    fn blocks_ipv6_link_local() {
        assert!(validate_webhook_url("https://[fe80::1]/notify").is_err());
        assert!(validate_webhook_url("https://[fe80::1%25eth0]/notify").is_err());
        // Upper boundary of fe80::/10
        assert!(validate_webhook_url("https://[febf::1]/notify").is_err());
    }

    #[test]
    fn blocks_ipv6_ula() {
        assert!(validate_webhook_url("https://[fd00::1]/notify").is_err());
        assert!(validate_webhook_url("https://[fc00::1]/notify").is_err());
    }

    #[test]
    fn blocks_ipv6_mapped_loopback() {
        assert!(validate_webhook_url("https://[::ffff:127.0.0.1]/notify").is_err());
    }

    #[test]
    fn blocks_ipv6_mapped_private() {
        assert!(validate_webhook_url("https://[::ffff:10.0.0.1]/notify").is_err());
        assert!(validate_webhook_url("https://[::ffff:192.168.1.1]/notify").is_err());
    }

    #[test]
    fn allows_public_ipv6() {
        assert!(validate_webhook_url("https://[2001:db8::1]/notify").is_ok());
        assert!(validate_webhook_url("https://[2001:db8::1]:443/notify").is_ok());
    }
}
