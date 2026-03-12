use agentos_types::{AgentID, AgentOSError};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

/// Lifecycle status of a hardware device (Spec §9).
///
/// State machine: Unknown → Quarantined → Approved | Denied
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceStatus {
    /// Newly connected — not yet reviewed.
    Quarantined,
    /// Explicitly approved for specific agents.
    Approved,
    /// Denied for all agents.
    Denied,
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
    pub granted_to: HashSet<AgentID>,
    /// When the device was first seen by the registry.
    pub first_seen: chrono::DateTime<chrono::Utc>,
    /// When the status last changed.
    pub status_changed_at: chrono::DateTime<chrono::Utc>,
}

/// Registry tracking hardware devices and per-agent access grants (Spec §9).
///
/// All newly connected devices start in `Quarantined` state and must be
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

    /// Register a newly detected device in Quarantined state.
    ///
    /// If the device is already known, its entry is not changed (idempotent).
    /// Returns `true` if this was a new device that just entered quarantine.
    pub fn quarantine_device(&self, device_id: &str, device_type: &str) -> bool {
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
                status: DeviceStatus::Quarantined,
                granted_to: HashSet::new(),
                first_seen: now,
                status_changed_at: now,
            },
        );
        true
    }

    /// Approve a quarantined device for a specific agent.
    ///
    /// Moves the device to `Approved` if not already, and adds the agent to the
    /// grant set. Returns `Err` if the device does not exist or is `Denied`.
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

        if entry.status == DeviceStatus::Denied {
            return Err(AgentOSError::HalError(format!(
                "Device '{}' is denied — cannot approve",
                device_id
            )));
        }

        entry.status = DeviceStatus::Approved;
        entry.granted_to.insert(agent_id);
        entry.status_changed_at = chrono::Utc::now();
        Ok(())
    }

    /// Revoke a specific agent's access to a device.
    ///
    /// If no agents remain after revocation, the device is moved back to
    /// `Quarantined` so a fresh approval flow is required.
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
                entry.status = DeviceStatus::Quarantined;
                entry.status_changed_at = chrono::Utc::now();
            }
        }
    }

    /// Deny a device for all agents. Clears any existing grants.
    ///
    /// Returns `Err` if the device is not in the registry.
    pub fn deny_device(&self, device_id: &str) -> Result<(), AgentOSError> {
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
        entry.status = DeviceStatus::Denied;
        entry.granted_to.clear();
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
            DeviceStatus::Quarantined => Err(AgentOSError::PermissionDenied {
                resource: device_id.to_string(),
                operation: "device_access".to_string(),
            }),
            DeviceStatus::Denied => Err(AgentOSError::PermissionDenied {
                resource: device_id.to_string(),
                operation: "device_access".to_string(),
            }),
            DeviceStatus::Approved => {
                if entry.granted_to.contains(agent_id) {
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
    fn test_new_device_quarantined() {
        let reg = HardwareRegistry::new();
        let is_new = reg.quarantine_device("usb:1", "USB Storage 64GB");
        assert!(is_new);

        let devices = reg.list_quarantined();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].status, DeviceStatus::Quarantined);
    }

    #[test]
    fn test_quarantine_idempotent() {
        let reg = HardwareRegistry::new();
        assert!(reg.quarantine_device("gpu:0", "nvidia-rtx-4090"));
        assert!(!reg.quarantine_device("gpu:0", "nvidia-rtx-4090")); // second call: false
    }

    #[test]
    fn test_approve_grants_access() {
        let reg = HardwareRegistry::new();
        reg.quarantine_device("cam:0", "webcam");
        let agent = AgentID::new();

        reg.approve_for_agent("cam:0", agent).unwrap();

        let devices = reg.list_devices();
        assert_eq!(devices[0].status, DeviceStatus::Approved);
        assert!(reg.check_access("cam:0", &agent).is_ok());
    }

    #[test]
    fn test_quarantined_device_blocks_access() {
        let reg = HardwareRegistry::new();
        reg.quarantine_device("usb:1", "USB Storage");
        let agent = AgentID::new();

        let result = reg.check_access("usb:1", &agent);
        assert!(result.is_err());
    }

    #[test]
    fn test_unapproved_agent_blocked_on_approved_device() {
        let reg = HardwareRegistry::new();
        reg.quarantine_device("gpu:0", "GPU");
        let approved_agent = AgentID::new();
        let other_agent = AgentID::new();

        reg.approve_for_agent("gpu:0", approved_agent).unwrap();

        assert!(reg.check_access("gpu:0", &approved_agent).is_ok());
        assert!(reg.check_access("gpu:0", &other_agent).is_err());
    }

    #[test]
    fn test_deny_blocks_all_agents() {
        let reg = HardwareRegistry::new();
        reg.quarantine_device("mic:0", "Microphone");
        let agent = AgentID::new();
        reg.approve_for_agent("mic:0", agent).unwrap();

        reg.deny_device("mic:0").unwrap();

        assert!(reg.check_access("mic:0", &agent).is_err());
        let devices = reg.list_devices();
        assert_eq!(devices[0].status, DeviceStatus::Denied);
    }

    #[test]
    fn test_revoke_drops_to_quarantine_when_last_agent() {
        let reg = HardwareRegistry::new();
        reg.quarantine_device("gpu:0", "GPU");
        let agent = AgentID::new();
        reg.approve_for_agent("gpu:0", agent).unwrap();

        reg.revoke_agent_access("gpu:0", &agent);

        let devices = reg.list_devices();
        assert_eq!(devices[0].status, DeviceStatus::Quarantined);
    }

    #[test]
    fn test_unknown_device_access_denied() {
        let reg = HardwareRegistry::new();
        let result = reg.check_access("usb:999", &AgentID::new());
        assert!(result.is_err());
    }
}
