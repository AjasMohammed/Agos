//! Operator-driven connector manifest lifecycle (register / remove).
//!
//! Manifests live in `<data_dir>/connectors/<id>.toml` — the same directory
//! the kernel loads at boot — so a connector added here survives restarts.

use agentos_audit::{AuditEntry, AuditEventType, AuditSeverity};
use agentos_connectors::ConnectorManifest;
use agentos_types::TraceID;
use std::path::PathBuf;

use crate::kernel::Kernel;
use crate::plugin_registry::valid_plugin_id;

impl Kernel {
    /// Directory the boot loader scans for connector manifests.
    pub fn connectors_dir(&self) -> PathBuf {
        self.data_dir.join("connectors")
    }

    /// Directory operator-installed plugin manifests live in — the `plugins/user`
    /// half of what `Kernel::boot` discovers (`<data_dir>/../plugins/{core,user}`).
    pub fn user_plugins_dir(&self) -> PathBuf {
        self.data_dir
            .parent()
            .unwrap_or(&self.data_dir)
            .join("plugins")
            .join("user")
    }

    /// Parse, validate, persist and register a connector manifest.
    /// Errors are operator-facing strings; the caller maps them to HTTP.
    ///
    /// `replace_id` turns this into an edit in place: the named connector's
    /// registration is dropped first and its stored OAuth credential is left
    /// alone, so fixing a typo in a manifest does not cost a re-authorisation.
    pub async fn install_connector_manifest(
        &self,
        manifest_toml: &str,
        replace_id: Option<&str>,
    ) -> Result<ConnectorManifest, String> {
        let manifest: ConnectorManifest = toml::from_str(manifest_toml)
            .map_err(|e| format!("Invalid connector manifest: {e}"))?;
        let id = manifest.connector.id.clone();
        if !valid_plugin_id(&id) {
            return Err(format!(
                "Invalid connector id '{id}': use letters, digits, '-' or '_' (max 64)"
            ));
        }
        // The id being edited is the identity: a manifest that renames it would
        // silently create a second connector and orphan the one on screen.
        if let Some(target) = replace_id {
            if target != id {
                return Err(format!(
                    "Manifest id '{id}' does not match connector '{target}' — remove it and add the new one instead"
                ));
            }
        }
        match (
            self.connector_registry.has_connector(&id).await,
            replace_id.is_some(),
        ) {
            (true, false) => return Err(format!("Connector '{id}' is already registered")),
            (false, true) => return Err(format!("Connector '{id}' is not registered")),
            // ponytail: `register` is an overwriting insert, so a replace needs
            // no deregister first. Dropping it also closes the window where a
            // failed file write left the connector unregistered *and*
            // un-editable (the replace path then 404s on its own id).
            _ => {}
        }

        let dir = self.connectors_dir();
        let path = dir.join(format!("{id}.toml"));
        let body = manifest_toml.to_string();
        tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&dir)?;
            std::fs::write(&path, body)
        })
        .await
        .map_err(|e| format!("write task failed: {e}"))?
        .map_err(|e| format!("Failed to write connector manifest: {e}"))?;

        self.connector_registry
            .register(manifest.clone())
            .await
            .map_err(|e| format!("Failed to register connector: {e}"))?;

        let _ = self.audit.append(AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
            event_type: AuditEventType::ConnectorRegistered,
            agent_id: None,
            task_id: None,
            tool_id: None,
            details: serde_json::json!({
                "connector_id": id,
                "tools": manifest.tools.len(),
                "base_url": manifest.connector.base_url,
            }),
            severity: AuditSeverity::Info,
            reversible: true,
            rollback_ref: None,
        });
        Ok(manifest)
    }

    /// Deregister a connector, drop its stored credential and delete its
    /// manifest file. Idempotent per step — a connector that exists only as a
    /// stored credential (e.g. a manual token) is still cleaned up.
    pub async fn remove_connector_manifest(&self, id: &str) -> Result<(), String> {
        if !valid_plugin_id(id) {
            return Err(format!("Invalid connector id '{id}'"));
        }
        let registered = self.connector_registry.has_connector(id).await;
        let had_cred = self
            .vault
            .oauth_store()
            .list()
            .await
            .unwrap_or_default()
            .iter()
            .any(|c| c.connector_id == id);
        let path = self.connectors_dir().join(format!("{id}.toml"));
        let file_exists = path.exists();
        if !registered && !had_cred && !file_exists {
            return Err(format!("Connector '{id}' not found"));
        }

        if had_cred {
            if let Err(e) = self.vault.oauth_store().delete(id).await {
                tracing::warn!(connector_id = %id, error = %e, "OAuth credential delete failed");
            }
        }
        if registered {
            if let Err(e) = self.connector_registry.deregister(id).await {
                tracing::warn!(connector_id = %id, error = %e, "connector deregister failed");
            }
        }
        if file_exists {
            tokio::task::spawn_blocking(move || std::fs::remove_file(&path))
                .await
                .map_err(|e| format!("remove task failed: {e}"))?
                .map_err(|e| format!("Failed to delete connector manifest: {e}"))?;
        }

        let _ = self.audit.append(AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
            event_type: AuditEventType::ConnectorRemoved,
            agent_id: None,
            task_id: None,
            tool_id: None,
            details: serde_json::json!({ "connector_id": id }),
            severity: AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        });
        Ok(())
    }
}
