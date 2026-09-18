//! CVE class: SSRF to cloud metadata / loopback via the agent's network permission.
//!
//! Public incident shape: OpenClaw's link-preview fetch let an injected URL reach
//! `169.254.169.254` and exfiltrate instance credentials. The control under test
//! is `PermissionSet::check` on `net:` targets, which every HTTP-capable tool
//! consults before connecting. Every encoding an HTTP client would still resolve
//! to a blocked address must be refused, and public hosts must still pass.

use agentos_types::{PermissionOp, PermissionSet};

fn net_perms() -> PermissionSet {
    let mut perms = PermissionSet::new();
    perms.grant("net:".into(), true, false, true, None);
    perms
}

#[test]
fn metadata_and_loopback_targets_are_blocked_in_every_encoding() {
    let perms = net_perms();
    let blocked = [
        // Cloud metadata, canonical and via userinfo smuggling.
        "http://169.254.169.254/latest/meta-data/",
        "http://x@169.254.169.254/latest/meta-data/",
        "http://user:pass@169.254.169.254:80/",
        // Integer-encoded IPv4 (decimal / hex) for 169.254.169.254.
        "http://2852039166/",
        "http://0xa9fea9fe/",
        "http://0xA9FEA9FE/",
        // IPv6-mapped IPv4 and loopback.
        "http://[::ffff:169.254.169.254]/",
        "http://[::ffff:a9fe:a9fe]/",
        "http://[::1]/",
        // Loopback in short, long, octal, and per-octet hex forms (inet_aton).
        "http://127.0.0.1:8080/",
        "http://127.1/",
        "http://127.0.1/",
        "http://0177.0.0.1/",
        "http://0x7f.0.0.1/",
        "http://0x7f.1/",
        "http://2130706433/",
        "http://0251.0376.0251.0376/",
        "http://localhost/",
        "http://localhost:11434/api/generate",
        // Provider metadata hostnames, including absolute (trailing-dot) FQDNs.
        "http://metadata.google.internal/computeMetadata/v1/",
        "http://metadata.google.internal./computeMetadata/v1/",
        "http://METADATA.GOOGLE.INTERNAL/",
        "http://metadata.goog/",
        "http://metadata/computeMetadata/v1/",
        "http://instance-data/latest/meta-data/",
        // RFC1918 and link-local ranges.
        "http://10.0.0.1/",
        "http://192.168.1.1/",
        "http://172.16.0.1/",
        "http://100.64.0.1/",
        "http://[fe80::1]/",
        "http://[fd00::1]/",
        // Non-HTTP scheme must not bypass the host check.
        "ftp://169.254.169.254/",
        "gopher://127.0.0.1:70/",
    ];
    for target in blocked {
        assert!(
            !perms.check(&format!("net:{target}"), PermissionOp::Read),
            "SSRF target must be blocked: {target}"
        );
    }
}

/// Positive control: the block list must not eat legitimate egress.
#[test]
fn public_hosts_remain_reachable() {
    let perms = net_perms();
    let allowed = [
        "https://example.com/",
        "https://api.anthropic.com/v1/messages",
        "https://token@api.openai.com/v1",
        "http://93.184.216.34/",
        "https://[2606:2800:220:1:248:1893:25c8:1946]/",
        "https://fdic.gov/",
        "https://metadata-api.example.com/",
    ];
    for target in allowed {
        assert!(
            perms.check(&format!("net:{target}"), PermissionOp::Read),
            "public target must stay reachable: {target}"
        );
    }
}
