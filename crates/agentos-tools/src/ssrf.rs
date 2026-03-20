/// Shared SSRF protection utilities used by `web_fetch` and `http_client`.
///
/// Both tools perform pre-flight IP-range checks. Keeping the logic here
/// prevents the two implementations from drifting (e.g. one copy gaining
/// CGN-range coverage while the other does not).
///
/// Returns `true` if the IP address falls into any range that should never be
/// reachable from an agent-initiated HTTP request.
///
/// Blocked ranges:
/// - IPv4 loopback (127.0.0.0/8)
/// - IPv4 unspecified (0.0.0.0; treated as loopback by Linux)
/// - IPv4 private (RFC 1918): 10/8, 172.16/12, 192.168/16
/// - IPv4 link-local (RFC 3927): 169.254/16  ← cloud metadata endpoint
/// - IPv4 multicast (224.0.0.0/4)
/// - IPv4 Carrier-Grade NAT (RFC 6598): 100.64/10
/// - IPv6 loopback, unspecified, multicast
/// - IPv6 unique-local fc00::/7
/// - IPv6 link-local fe80::/10
/// - IPv4-mapped IPv6 (::ffff:0:0/96) — applies the IPv4 rules to the embedded address
///
/// This function is a complete single source of truth — callers do not need to
/// separately check `is_loopback()`, `is_unspecified()`, or `is_multicast()`.
pub(crate) fn is_private_ip(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ipv4) => {
            ipv4.is_loopback()
                || ipv4.is_unspecified()
                || ipv4.is_private()
                || ipv4.is_link_local()
                || ipv4.is_multicast()
                // 100.64.0.0/10 — Carrier-Grade NAT (RFC 6598); used internally
                // by some cloud providers and ISPs.
                || {
                    let o = ipv4.octets();
                    o[0] == 100 && o[1] >= 64 && o[1] < 128
                }
        }
        std::net::IpAddr::V6(ipv6) => {
            // IPv4-mapped IPv6 addresses (::ffff:x.x.x.x) must be treated as
            // their embedded IPv4 address to prevent bypasses like ::ffff:127.0.0.1
            // or ::ffff:169.254.169.254 (cloud metadata endpoint).
            if let Some(ipv4) = ipv6.to_ipv4_mapped() {
                return is_private_ip(&std::net::IpAddr::V4(ipv4));
            }
            ipv6.is_loopback()
                || ipv6.is_unspecified()
                || ipv6.is_multicast()
                // fc00::/7 — unique local
                || (ipv6.segments()[0] & 0xfe00) == 0xfc00
                // fe80::/10 — link local
                || (ipv6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    #[test]
    fn blocks_loopback() {
        assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))));
        assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::new(
            127, 255, 255, 255
        ))));
    }

    #[test]
    fn blocks_rfc1918() {
        assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
        assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1))));
        assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));
    }

    #[test]
    fn blocks_link_local_metadata() {
        // 169.254.169.254 is the cloud metadata endpoint (AWS/GCP/Azure)
        assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::new(
            169, 254, 169, 254
        ))));
    }

    #[test]
    fn blocks_cgn_range() {
        // RFC 6598 — 100.64.0.0/10
        assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1))));
        assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::new(
            100, 127, 255, 255
        ))));
        // Boundary: 100.63.x.x and 100.128.x.x are NOT CGN
        assert!(!is_private_ip(&IpAddr::V4(Ipv4Addr::new(100, 63, 0, 1))));
        assert!(!is_private_ip(&IpAddr::V4(Ipv4Addr::new(100, 128, 0, 1))));
    }

    #[test]
    fn blocks_unspecified_and_multicast_ipv4() {
        // 0.0.0.0 — Linux treats this as loopback
        assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0))));
        // 224.0.0.1 — IPv4 multicast
        assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::new(224, 0, 0, 1))));
    }

    #[test]
    fn allows_public_ipv4() {
        assert!(!is_private_ip(&IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(!is_private_ip(&IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))));
    }

    #[test]
    fn blocks_ipv6_private_ranges() {
        // fc00::/7 — unique local
        assert!(is_private_ip(&IpAddr::V6(
            "fc00::1".parse::<Ipv6Addr>().unwrap()
        )));
        // fe80::/10 — link local
        assert!(is_private_ip(&IpAddr::V6(
            "fe80::1".parse::<Ipv6Addr>().unwrap()
        )));
    }

    #[test]
    fn allows_public_ipv6() {
        assert!(!is_private_ip(&IpAddr::V6(
            "2001:4860:4860::8888".parse::<Ipv6Addr>().unwrap()
        )));
    }

    #[test]
    fn blocks_ipv4_mapped_ipv6() {
        // ::ffff:127.0.0.1 — IPv4-mapped loopback; must not bypass SSRF checks
        assert!(is_private_ip(&IpAddr::V6(
            "::ffff:127.0.0.1".parse::<Ipv6Addr>().unwrap()
        )));
        // ::ffff:169.254.169.254 — cloud metadata endpoint via IPv4-mapped IPv6
        assert!(is_private_ip(&IpAddr::V6(
            "::ffff:169.254.169.254".parse::<Ipv6Addr>().unwrap()
        )));
        // ::ffff:10.0.0.1 — RFC 1918 private via IPv4-mapped IPv6
        assert!(is_private_ip(&IpAddr::V6(
            "::ffff:10.0.0.1".parse::<Ipv6Addr>().unwrap()
        )));
        // ::ffff:8.8.8.8 — public IPv4-mapped; must NOT be blocked
        assert!(!is_private_ip(&IpAddr::V6(
            "::ffff:8.8.8.8".parse::<Ipv6Addr>().unwrap()
        )));
    }
}
