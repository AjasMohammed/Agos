//! Ed25519 manifest signature verification for the tool registry.
//!
//! # Signing payload
//!
//! The payload is a canonical JSON object (sorted keys, no extra whitespace) over
//! the security-relevant fields of a `ToolManifest`. Mutable metadata fields
//! (`description`, `checksum`) and the signature itself are excluded.
//!
//! ```json
//! {"author":"...","capabilities":[...],"max_cpu_ms":N,"max_memory_mb":N,"name":"...","network":B,"version":"...","weight":"stateless"}
//! ```
//!
//! # Trust tier policy
//!
//! | Tier        | Behavior                                            |
//! |-------------|-----------------------------------------------------|
//! | `Core`      | Accepted without signature — distribution-trusted.  |
//! | `Verified`  | Author Ed25519 signature required and verified.     |
//! | `Community` | Author Ed25519 signature required and verified.     |
//! | `Blocked`   | Hard-rejected; `ToolBlocked` error returned.        |

use agentos_types::{AgentOSError, ToolManifest, TrustTier};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde_json::{json, Map, Value};
use std::collections::HashSet;

/// Build the deterministic signing payload for a manifest.
///
/// Uses `serde_json` with a BTreeMap-ordered object so key order is always
/// alphabetical, producing the same bytes regardless of platform or field order.
pub fn signing_payload(manifest: &ToolManifest) -> Vec<u8> {
    let mut caps = manifest.capabilities_required.permissions.clone();
    caps.sort(); // deterministic order

    let mut payload = Map::new();
    payload.insert("author".to_string(), json!(manifest.manifest.author));
    payload.insert("capabilities".to_string(), json!(caps));
    payload.insert("max_cpu_ms".to_string(), json!(manifest.sandbox.max_cpu_ms));
    payload.insert(
        "max_memory_mb".to_string(),
        json!(manifest.sandbox.max_memory_mb),
    );
    payload.insert("name".to_string(), json!(manifest.manifest.name));
    payload.insert("network".to_string(), json!(manifest.sandbox.network));
    payload.insert("version".to_string(), json!(manifest.manifest.version));
    if let Some(weight) = manifest.sandbox.weight.as_ref() {
        payload.insert("weight".to_string(), json!(weight));
    }

    // serde_json serialises Value::Object with BTreeMap-ordered keys
    serde_json::to_vec(&Value::Object(payload))
        .expect("signing payload serialization is infallible")
}

/// Verify the Ed25519 signature on a manifest.
///
/// Returns `Ok(())` for `Core` (unconditionally trusted) and for `Verified`/
/// `Community` manifests with a valid author signature. Returns an error for
/// `Blocked` or any manifest where the signature is absent or invalid.
pub fn verify_manifest(manifest: &ToolManifest) -> Result<(), AgentOSError> {
    let info = &manifest.manifest;

    match info.trust_tier {
        TrustTier::Blocked => Err(AgentOSError::ToolBlocked {
            name: info.name.clone(),
        }),

        // Core tools are part of the AgentOS distribution and are trusted without
        // a runtime signature check. In a production hardened build this would
        // verify against an embedded foundation public key.
        TrustTier::Core => Ok(()),

        TrustTier::Verified | TrustTier::Community => {
            let pubkey_hex = info.author_pubkey.as_deref().ok_or_else(|| {
                AgentOSError::ToolSignatureInvalid {
                    name: info.name.clone(),
                    reason: "missing author_pubkey field".into(),
                }
            })?;

            let sig_hex =
                info.signature
                    .as_deref()
                    .ok_or_else(|| AgentOSError::ToolSignatureInvalid {
                        name: info.name.clone(),
                        reason: "missing signature field".into(),
                    })?;

            verify_ed25519(
                info.name.as_str(),
                pubkey_hex,
                sig_hex,
                &signing_payload(manifest),
            )
        }
    }
}

/// Low-level Ed25519 verify: pubkey_hex + sig_hex over `message`.
fn verify_ed25519(
    tool_name: &str,
    pubkey_hex: &str,
    sig_hex: &str,
    message: &[u8],
) -> Result<(), AgentOSError> {
    let pub_bytes = hex::decode(pubkey_hex).map_err(|e| AgentOSError::ToolSignatureInvalid {
        name: tool_name.to_string(),
        reason: format!("invalid author_pubkey hex: {e}"),
    })?;

    let pub_array: [u8; 32] =
        pub_bytes
            .try_into()
            .map_err(|_| AgentOSError::ToolSignatureInvalid {
                name: tool_name.to_string(),
                reason: "author_pubkey must be 32 bytes (64 hex chars)".into(),
            })?;

    let verifying_key =
        VerifyingKey::from_bytes(&pub_array).map_err(|e| AgentOSError::ToolSignatureInvalid {
            name: tool_name.to_string(),
            reason: format!("invalid author_pubkey: {e}"),
        })?;

    let sig_bytes = hex::decode(sig_hex).map_err(|e| AgentOSError::ToolSignatureInvalid {
        name: tool_name.to_string(),
        reason: format!("invalid signature hex: {e}"),
    })?;

    let sig_array: [u8; 64] =
        sig_bytes
            .try_into()
            .map_err(|_| AgentOSError::ToolSignatureInvalid {
                name: tool_name.to_string(),
                reason: "signature must be 64 bytes (128 hex chars)".into(),
            })?;

    let signature = Signature::from_bytes(&sig_array);

    verifying_key
        .verify(message, &signature)
        .map_err(|_| AgentOSError::ToolSignatureInvalid {
            name: tool_name.to_string(),
            reason: "signature verification failed".into(),
        })
}

/// Sign a manifest payload with a raw 32-byte Ed25519 signing key.
/// Used by the CLI `tool sign` command and tests.
pub fn sign_manifest(manifest: &ToolManifest, signing_key_bytes: &[u8; 32]) -> String {
    use ed25519_dalek::{Signer, SigningKey};
    let key = SigningKey::from_bytes(signing_key_bytes);
    let payload = signing_payload(manifest);
    let sig = key.sign(&payload);
    hex::encode(sig.to_bytes())
}

/// Derive the hex-encoded Ed25519 public key from a 32-byte seed.
pub fn pubkey_hex_from_seed(seed: &[u8; 32]) -> String {
    use ed25519_dalek::SigningKey;
    let key = SigningKey::from_bytes(seed);
    hex::encode(key.verifying_key().to_bytes())
}

/// Certificate Revocation List: a set of author public key hex strings
/// that have been revoked. Tools signed by revoked keys are rejected.
#[derive(Debug, Clone, Default)]
pub struct RevocationList {
    pub revoked_pubkeys: HashSet<String>,
}

impl RevocationList {
    pub fn new() -> Self {
        Self {
            revoked_pubkeys: HashSet::new(),
        }
    }

    /// Load a CRL from a JSON file. The file should contain an array of hex pubkey strings.
    pub fn load_from_file(path: &std::path::Path) -> Result<Self, String> {
        let data =
            std::fs::read_to_string(path).map_err(|e| format!("Failed to read CRL file: {}", e))?;
        let keys: Vec<String> =
            serde_json::from_str(&data).map_err(|e| format!("Failed to parse CRL JSON: {}", e))?;
        Ok(Self {
            revoked_pubkeys: keys.into_iter().collect(),
        })
    }

    /// Check if a pubkey is revoked.
    pub fn is_revoked(&self, pubkey_hex: &str) -> bool {
        self.revoked_pubkeys.contains(pubkey_hex)
    }
}

/// Verify a manifest with an additional CRL check.
/// If the author's public key is in the revocation list, the tool is rejected.
pub fn verify_manifest_with_crl(
    manifest: &ToolManifest,
    crl: &RevocationList,
) -> Result<(), AgentOSError> {
    // CRL check: if the author pubkey is revoked, reject immediately
    if let Some(ref pubkey_hex) = manifest.manifest.author_pubkey {
        if crl.is_revoked(pubkey_hex) {
            return Err(AgentOSError::ToolBlocked {
                name: manifest.manifest.name.clone(),
            });
        }
    }

    // Proceed with normal signature verification
    verify_manifest(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_types::{
        tool::{ToolCapabilities, ToolInfo, ToolOutputs, ToolSchema},
        ToolExecutor, ToolManifest, ToolSandbox, TrustTier,
    };
    use ed25519_dalek::{Signer, SigningKey};

    fn make_manifest(trust_tier: TrustTier) -> ToolManifest {
        ToolManifest {
            manifest: ToolInfo {
                name: "test-tool".into(),
                version: "1.0.0".into(),
                description: "Test".into(),
                author: "test-author".into(),
                checksum: None,
                author_pubkey: None,
                signature: None,
                trust_tier,
            },
            capabilities_required: ToolCapabilities {
                permissions: vec!["fs.read".into()],
            },
            capabilities_provided: ToolOutputs {
                outputs: vec!["content.text".into()],
            },
            intent_schema: ToolSchema {
                input: "TestInput".into(),
                output: "TestOutput".into(),
            },
            input_schema: None,
            sandbox: ToolSandbox {
                network: false,
                fs_write: false,
                gpu: false,
                max_memory_mb: 64,
                max_cpu_ms: 5000,
                syscalls: vec![],
                weight: None,
            },
            executor: ToolExecutor::default(),
        }
    }

    #[test]
    fn core_tool_accepted_without_signature() {
        let manifest = make_manifest(TrustTier::Core);
        assert!(verify_manifest(&manifest).is_ok());
    }

    #[test]
    fn blocked_tool_rejected() {
        let manifest = make_manifest(TrustTier::Blocked);
        let err = verify_manifest(&manifest).unwrap_err();
        assert!(matches!(err, AgentOSError::ToolBlocked { .. }));
    }

    #[test]
    fn community_tool_without_signature_rejected() {
        let manifest = make_manifest(TrustTier::Community);
        let err = verify_manifest(&manifest).unwrap_err();
        assert!(matches!(err, AgentOSError::ToolSignatureInvalid { .. }));
    }

    #[test]
    fn community_tool_with_valid_signature_accepted() {
        let seed = [42u8; 32];
        let signing_key = SigningKey::from_bytes(&seed);
        let pubkey_hex = hex::encode(signing_key.verifying_key().to_bytes());

        let mut manifest = make_manifest(TrustTier::Community);
        manifest.manifest.author_pubkey = Some(pubkey_hex);

        // Sign the payload
        let payload = signing_payload(&manifest);
        let sig = signing_key.sign(&payload);
        manifest.manifest.signature = Some(hex::encode(sig.to_bytes()));

        assert!(verify_manifest(&manifest).is_ok());
    }

    #[test]
    fn tampered_manifest_rejected() {
        let seed = [42u8; 32];
        let signing_key = SigningKey::from_bytes(&seed);
        let pubkey_hex = hex::encode(signing_key.verifying_key().to_bytes());

        let mut manifest = make_manifest(TrustTier::Community);
        manifest.manifest.author_pubkey = Some(pubkey_hex);

        let payload = signing_payload(&manifest);
        let sig = signing_key.sign(&payload);
        manifest.manifest.signature = Some(hex::encode(sig.to_bytes()));

        // Tamper: change version after signing
        manifest.manifest.version = "9.9.9".into();

        let err = verify_manifest(&manifest).unwrap_err();
        assert!(matches!(err, AgentOSError::ToolSignatureInvalid { .. }));
    }

    #[test]
    fn signing_payload_is_deterministic() {
        let m1 = make_manifest(TrustTier::Community);
        let m2 = make_manifest(TrustTier::Community);
        assert_eq!(signing_payload(&m1), signing_payload(&m2));
    }

    #[test]
    fn signing_payload_includes_weight_when_present() {
        let mut manifest = make_manifest(TrustTier::Community);
        manifest.sandbox.weight = Some("stateless".into());

        let payload: serde_json::Value =
            serde_json::from_slice(&signing_payload(&manifest)).unwrap();
        assert_eq!(
            payload.get("weight").and_then(|value| value.as_str()),
            Some("stateless")
        );
    }

    #[test]
    fn tampering_weight_invalidates_signature() {
        let seed = [42u8; 32];
        let signing_key = SigningKey::from_bytes(&seed);
        let pubkey_hex = hex::encode(signing_key.verifying_key().to_bytes());

        let mut manifest = make_manifest(TrustTier::Community);
        manifest.manifest.author_pubkey = Some(pubkey_hex);
        manifest.sandbox.weight = Some("stateless".into());

        let payload = signing_payload(&manifest);
        let sig = signing_key.sign(&payload);
        manifest.manifest.signature = Some(hex::encode(sig.to_bytes()));

        manifest.sandbox.weight = Some("network".into());

        let err = verify_manifest(&manifest).unwrap_err();
        assert!(matches!(err, AgentOSError::ToolSignatureInvalid { .. }));
    }

    #[test]
    fn crl_blocks_revoked_pubkey() {
        let seed = [42u8; 32];
        let signing_key = SigningKey::from_bytes(&seed);
        let pubkey_hex = hex::encode(signing_key.verifying_key().to_bytes());

        let mut manifest = make_manifest(TrustTier::Community);
        manifest.manifest.author_pubkey = Some(pubkey_hex.clone());

        let payload = signing_payload(&manifest);
        let sig = signing_key.sign(&payload);
        manifest.manifest.signature = Some(hex::encode(sig.to_bytes()));

        // Without CRL: accepted
        assert!(verify_manifest_with_crl(&manifest, &RevocationList::new()).is_ok());

        // With CRL containing the pubkey: rejected
        let mut crl = RevocationList::new();
        crl.revoked_pubkeys.insert(pubkey_hex);
        let err = verify_manifest_with_crl(&manifest, &crl).unwrap_err();
        assert!(matches!(err, AgentOSError::ToolBlocked { .. }));
    }

    #[test]
    fn crl_allows_non_revoked_pubkey() {
        let seed = [42u8; 32];
        let signing_key = SigningKey::from_bytes(&seed);
        let pubkey_hex = hex::encode(signing_key.verifying_key().to_bytes());

        let mut manifest = make_manifest(TrustTier::Community);
        manifest.manifest.author_pubkey = Some(pubkey_hex);

        let payload = signing_payload(&manifest);
        let sig = signing_key.sign(&payload);
        manifest.manifest.signature = Some(hex::encode(sig.to_bytes()));

        // CRL with a different key — should pass
        let mut crl = RevocationList::new();
        crl.revoked_pubkeys.insert("deadbeef".repeat(4));
        assert!(verify_manifest_with_crl(&manifest, &crl).is_ok());
    }
}
