use agentos_types::CapabilityToken;
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Feed a length-prefixed byte slice into the HMAC to prevent concatenation collisions.
fn update_length_prefixed(mac: &mut HmacSha256, data: &[u8]) {
    mac.update(&(data.len() as u32).to_le_bytes());
    mac.update(data);
}

/// Feed all security-relevant token fields into the HMAC instance.
/// This is the single canonical field layout — used by both signing and verification.
fn feed_token_fields(mac: &mut HmacSha256, token: &CapabilityToken) {
    // Fixed-width UUID fields (16 bytes each)
    mac.update(token.task_id.as_uuid().as_bytes());
    mac.update(token.agent_id.as_uuid().as_bytes());

    // Tool IDs — BTreeSet guarantees deterministic order; prefix with count
    mac.update(&(token.allowed_tools.len() as u32).to_le_bytes());
    for tool_id in &token.allowed_tools {
        mac.update(tool_id.as_uuid().as_bytes());
    }

    // Intent flags — BTreeSet guarantees deterministic order; prefix with count
    mac.update(&(token.allowed_intents.len() as u32).to_le_bytes());
    for flag in &token.allowed_intents {
        mac.update(&[*flag as u8]);
    }

    // Permission entries — sorted by resource name for deterministic order regardless of
    // insertion order. Without sorting, a token reconstructed with entries in a different
    // order would fail HMAC verification even though it encodes the same permissions.
    let mut sorted_entries: Vec<_> = token.permissions.entries.iter().collect();
    sorted_entries.sort_by(|a, b| a.resource.cmp(&b.resource));
    mac.update(&(sorted_entries.len() as u32).to_le_bytes());
    for entry in &sorted_entries {
        update_length_prefixed(mac, entry.resource.as_bytes());
        mac.update(&[entry.read as u8, entry.write as u8, entry.execute as u8]);
        // Sign expires_at so time-limited permissions can't be made permanent
        match &entry.expires_at {
            Some(dt) => {
                mac.update(&[1u8]);
                update_length_prefixed(mac, dt.to_rfc3339().as_bytes());
            }
            None => {
                mac.update(&[0u8]);
            }
        }
    }

    // Deny entries — must be signed so they can't be stripped from a token
    mac.update(&(token.permissions.deny_entries.len() as u32).to_le_bytes());
    for deny in &token.permissions.deny_entries {
        update_length_prefixed(mac, deny.as_bytes());
    }

    // Token-level timestamps
    update_length_prefixed(mac, token.issued_at.to_rfc3339().as_bytes());
    update_length_prefixed(mac, token.expires_at.to_rfc3339().as_bytes());
}

/// Compute the HMAC-SHA256 signature for a token.
/// Signs over ALL security-relevant fields with length-prefixed encoding:
/// task_id | agent_id | allowed_tools | allowed_intents | permissions (entries + deny_entries) | issued_at | expires_at
pub fn compute_signature(signing_key: &[u8; 32], token: &CapabilityToken) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(signing_key).expect("HMAC can take any size key");
    feed_token_fields(&mut mac, token);
    mac.finalize().into_bytes().to_vec()
}

/// Verify a token's HMAC-SHA256 signature using constant-time comparison.
/// Delegates to `feed_token_fields` to avoid duplicating the HMAC field layout.
pub fn verify_token_signature(signing_key: &[u8; 32], token: &CapabilityToken) -> bool {
    let mut mac = HmacSha256::new_from_slice(signing_key).expect("HMAC can take any size key");
    feed_token_fields(&mut mac, token);
    mac.verify_slice(&token.signature).is_ok()
}
