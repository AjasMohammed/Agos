use agentos_types::{AgentID, AgentOSError};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

/// Lifecycle status of a hardware device (Spec §9).
///
/// State machine: Unknown → Pending → Approved | Quarantined
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceStatus {
    /// Registered and awaiting explicit approval before access is allowed.
    Pending,
    /// Explicitly approved for specific agents.
    Approved,
    /// Hard-denied for all agents.
    Quarantined,
}

/// A registered hardware device and its access policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceEntry {
    /// Unique device identifier, e.g. `"gpu:0"`, `"usb:1"`, `"cam:0"`.
    pub id: String,
    /// Human-readable device type, e.g. `"nvidia-rtx-4090"`, `"webcam"`.
    pub device_type: String,
    /// Current approval status.
    pub status: DeviceStatus,
    /// Set of agents that have been granted access to this device.
    /// Only meaningful when `status == Approved`.
    #[serde(default)]
    pub granted_to: HashSet<AgentID>,
    /// Agents explicitly denied from using this device while it remains available
    /// to other approved agents.
    #[serde(default)]
    pub denied_to: HashSet<AgentID>,
    /// When the device was first seen by the registry.
    pub first_seen: chrono::DateTime<chrono::Utc>,
    /// When the status last changed.
    pub status_changed_at: chrono::DateTime<chrono::Utc>,
}

/// Registry tracking hardware devices and per-agent access grants (Spec §9).
///
/// Newly registered devices typically start in `Pending` state and must be
/// explicitly approved via `approve_for_agent()` before any agent can use them.
pub struct HardwareRegistry {
    devices: RwLock<HashMap<String, DeviceEntry>>,
}

impl Default for HardwareRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl HardwareRegistry {
    pub fn new() -> Self {
        Self {
            devices: RwLock::new(HashMap::new()),
        }
    }

    /// Register a newly detected device with an explicit status.
    ///
    /// If the device is already known, its entry is not changed (idempotent).
    /// Returns `true` if this was a new device entry.
    pub fn register_device(
        &self,
        device_id: &str,
        device_type: &str,
        status: DeviceStatus,
    ) -> bool {
        let mut devices = self.devices.write().unwrap_or_else(|error| {
            tracing::warn!(
                error = %error,
                "Recovered from poisoned lock in hardware registry write path"
            );
            error.into_inner()
        });
        if devices.contains_key(device_id) {
            return false; // already known
        }
        let now = chrono::Utc::now();
        devices.insert(
            device_id.to_string(),
            DeviceEntry {
                id: device_id.to_string(),
                device_type: device_type.to_string(),
                status,
                granted_to: HashSet::new(),
                denied_to: HashSet::new(),
                first_seen: now,
                status_changed_at: now,
            },
        );
        true
    }

    /// Register a newly detected device in `Pending` state.
    pub fn register_pending_device(&self, device_id: &str, device_type: &str) -> bool {
        self.register_device(device_id, device_type, DeviceStatus::Pending)
    }

    /// Approve a quarantined device for a specific agent.
    ///
    /// Moves the device to `Approved` if not already, and adds the agent to the
    /// grant set. Returns `Err` if the device does not exist or is quarantined.
    pub fn approve_for_agent(
        &self,
        device_id: &str,
        agent_id: AgentID,
    ) -> Result<(), AgentOSError> {
        let mut devices = self.devices.write().unwrap_or_else(|error| {
            tracing::warn!(
                error = %error,
                "Recovered from poisoned lock in hardware registry write path"
            );
            error.into_inner()
        });
        let entry = devices.get_mut(device_id).ok_or_else(|| {
            AgentOSError::HalError(format!("Device '{}' not in registry", device_id))
        })?;

        if entry.status == DeviceStatus::Quarantined {
            return Err(AgentOSError::HalError(format!(
                "Device '{}' is quarantined — cannot approve",
                device_id
            )));
        }

        let preserve_global_approval =
            entry.status == DeviceStatus::Approved && entry.granted_to.is_empty();

        entry.status = DeviceStatus::Approved;
        if preserve_global_approval {
            entry.denied_to.remove(&agent_id);
            entry.status_changed_at = chrono::Utc::now();
            return Ok(());
        }

        entry.granted_to.insert(agent_id);
        entry.denied_to.remove(&agent_id);
        entry.status_changed_at = chrono::Utc::now();
        Ok(())
    }

    /// Revoke a specific agent's access to a device.
    ///
    /// If no agents remain after revocation, the device is moved back to
    /// `Pending` so a fresh approval flow is required.
    pub fn revoke_agent_access(&self, device_id: &str, agent_id: &AgentID) {
        let mut devices = self.devices.write().unwrap_or_else(|error| {
            tracing::warn!(
                error = %error,
                "Recovered from poisoned lock in hardware registry write path"
            );
            error.into_inner()
        });
        if let Some(entry) = devices.get_mut(device_id) {
            entry.granted_to.remove(agent_id);
            if entry.granted_to.is_empty() && entry.status == DeviceStatus::Approved {
                entry.status = DeviceStatus::Pending;
                entry.status_changed_at = chrono::Utc::now();
            }
        }
    }

    /// Quarantine a device for all agents. Clears any existing grants.
    ///
    /// Returns `Err` if the device is not in the registry.
    pub fn quarantine_device(&self, device_id: &str) -> Result<(), AgentOSError> {
        let mut devices = self.devices.write().unwrap_or_else(|error| {
            tracing::warn!(
                error = %error,
                "Recovered from poisoned lock in hardware registry write path"
            );
            error.into_inner()
        });
        let entry = devices.get_mut(device_id).ok_or_else(|| {
            AgentOSError::HalError(format!("Device '{}' not in registry", device_id))
        })?;
        entry.status = DeviceStatus::Quarantined;
        entry.granted_to.clear();
        entry.denied_to.clear();
        entry.status_changed_at = chrono::Utc::now();
        Ok(())
    }

    pub fn deny_for_agent(&self, device_id: &str, agent_id: AgentID) -> Result<(), AgentOSError> {
        let mut devices = self.devices.write().unwrap_or_else(|error| {
            tracing::warn!(
                error = %error,
                "Recovered from poisoned lock in hardware registry write path"
            );
            error.into_inner()
        });
        let entry = devices.get_mut(device_id).ok_or_else(|| {
            AgentOSError::HalError(format!("Device '{}' not in registry", device_id))
        })?;
        entry.denied_to.insert(agent_id);
        entry.status_changed_at = chrono::Utc::now();
        Ok(())
    }

    pub fn get_device(&self, device_id: &str) -> Option<DeviceEntry> {
        self.devices
            .read()
            .unwrap_or_else(|error| {
                tracing::warn!(
                    error = %error,
                    "Recovered from poisoned lock in hardware registry read path"
                );
                error.into_inner()
            })
            .get(device_id)
            .cloned()
    }

    pub fn get_device_status(&self, device_id: &str) -> Option<DeviceStatus> {
        self.get_device(device_id).map(|entry| entry.status)
    }

    pub fn set_device_status(
        &self,
        device_id: &str,
        status: DeviceStatus,
    ) -> Result<(), AgentOSError> {
        let mut devices = self.devices.write().unwrap_or_else(|error| {
            tracing::warn!(
                error = %error,
                "Recovered from poisoned lock in hardware registry write path"
            );
            error.into_inner()
        });
        let entry = devices.get_mut(device_id).ok_or_else(|| {
            AgentOSError::HalError(format!("Device '{}' not in registry", device_id))
        })?;
        entry.status = status;
        if entry.status != DeviceStatus::Approved {
            entry.granted_to.clear();
            entry.denied_to.clear();
        }
        entry.status_changed_at = chrono::Utc::now();
        Ok(())
    }

    /// Check whether a specific agent is allowed to access a device.
    ///
    /// Returns `Ok(())` if access is permitted, `Err` otherwise.
    pub fn check_access(&self, device_id: &str, agent_id: &AgentID) -> Result<(), AgentOSError> {
        let devices = self.devices.read().unwrap_or_else(|error| {
            tracing::warn!(
                error = %error,
                "Recovered from poisoned lock in hardware registry read path"
            );
            error.into_inner()
        });
        let entry = devices.get(device_id).ok_or_else(|| {
            AgentOSError::HalError(format!(
                "Device '{}' not in registry — access denied (quarantine unknown devices)",
                device_id
            ))
        })?;

        match entry.status {
            DeviceStatus::Pending => Err(AgentOSError::DeviceAccessPending {
                device_id: device_id.to_string(),
                escalation_id: "pending".to_string(),
            }),
            DeviceStatus::Quarantined => Err(AgentOSError::PermissionDenied {
                resource: device_id.to_string(),
                operation: "device_access".to_string(),
            }),
            DeviceStatus::Approved => {
                if entry.denied_to.contains(agent_id) {
                    return Err(AgentOSError::PermissionDenied {
                        resource: device_id.to_string(),
                        operation: "device_access".to_string(),
                    });
                }
                if entry.granted_to.is_empty() || entry.granted_to.contains(agent_id) {
                    Ok(())
                } else {
                    Err(AgentOSError::PermissionDenied {
                        resource: device_id.to_string(),
                        operation: "device_access".to_string(),
                    })
                }
            }
        }
    }

    /// List all device entries (for status reporting / CLI).
    pub fn list_devices(&self) -> Vec<DeviceEntry> {
        self.devices
            .read()
            .unwrap_or_else(|error| {
                tracing::warn!(
                    error = %error,
                    "Recovered from poisoned lock in hardware registry read path"
                );
                error.into_inner()
            })
            .values()
            .cloned()
            .collect()
    }

    /// List devices currently in quarantine (pending user approval).
    pub fn list_quarantined(&self) -> Vec<DeviceEntry> {
        self.devices
            .read()
            .unwrap_or_else(|error| {
                tracing::warn!(
                    error = %error,
                    "Recovered from poisoned lock in hardware registry read path"
                );
                error.into_inner()
            })
            .values()
            .filter(|e| e.status == DeviceStatus::Quarantined)
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_device_pending() {
        let reg = HardwareRegistry::new();
        let is_new = reg.register_pending_device("usb:1", "USB Storage 64GB");
        assert!(is_new);

        let devices = reg.list_devices();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].status, DeviceStatus::Pending);
    }

    #[test]
    fn test_register_pending_idempotent() {
        let reg = HardwareRegistry::new();
        assert!(reg.register_pending_device("gpu:0", "nvidia-rtx-4090"));
        assert!(!reg.register_pending_device("gpu:0", "nvidia-rtx-4090")); // second call: false
    }

    #[test]
    fn test_approve_grants_access() {
        let reg = HardwareRegistry::new();
        reg.register_pending_device("cam:0", "webcam");
        let agent = AgentID::new();

        reg.approve_for_agent("cam:0", agent).unwrap();

        let devices = reg.list_devices();
        assert_eq!(devices[0].status, DeviceStatus::Approved);
        assert!(reg.check_access("cam:0", &agent).is_ok());
    }

    #[test]
    fn test_pending_device_blocks_access() {
        let reg = HardwareRegistry::new();
        reg.register_pending_device("usb:1", "USB Storage");
        let agent = AgentID::new();

        let result = reg.check_access("usb:1", &agent);
        assert!(result.is_err());
    }

    #[test]
    fn test_unapproved_agent_blocked_on_approved_device() {
        let reg = HardwareRegistry::new();
        reg.register_pending_device("gpu:0", "GPU");
        let approved_agent = AgentID::new();
        let other_agent = AgentID::new();

        reg.approve_for_agent("gpu:0", approved_agent).unwrap();

        assert!(reg.check_access("gpu:0", &approved_agent).is_ok());
        assert!(reg.check_access("gpu:0", &other_agent).is_err());
    }

    #[test]
    fn test_quarantine_blocks_all_agents() {
        let reg = HardwareRegistry::new();
        reg.register_pending_device("mic:0", "Microphone");
        let agent = AgentID::new();
        reg.approve_for_agent("mic:0", agent).unwrap();

        reg.quarantine_device("mic:0").unwrap();

        assert!(reg.check_access("mic:0", &agent).is_err());
        let devices = reg.list_devices();
        assert_eq!(devices[0].status, DeviceStatus::Quarantined);
    }

    #[test]
    fn test_revoke_drops_to_pending_when_last_agent() {
        let reg = HardwareRegistry::new();
        reg.register_pending_device("gpu:0", "GPU");
        let agent = AgentID::new();
        reg.approve_for_agent("gpu:0", agent).unwrap();

        reg.revoke_agent_access("gpu:0", &agent);

        let devices = reg.list_devices();
        assert_eq!(devices[0].status, DeviceStatus::Pending);
    }

    #[test]
    fn test_unknown_device_access_denied() {
        let reg = HardwareRegistry::new();
        let result = reg.check_access("usb:999", &AgentID::new());
        assert!(result.is_err());
    }

    #[test]
    fn test_agent_specific_deny_preserves_other_grants() {
        let reg = HardwareRegistry::new();
        let approved_agent = AgentID::new();
        let denied_agent = AgentID::new();
        reg.register_pending_device("gpu:0", "GPU");
        reg.approve_for_agent("gpu:0", approved_agent).unwrap();
        reg.deny_for_agent("gpu:0", denied_agent).unwrap();

        assert!(reg.check_access("gpu:0", &approved_agent).is_ok());
        assert!(reg.check_access("gpu:0", &denied_agent).is_err());
    }

    #[test]
    fn test_approve_preserves_global_approval_semantics() {
        let reg = HardwareRegistry::new();
        let first_agent = AgentID::new();
        let second_agent = AgentID::new();
        reg.register_device("storage:sda", "block-device", DeviceStatus::Approved);

        reg.approve_for_agent("storage:sda", first_agent).unwrap();

        assert!(reg.check_access("storage:sda", &first_agent).is_ok());
        assert!(reg.check_access("storage:sda", &second_agent).is_ok());
    }
}
