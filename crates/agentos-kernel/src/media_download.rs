//! SSRF-guarded downloader for inbound channel media served at a remote URL
//! (e.g. Discord CDN attachments). Unlike the Telegram `getFile` path — whose
//! URL is derived from `api.telegram.org` and is not attacker-influenceable —
//! these URLs come from platform message payloads, so a kernel-side fetch must
//! be guarded against SSRF (private/loopback/link-local/IMDS targets) and must
//! not follow redirects (a public URL could 302 to an internal host).

use std::net::IpAddr;
use std::time::Duration;

use futures::StreamExt;

/// Maximum bytes for an inbound remote media download.
pub const MAX_REMOTE_MEDIA_BYTES: u64 = 20 * 1024 * 1024;

/// Optional bearer auth for a download, gated to a trusted host suffix. The
/// token is attached ONLY when the (already SSRF-validated, pinned) host equals
/// or is a subdomain of `trusted_host_suffix` — so a channel's bot token is
/// never leaked to an arbitrary URL (e.g. Slack `url_private` → `files.slack.com`).
pub struct MediaAuth<'a> {
    pub bearer: &'a str,
    pub trusted_host_suffix: &'a str,
}

/// True if `ip` is in a range that must never be fetched server-side. Mirrors
/// the inline guards in `agentos-llm`/`agentos-tools` (those are not `pub`).
fn is_private_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_multicast()
                || v4.is_broadcast()
                // "this network" 0.0.0.0/8 (routes to loopback on Linux)
                || v4.octets()[0] == 0
                // CGNAT 100.64.0.0/10
                || (v4.octets()[0] == 100 && (64..=127).contains(&v4.octets()[1]))
        }
        IpAddr::V6(v6) => {
            // Unwrap embedded IPv4 — both IPv4-mapped (::ffff:a.b.c.d) and the
            // deprecated IPv4-compatible (::a.b.c.d) — and re-check as IPv4.
            #[allow(deprecated)]
            if let Some(v4) = v6.to_ipv4() {
                return is_private_ip(&IpAddr::V4(v4));
            }
            let seg = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // unique-local fc00::/7
                || (seg[0] & 0xfe00) == 0xfc00
                // link-local fe80::/10
                || (seg[0] & 0xffc0) == 0xfe80
                // 6to4 2002::/16 (embeds an IPv4 — block defensively)
                || seg[0] == 0x2002
                // NAT64 well-known prefix 64:ff9b::/96
                || (seg[0] == 0x0064 && seg[1] == 0xff9b)
        }
    }
}

/// Download remote media with an SSRF guard, no redirects, and a streamed size
/// cap. Returns `(bytes, detected_mime)`. The MIME is sniffed from magic bytes
/// (see `adapters::telegram::sniff_mime`) — the caller may prefer a
/// platform-declared MIME when present.
pub async fn download_remote_media(
    url: &str,
    max_bytes: u64,
    auth: Option<MediaAuth<'_>>,
) -> Result<(Vec<u8>, String), String> {
    let parsed = reqwest::Url::parse(url).map_err(|_| "invalid media URL".to_string())?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => return Err(format!("unsupported URL scheme '{other}'")),
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| "media URL has no host".to_string())?
        .to_string();
    let port = parsed.port_or_known_default().unwrap_or(443);

    // Resolve the host ONCE, reject any private/internal target, and PIN reqwest
    // to exactly those validated addresses via `resolve_to_addrs`. This closes
    // the DNS-rebinding gap where a second resolution at connect time could
    // return an internal IP after the check passed.
    let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host.as_str(), port))
        .await
        .map_err(|_| format!("DNS resolution failed for host '{host}'"))?
        .collect();
    if addrs.is_empty() {
        return Err(format!("host '{host}' did not resolve"));
    }
    for a in &addrs {
        if is_private_ip(&a.ip()) {
            return Err(format!(
                "host '{host}' resolves to a private/internal address"
            ));
        }
    }

    // Redirects disabled so a vetted public URL cannot bounce to an internal one.
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .resolve_to_addrs(&host, &addrs)
        // Below the InboundRouter's 20s enrich budget so one stuck transfer
        // yields rather than starving the rest of the message's media.
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("http client build failed: {e}"))?;

    let mut req = client.get(parsed);
    if let Some(a) = &auth {
        // Attach the token only when the validated, pinned host is within the
        // trusted suffix — never leak it to an arbitrary host.
        if host == a.trusted_host_suffix || host.ends_with(&format!(".{}", a.trusted_host_suffix)) {
            req = req.bearer_auth(a.bearer);
        }
    }
    let resp = req
        .send()
        .await
        .map_err(|_| "media download request failed (details redacted)".to_string())?;
    if !resp.status().is_success() {
        return Err(format!("media download HTTP {}", resp.status().as_u16()));
    }
    if let Some(len) = resp.content_length() {
        if len > max_bytes {
            return Err(format!("media too large: {len} bytes (cap {max_bytes})"));
        }
    }

    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "media body read failed".to_string())?;
        if buf.len() as u64 + chunk.len() as u64 > max_bytes {
            return Err(format!(
                "media exceeded cap during download (> {max_bytes} bytes)"
            ));
        }
        buf.extend_from_slice(&chunk);
    }

    let mime = crate::adapters::telegram::sniff_mime(&buf).to_string();
    Ok((buf, mime))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn blocks_loopback_and_private_and_imds() {
        assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))));
        assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5))));
        assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));
        assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::new(
            169, 254, 169, 254
        ))));
        assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::new(100, 100, 0, 1))));
    }

    #[test]
    fn allows_public() {
        assert!(!is_private_ip(&IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(!is_private_ip(&IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))));
    }

    #[tokio::test]
    async fn rejects_bad_scheme() {
        let err = download_remote_media("ftp://example.com/x", 1024, None)
            .await
            .unwrap_err();
        assert!(err.contains("scheme"));
    }

    #[tokio::test]
    async fn rejects_private_literal_host() {
        // Literal IPs resolve without network via lookup_host, so this exercises
        // the SSRF rejection branch offline.
        let err = download_remote_media("http://169.254.169.254/latest/meta-data", 1024, None)
            .await
            .unwrap_err();
        assert!(err.contains("private/internal"), "got: {err}");
    }

    #[test]
    fn blocks_embedded_ipv4_and_special_ipv6() {
        use std::net::Ipv6Addr;
        // IPv4-mapped (::ffff:a.b.c.d) and IPv4-compatible (::a.b.c.d) unwrap to v4.
        assert!(is_private_ip(&IpAddr::V6(
            "::ffff:127.0.0.1".parse::<Ipv6Addr>().unwrap()
        )));
        assert!(is_private_ip(&IpAddr::V6(
            "::ffff:10.0.0.1".parse::<Ipv6Addr>().unwrap()
        )));
        // 6to4 (2002::/16) and NAT64 (64:ff9b::/96) embed IPv4 — blocked defensively.
        assert!(is_private_ip(&IpAddr::V6(
            "2002::1".parse::<Ipv6Addr>().unwrap()
        )));
        assert!(is_private_ip(&IpAddr::V6(
            "64:ff9b::1".parse::<Ipv6Addr>().unwrap()
        )));
        // Real public IPv6 still allowed.
        assert!(!is_private_ip(&IpAddr::V6(
            "2606:4700:4700::1111".parse::<Ipv6Addr>().unwrap()
        )));
    }
}
