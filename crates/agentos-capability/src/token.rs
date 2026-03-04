use agentos_types::CapabilityToken;
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Compute the HMAC-SHA256 signature for a token.
/// Signs over: task_id | agent_id | allowed_tools | allowed_intents | permissions | issued_at | expires_at
pub fn compute_signature(signing_key: &[u8; 32], token: &CapabilityToken) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(signing_key)
        .expect("HMAC can take any size key");

    // Create a canonical byte representation to sign
    mac.update(token.task_id.as_uuid().as_bytes());
    mac.update(token.agent_id.as_uuid().as_bytes());

    // Sort tool IDs for deterministic signing
    for tool_id in &token.allowed_tools {
        mac.update(tool_id.as_uuid().as_bytes());
    }

    // Encode intent flags as bytes
    for flag in &token.allowed_intents {
        mac.update(&[*flag as u8]);
    }

    // Sign over permissions so they can't be tampered with
    for entry in &token.permissions.entries {
        mac.update(entry.resource.as_bytes());
        mac.update(&[entry.read as u8, entry.write as u8, entry.execute as u8]);
    }

    // Timestamps
    mac.update(token.issued_at.to_rfc3339().as_bytes());
    mac.update(token.expires_at.to_rfc3339().as_bytes());

    mac.finalize().into_bytes().to_vec()
}
