//! Registry of all capability providers, keyed by domain name.
//!
//! The registry is initialized at kernel boot and populated with providers
//! for each managed capability domain (environments, processes, networking,
//! builds, storage). Tools look up providers by domain name to delegate
//! execution through the kernel's mediation layer.

use crate::capability_provider::CapabilityProvider;
use agentos_types::{AgentOSError, CapabilityDescriptorSummary};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Registry of all capability providers, keyed by domain name.
///
/// Uses `BTreeMap` for deterministic iteration order (domains are listed
/// alphabetically when exposed to agents or CLI). Thread-safe via
/// `Arc<dyn CapabilityProvider>` — providers are immutable after registration.
/// The registry itself is wrapped in `Arc<RwLock<_>>` at the kernel level to
/// allow late registration during boot, but is effectively read-only after
/// the boot sequence completes.
pub struct CapabilityRegistry {
    providers: BTreeMap<String, Arc<dyn CapabilityProvider>>,
}

impl CapabilityRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            providers: BTreeMap::new(),
        }
    }

    /// Register a capability provider for a domain.
    ///
    /// Domain names must be non-empty and consist only of ASCII alphanumeric
    /// characters, hyphens, and underscores. Returns an error if the domain
    /// name is invalid or already registered.
    pub fn register(&mut self, provider: Arc<dyn CapabilityProvider>) -> Result<(), AgentOSError> {
        let domain = provider.domain().to_string();

        // Validate domain name: must be 1-64 chars, ASCII alphanumeric plus
        // hyphens/underscores. Dots and slashes are rejected to prevent
        // permission-prefix shadowing (e.g., "env.install" as a domain name).
        const MAX_DOMAIN_LEN: usize = 64;
        if domain.is_empty()
            || domain.len() > MAX_DOMAIN_LEN
            || !domain
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(AgentOSError::KernelError {
                reason: format!(
                    "invalid capability domain name '{domain}': \
                     must be non-empty ASCII alphanumeric with hyphens/underscores"
                ),
            });
        }

        if self.providers.contains_key(&domain) {
            return Err(AgentOSError::KernelError {
                reason: format!("capability domain '{domain}' already registered"),
            });
        }

        tracing::info!(domain = %domain, "registered capability provider");
        self.providers.insert(domain, provider);
        Ok(())
    }

    /// Look up a provider by domain name.
    pub fn get(&self, domain: &str) -> Option<&Arc<dyn CapabilityProvider>> {
        self.providers.get(domain)
    }

    /// List all registered domain names (sorted alphabetically).
    pub fn domains(&self) -> Vec<&str> {
        self.providers.keys().map(|s| s.as_str()).collect()
    }

    /// Check whether a domain is registered.
    pub fn has_domain(&self, domain: &str) -> bool {
        self.providers.contains_key(domain)
    }

    /// Return descriptors for all registered providers.
    ///
    /// Used for agent tool discovery and manual generation. Returns descriptors
    /// in deterministic alphabetical order by domain name.
    pub fn list_capabilities(&self) -> Vec<CapabilityDescriptorSummary> {
        self.providers
            .values()
            .map(|p| CapabilityDescriptorSummary {
                domain: p.domain().to_string(),
                actions: p
                    .supported_actions()
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
                description: p.description().to_string(),
            })
            .collect()
    }

    /// Number of registered providers.
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    /// Whether the registry has no providers.
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

impl Default for CapabilityRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability_provider::{CapabilityContext, CapabilityResult};
    use async_trait::async_trait;
    use serde_json::{json, Value};

    struct TestProvider {
        domain_name: &'static str,
    }

    #[async_trait]
    impl CapabilityProvider for TestProvider {
        fn domain(&self) -> &str {
            self.domain_name
        }

        fn supported_actions(&self) -> &[&str] {
            &["action-a", "action-b"]
        }

        fn required_permissions(
            &self,
            _action: &str,
        ) -> Option<Vec<(String, agentos_types::PermissionOp)>> {
            Some(vec![])
        }

        async fn execute(
            &self,
            _action: &str,
            _params: Value,
            _context: &CapabilityContext,
        ) -> Result<CapabilityResult, AgentOSError> {
            Ok(CapabilityResult {
                output: json!({}),
                audit_metadata: json!({}),
            })
        }

        fn description(&self) -> &str {
            "test provider"
        }
    }

    #[test]
    fn register_and_lookup() {
        let mut reg = CapabilityRegistry::new();
        let provider = Arc::new(TestProvider { domain_name: "env" });
        reg.register(provider).unwrap();

        assert!(reg.has_domain("env"));
        assert!(!reg.has_domain("proc"));
        assert_eq!(reg.len(), 1);
        assert!(!reg.is_empty());

        let p = reg.get("env").unwrap();
        assert_eq!(p.domain(), "env");
    }

    #[test]
    fn duplicate_registration_returns_error() {
        let mut reg = CapabilityRegistry::new();
        let p1 = Arc::new(TestProvider { domain_name: "env" });
        let p2 = Arc::new(TestProvider { domain_name: "env" });

        reg.register(p1).unwrap();
        let err = reg.register(p2).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("already registered"));
    }

    #[test]
    fn get_unknown_domain_returns_none() {
        let reg = CapabilityRegistry::new();
        assert!(reg.get("nonexistent").is_none());
    }

    #[test]
    fn list_capabilities_returns_descriptors() {
        let mut reg = CapabilityRegistry::new();
        reg.register(Arc::new(TestProvider { domain_name: "env" }))
            .unwrap();
        reg.register(Arc::new(TestProvider {
            domain_name: "proc",
        }))
        .unwrap();

        let caps = reg.list_capabilities();
        assert_eq!(caps.len(), 2);
        // BTreeMap: sorted alphabetically
        assert_eq!(caps[0].domain, "env");
        assert_eq!(caps[1].domain, "proc");
        assert_eq!(caps[0].actions, vec!["action-a", "action-b"]);
        assert_eq!(caps[0].description, "test provider");
    }

    #[test]
    fn domains_lists_all_sorted() {
        let mut reg = CapabilityRegistry::new();
        reg.register(Arc::new(TestProvider { domain_name: "net" }))
            .unwrap();
        reg.register(Arc::new(TestProvider {
            domain_name: "build",
        }))
        .unwrap();

        // BTreeMap guarantees sorted order
        assert_eq!(reg.domains(), vec!["build", "net"]);
    }

    #[test]
    fn empty_registry() {
        let reg = CapabilityRegistry::new();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
        assert!(reg.domains().is_empty());
        assert!(reg.list_capabilities().is_empty());
    }

    #[test]
    fn rejects_empty_domain_name() {
        let mut reg = CapabilityRegistry::new();
        // Create a provider with empty domain via a custom struct
        struct EmptyDomainProvider;
        #[async_trait]
        impl CapabilityProvider for EmptyDomainProvider {
            fn domain(&self) -> &str {
                ""
            }
            fn supported_actions(&self) -> &[&str] {
                &[]
            }
            fn required_permissions(
                &self,
                _: &str,
            ) -> Option<Vec<(String, agentos_types::PermissionOp)>> {
                None
            }
            async fn execute(
                &self,
                _: &str,
                _: Value,
                _: &CapabilityContext,
            ) -> Result<CapabilityResult, AgentOSError> {
                unreachable!()
            }
            fn description(&self) -> &str {
                ""
            }
        }

        let err = reg.register(Arc::new(EmptyDomainProvider)).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("invalid capability domain name"));
    }

    #[test]
    fn rejects_domain_with_dots() {
        let mut reg = CapabilityRegistry::new();
        struct DottedProvider;
        #[async_trait]
        impl CapabilityProvider for DottedProvider {
            fn domain(&self) -> &str {
                "env.install"
            }
            fn supported_actions(&self) -> &[&str] {
                &[]
            }
            fn required_permissions(
                &self,
                _: &str,
            ) -> Option<Vec<(String, agentos_types::PermissionOp)>> {
                None
            }
            async fn execute(
                &self,
                _: &str,
                _: Value,
                _: &CapabilityContext,
            ) -> Result<CapabilityResult, AgentOSError> {
                unreachable!()
            }
            fn description(&self) -> &str {
                ""
            }
        }

        let err = reg.register(Arc::new(DottedProvider)).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("invalid capability domain name"));
    }
}
