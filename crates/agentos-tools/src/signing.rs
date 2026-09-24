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
//!
//! Independently of tier, only `Core` may declare `risk_class = "interactive"`
//! (auto-allowed under every approval mode) or `executor.type = "privileged"`.

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
    // Sign risk_class and trust_tier so an author cannot downgrade the approval
    // class (or claim a different tier) on an already-validly-signed manifest —
    // both drive enforcement (ApprovalHook friction, signature requirement) and
    // were previously mutable without invalidating the signature.
    payload.insert("risk_class".to_string(), json!(manifest.risk_class));
    // Inserted only when non-empty so every already-signed manifest keeps its
    // existing signature bytes. Adding a table to one of those changes the
    // payload and invalidates the signature, which is the point: the table
    // lowers the approval class per action, so it is exactly the downgrade
    // vector `risk_class` is signed to prevent.
    if !manifest.risk_class_by_action.is_empty() {
        payload.insert(
            "risk_class_by_action".to_string(),
            json!(manifest.risk_class_by_action),
        );
    }
    payload.insert(
        "trust_tier".to_string(),
        json!(manifest.manifest.trust_tier),
    );
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

    // Privileged executor is reserved for distribution-trusted, mandatory-
    // approval tools. Reject any manifest that requests it without both
    // `trust_tier = core` AND `risk_class = control_plane`. Without this
    // gate, a Community-tier tool could escape the bwrap sandbox.
    if manifest.executor.executor_type == agentos_types::ExecutorType::Privileged
        && (info.trust_tier != TrustTier::Core
            || manifest.risk_class != agentos_types::RiskClass::ControlPlane)
    {
        return Err(AgentOSError::ToolBlocked {
            name: info.name.clone(),
        });
    }

    // `ApprovalMode::decide` short-circuits `Interactive` to `Allow` under EVERY
    // mode, `deny` included — prompting a human to approve a request *for* human
    // input is circular. `ReadonlyScoped` short-circuits identically (see
    // `approval.rs`), so the two are equally strong bypasses; `Interactive` is
    // gated here because a tool can self-declare it, and it is reserved for
    // distribution-trusted tools.
    // Without this gate a Community `tool.toml` declaring `risk_class =
    // "interactive"` and self-signed with its own generated key (there is no
    // trusted-key allowlist) would be auto-approved on every call, and the
    // install prompt never shows risk_class for the operator to catch it.
    if manifest.risk_class == agentos_types::RiskClass::Interactive
        && info.trust_tier != TrustTier::Core
    {
        return Err(AgentOSError::ToolBlocked {
            name: info.name.clone(),
        });
    }

    // `WriteAgentState` is Core-only for the same reason, one step further: it
    // is auto-allowed under the default `ask_edit` AND it suppresses the legacy
    // `risk_classifier` backstop in the task executor. A self-signed Community
    // manifest claiming it would run writes unattended on both counts. Every
    // shipped user of the class is a `tools/core` manifest, so this costs
    // nothing today and closes the self-declaration path.
    if manifest.risk_class == agentos_types::RiskClass::WriteAgentState
        && info.trust_tier != TrustTier::Core
    {
        return Err(AgentOSError::ToolBlocked {
            name: info.name.clone(),
        });
    }

    // Per-action overrides may name `ReadonlyExternal` and nothing else. They
    // exist to stop a read action inheriting a write action's prompt — not to
    // reclassify a tool.
    //
    // `ExecCapable` is permitted alongside it, and is strictly MORE friction
    // than `ReadonlyExternal` at every mode (`auto` → Allow, `ask_edit` →
    // Prompt, `ask_always` → Prompt, `deny` → Deny). It is what a hardware
    // action that changes host state but is not kernel admin should resolve
    // to — `audio` `speak`/`playback` are control_plane at the tool level, and
    // ControlPlane is the non-overridable floor the standing-grant matcher is
    // never consulted for, so without this override every "say it out loud"
    // re-prompts and "approve & remember" can mint nothing (2026-09-20).
    //
    // `ReadonlyScoped` is excluded even though it reads as the *safer* label:
    // `ApprovalMode::decide` short-circuits it to `Allow` before it ever looks
    // at the mode, so it is allowed under `deny` too — the same strength as
    // `Interactive`, which the gate above reserves for `Core`. An override is
    // meant to lower friction, and `ReadonlyExternal` does exactly that
    // (allowed under `auto`/`ask_edit`, still prompts under `ask_always`, still
    // denied under `deny`) without punching through an operator's `deny`.
    //
    // Without the bound, `[risk_class_by_action] connect = "readonly_scoped"`
    // on a self-signed manifest would auto-approve under every mode while every
    // discovery surface still reported the tool-level `control_plane` — a
    // bypass hidden behind an accurate-looking label, through a field the two
    // Core-only gates above never inspect. Checked before the tier match so it
    // binds `Core` manifests too, which skip the signature check entirely.
    if let Some((action, class)) = manifest.risk_class_by_action.iter().find(|(_, class)| {
        !matches!(
            class,
            agentos_types::RiskClass::ReadonlyExternal | agentos_types::RiskClass::ExecCapable
        )
    }) {
        tracing::error!(
            tool = %info.name,
            %action,
            ?class,
            "manifest risk_class_by_action may only name readonly_external or exec_capable"
        );
        return Err(AgentOSError::ToolBlocked {
            name: info.name.clone(),
        });
    }

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
        RiskClass, ToolExecutor, ToolManifest, ToolSandbox, TrustTier,
    };
    use ed25519_dalek::{Signer, SigningKey};

    fn make_manifest(trust_tier: TrustTier) -> ToolManifest {
        ToolManifest {
            manifest: ToolInfo {
                category: None,
                search_hints: vec![],
                name: "test-tool".into(),
                version: "1.0.0".into(),
                description: "Test".into(),
                author: "test-author".into(),
                checksum: None,
                author_pubkey: None,
                signature: None,
                trust_tier,
                tags: None,
                capability_tags: vec![],
                group: String::new(),
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
            payload_schema: None,
            examples: vec![],
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
            fallbacks: vec![],
            risk_class: RiskClass::ReadonlyScoped,
            risk_class_by_action: Default::default(),
            usage_hints: None,
            tags: vec![],
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

    /// The table lowers the approval class per action, so it must be inside the
    /// signature. Otherwise an author appends
    /// `[risk_class_by_action] <every action> = "readonly_scoped"` to a manifest
    /// already signed and shipped, and every call auto-approves under every
    /// mode with the signature still verifying.
    #[test]
    fn override_table_is_covered_by_the_signature() {
        let mut manifest = make_manifest(TrustTier::Community);
        let before = signing_payload(&manifest);
        // Empty table must not perturb the bytes, or every existing signature
        // breaks the moment this field ships.
        assert!(!String::from_utf8_lossy(&before).contains("risk_class_by_action"));

        manifest
            .risk_class_by_action
            .insert("list".into(), RiskClass::ReadonlyExternal);
        let after = signing_payload(&manifest);
        assert_ne!(
            before, after,
            "bolting on a table must invalidate the signature"
        );

        // And the class itself is signed, not just the presence of a key.
        manifest
            .risk_class_by_action
            .insert("list".into(), RiskClass::ReadonlyScoped);
        assert_ne!(after, signing_payload(&manifest));
    }

    /// `ReadonlyExternal` and `ExecCapable` are the only classes an override
    /// may name.
    ///
    /// `ReadonlyScoped` is the trap: it reads as the safer of the two read
    /// classes and is rejected precisely because it is not — it is `Allow` under
    /// `deny`, the same strength as `Interactive`, which the Core-only gate
    /// above exists to keep out of self-declared manifests.
    #[test]
    fn override_table_may_only_name_readonly_external_or_exec_capable() {
        use agentos_types::{ApprovalDecision, ApprovalMode};

        // The reason the bound is what it is, asserted rather than narrated.
        assert_eq!(
            ApprovalMode::Deny.decide(RiskClass::ReadonlyScoped),
            ApprovalDecision::Allow
        );
        assert_eq!(
            ApprovalMode::Deny.decide(RiskClass::ReadonlyExternal),
            ApprovalDecision::Deny
        );
        assert_eq!(
            ApprovalMode::AskAlways.decide(RiskClass::ReadonlyExternal),
            ApprovalDecision::Prompt
        );
        assert_eq!(
            ApprovalMode::AskEdit.decide(RiskClass::ReadonlyExternal),
            ApprovalDecision::Allow
        );

        // `ExecCapable` is permitted: at every mode it is at least as much
        // friction as `ReadonlyExternal`, which already is.
        assert_eq!(
            ApprovalMode::Deny.decide(RiskClass::ExecCapable),
            ApprovalDecision::Deny
        );
        assert_eq!(
            ApprovalMode::AskEdit.decide(RiskClass::ExecCapable),
            ApprovalDecision::Prompt
        );

        for class in [
            RiskClass::ReadonlyScoped,
            RiskClass::Interactive,
            RiskClass::WriteAgentState,
            RiskClass::WriteScoped,
            RiskClass::ControlPlane,
        ] {
            let mut manifest = make_manifest(TrustTier::Core);
            manifest
                .risk_class_by_action
                .insert("connect".into(), class.clone());
            assert!(
                verify_manifest(&manifest).is_err(),
                "{class:?} must be rejected in risk_class_by_action"
            );
        }

        let mut manifest = make_manifest(TrustTier::Core);
        manifest
            .risk_class_by_action
            .insert("status".into(), RiskClass::ReadonlyExternal);
        manifest
            .risk_class_by_action
            .insert("speak".into(), RiskClass::ExecCapable);
        assert!(verify_manifest(&manifest).is_ok());
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
    fn privileged_executor_requires_core_tier_and_control_plane_risk() {
        use agentos_types::ExecutorType;

        // Core + ControlPlane + Privileged → accepted
        let mut m = make_manifest(TrustTier::Core);
        m.executor.executor_type = ExecutorType::Privileged;
        m.risk_class = RiskClass::ControlPlane;
        assert!(verify_manifest(&m).is_ok());

        // Core + Privileged but wrong risk class → rejected
        let mut m = make_manifest(TrustTier::Core);
        m.executor.executor_type = ExecutorType::Privileged;
        m.risk_class = RiskClass::WriteScoped;
        assert!(matches!(
            verify_manifest(&m).unwrap_err(),
            AgentOSError::ToolBlocked { .. }
        ));

        // Community-tier Privileged → rejected even with valid signature
        let seed = [42u8; 32];
        let signing_key = SigningKey::from_bytes(&seed);
        let pubkey_hex = hex::encode(signing_key.verifying_key().to_bytes());
        let mut m = make_manifest(TrustTier::Community);
        m.manifest.author_pubkey = Some(pubkey_hex);
        m.risk_class = RiskClass::ControlPlane;
        m.executor.executor_type = ExecutorType::Privileged;
        let payload = signing_payload(&m);
        let sig = signing_key.sign(&payload);
        m.manifest.signature = Some(hex::encode(sig.to_bytes()));
        assert!(matches!(
            verify_manifest(&m).unwrap_err(),
            AgentOSError::ToolBlocked { .. }
        ));

        // Inline executor on a Community tool with control_plane risk → accepted
        // (the gate fires only on Privileged executor).
        let mut m = make_manifest(TrustTier::Community);
        m.manifest.author_pubkey = Some(hex::encode(
            SigningKey::from_bytes(&seed).verifying_key().to_bytes(),
        ));
        m.risk_class = RiskClass::ControlPlane;
        m.executor.executor_type = ExecutorType::Inline;
        let payload = signing_payload(&m);
        let sig = SigningKey::from_bytes(&seed).sign(&payload);
        m.manifest.signature = Some(hex::encode(sig.to_bytes()));
        assert!(verify_manifest(&m).is_ok());
    }

    #[test]
    fn write_agent_state_risk_class_requires_core_tier() {
        // Core + WriteAgentState → accepted (every `tools/core` user).
        let mut m = make_manifest(TrustTier::Core);
        m.risk_class = RiskClass::WriteAgentState;
        assert!(verify_manifest(&m).is_ok());

        // Community + WriteAgentState → rejected even with a valid
        // self-signature. The class is auto-allowed under the default
        // `ask_edit` and suppresses the executor's legacy risk backstop, so a
        // third-party manifest must never be able to claim it.
        let seed = [13u8; 32];
        let signing_key = SigningKey::from_bytes(&seed);
        let mut m = make_manifest(TrustTier::Community);
        m.manifest.author_pubkey = Some(hex::encode(signing_key.verifying_key().to_bytes()));
        m.risk_class = RiskClass::WriteAgentState;
        let sig = signing_key.sign(&signing_payload(&m));
        m.manifest.signature = Some(hex::encode(sig.to_bytes()));
        assert!(matches!(
            verify_manifest(&m).unwrap_err(),
            AgentOSError::ToolBlocked { .. }
        ));
    }

    #[test]
    fn interactive_risk_class_requires_core_tier() {
        // Core + Interactive → accepted (this is `ask-user`).
        let mut m = make_manifest(TrustTier::Core);
        m.risk_class = RiskClass::Interactive;
        assert!(verify_manifest(&m).is_ok());

        // Community + Interactive → rejected even with a valid self-signature.
        // `Interactive` is auto-allowed under every approval mode including
        // `deny`, so a third-party manifest must never be able to claim it.
        let seed = [11u8; 32];
        let signing_key = SigningKey::from_bytes(&seed);
        let mut m = make_manifest(TrustTier::Community);
        m.manifest.author_pubkey = Some(hex::encode(signing_key.verifying_key().to_bytes()));
        m.risk_class = RiskClass::Interactive;
        let sig = signing_key.sign(&signing_payload(&m));
        m.manifest.signature = Some(hex::encode(sig.to_bytes()));
        assert!(matches!(
            verify_manifest(&m).unwrap_err(),
            AgentOSError::ToolBlocked { .. }
        ));

        // Verified tier is not exempt either.
        let mut m = make_manifest(TrustTier::Verified);
        m.manifest.author_pubkey = Some(hex::encode(signing_key.verifying_key().to_bytes()));
        m.risk_class = RiskClass::Interactive;
        let sig = signing_key.sign(&signing_payload(&m));
        m.manifest.signature = Some(hex::encode(sig.to_bytes()));
        assert!(matches!(
            verify_manifest(&m).unwrap_err(),
            AgentOSError::ToolBlocked { .. }
        ));
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
    fn signed_risk_class_tampering_is_rejected() {
        let seed = [7u8; 32];
        let signing_key = SigningKey::from_bytes(&seed);
        let mut m = make_manifest(TrustTier::Community);
        m.manifest.author_pubkey = Some(hex::encode(signing_key.verifying_key().to_bytes()));
        m.risk_class = RiskClass::ExecCapable;
        let sig = signing_key.sign(&signing_payload(&m));
        m.manifest.signature = Some(hex::encode(sig.to_bytes()));

        // Valid as signed.
        assert!(verify_manifest(&m).is_ok());

        // Downgrading risk_class after signing must invalidate the signature —
        // risk_class is now bound into the signed payload.
        m.risk_class = RiskClass::ReadonlyScoped;
        assert!(matches!(
            verify_manifest(&m).unwrap_err(),
            AgentOSError::ToolSignatureInvalid { .. }
        ));
    }

    #[test]
    fn signed_trust_tier_tampering_is_rejected() {
        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        let mut m = make_manifest(TrustTier::Community);
        m.manifest.author_pubkey = Some(hex::encode(signing_key.verifying_key().to_bytes()));
        let sig = signing_key.sign(&signing_payload(&m));
        m.manifest.signature = Some(hex::encode(sig.to_bytes()));
        assert!(verify_manifest(&m).is_ok());

        // Promoting the tier after signing must invalidate the signature.
        m.manifest.trust_tier = TrustTier::Verified;
        assert!(matches!(
            verify_manifest(&m).unwrap_err(),
            AgentOSError::ToolSignatureInvalid { .. }
        ));
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
