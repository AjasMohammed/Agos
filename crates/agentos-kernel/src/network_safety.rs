//! SSRF gate for operator-supplied outbound URLs (escalation webhooks, the
//! webhook/Slack delivery adapters, ntfy server URLs).
//!
//! This module used to extract the host by hand — strip the scheme, cut at the
//! first `/?#:` — which never stripped userinfo, so
//! `https://evil.example@169.254.169.254/latest/meta-data/` yielded the "host"
//! `evil.example@169.254.169.254`, matched no private-range prefix, and was
//! accepted while the request went straight to the cloud metadata endpoint. It
//! also missed integer/hex/octal IPv4 (`https://2130706433/`), the CGNAT range
//! and the `.local`/`.internal`/`.lan` suffixes.
//!
//! Parsing and the blocklist are now delegated to
//! [`agentos_channels::webhook::validate_webhook_url`] — the one
//! `url::Url`-based implementation in the workspace — so there is a single
//! place where "is this host reachable" is decided.

use agentos_types::AgentOSError;
use url::Url;

/// Validates that a webhook URL is safe to POST to, preventing SSRF attacks.
///
/// Requires `https` (no cleartext payloads) and rejects loopback, RFC 1918,
/// link-local/cloud-metadata (169.254/16), CGNAT (100.64/10), IPv6
/// loopback/link-local/ULA, IPv4-mapped IPv6, and private-looking hostnames
/// (`localhost`, `*.local`, `*.internal`, `*.lan`, anything containing
/// "metadata").
///
/// Note: DNS rebinding attacks (where a safe hostname later resolves to a
/// private IP) are not mitigated here. For production deployments, perform a
/// post-resolution IP check after `tokio::net::lookup_host`.
pub fn validate_webhook_url(url: &str) -> Result<(), AgentOSError> {
    validate_url(url, true).map_err(AgentOSError::SchemaValidation)
}

/// Internal check that returns a plain `String` error — used by `escalation.rs`
/// via a thin wrapper that converts to `AgentOSError`.
pub(crate) fn validate_webhook_url_str(url: &str) -> Result<(), String> {
    validate_url(url, true)
}

/// Validates that a server URL is safe against SSRF attacks, allowing HTTP or HTTPS.
///
/// Unlike `validate_webhook_url`, this does NOT require HTTPS — it is intended for
/// adapter server URLs (e.g. self-hosted ntfy instances) where HTTP is legitimate.
/// All private/loopback IP blocklist rules still apply.
pub fn validate_server_url(url: &str) -> Result<(), AgentOSError> {
    validate_url(url, false).map_err(AgentOSError::SchemaValidation)
}

fn validate_url(raw: &str, require_https: bool) -> Result<(), String> {
    let label = if require_https { "Webhook" } else { "Server" };

    // WHATWG parsing does the work the hand-rolled extractor got wrong:
    // userinfo is a separate component, integer/hex/octal IPv4 is canonicalised
    // to an `Ipv4Addr`, and an unbracketed IPv6 literal is rejected outright
    // (`:` is a forbidden domain code point) instead of being truncated to its
    // first group.
    let parsed = Url::parse(raw).map_err(|e| format!("{label} URL is not a valid URL: {e}"))?;

    let scheme = parsed.scheme();
    if require_https {
        if scheme != "https" {
            return Err(format!(
                "Webhook URL must use HTTPS scheme (got: '{scheme}')"
            ));
        }
    } else if scheme != "https" && scheme != "http" {
        return Err(format!(
            "Server URL must use http or https scheme (got: '{scheme}')"
        ));
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| format!("{label} URL has no host"))?;
    // Kept from the old validator and not covered by the channels blocklist:
    // internal naming conventions around metadata services.
    if host.to_ascii_lowercase().contains("metadata") {
        return Err(format!(
            "{label} URL appears to target an instance metadata service: '{host}'"
        ));
    }

    // The shared validator requires https and only ever inspects the host, so
    // an http server URL is checked through its https twin. If `set_scheme`
    // ever refuses, the twin stays http and the validator rejects it — the
    // failure mode is closed.
    let mut https_form = parsed.clone();
    if scheme != "https" {
        let _ = https_form.set_scheme("https");
    }
    agentos_channels::webhook::validate_webhook_url(https_form.as_str())
        .map_err(|e| format!("{label} URL rejected: {e}"))
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

    // ── Unbracketed IPv6 bypass regression tests ─────────────────────────────

    #[test]
    fn blocks_unbracketed_ipv6_link_local_webhook() {
        assert!(validate_webhook_url("https://fe80::1/notify").is_err());
    }

    #[test]
    fn blocks_unbracketed_ipv6_link_local_server() {
        assert!(validate_server_url("http://fe80::1/path").is_err());
    }

    #[test]
    fn blocks_unbracketed_ipv6_loopback_server() {
        assert!(validate_server_url("http://::1/path").is_err());
    }

    #[test]
    fn blocks_unbracketed_ipv6_ula_server() {
        assert!(validate_server_url("http://fd00::1/path").is_err());
    }

    #[test]
    fn allows_host_with_port_server() {
        assert!(validate_server_url("http://example.com:8080/path").is_ok());
        assert!(validate_server_url("https://example.com:443/path").is_ok());
    }

    // ── SEC-06: cases the hand-rolled host extractor accepted ────────────────

    #[test]
    fn blocks_userinfo_smuggled_metadata_host() {
        // The bug: host was read as "evil.example@169.254.169.254", which
        // matched no private prefix, so the POST reached the metadata service.
        assert!(validate_webhook_url("https://evil.example@169.254.169.254/").is_err());
        assert!(
            validate_webhook_url("https://evil.example@169.254.169.254/latest/meta-data/").is_err()
        );
        // Userinfo containing its own '@' must not shift the split either.
        assert!(validate_webhook_url("https://a@b@127.0.0.1/notify").is_err());
        assert!(validate_server_url("http://user:pass@10.0.0.1/path").is_err());
    }

    #[test]
    fn blocks_integer_and_hex_encoded_ipv4() {
        // 2130706433 == 0x7f000001 == 127.0.0.1
        assert!(validate_webhook_url("https://2130706433/").is_err());
        assert!(validate_webhook_url("https://0x7f000001/").is_err());
        assert!(validate_server_url("http://2130706433/path").is_err());
    }

    #[test]
    fn blocks_cgnat_and_internal_suffixes() {
        assert!(validate_webhook_url("https://100.64.0.1/notify").is_err());
        assert!(validate_webhook_url("https://vault.internal/notify").is_err());
        assert!(validate_webhook_url("https://printer.local/notify").is_err());
        assert!(validate_webhook_url("https://nas.lan/notify").is_err());
    }

    #[test]
    fn blocks_metadata_hostname() {
        assert!(validate_webhook_url("https://metadata.google.internal/computeMetadata/").is_err());
        assert!(validate_server_url("http://metadata/latest").is_err());
    }
}
