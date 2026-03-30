use std::collections::HashMap;

use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::{json, Value};
use zbus::fdo::ObjectManagerProxy;
use zbus::names::OwnedInterfaceName;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Str};

use crate::hal::HalDriver;

const UDISKS2_SERVICE: &str = "org.freedesktop.UDisks2";
const UDISKS2_ROOT_PATH: &str = "/org/freedesktop/UDisks2";
const FILESYSTEM_INTERFACE: &str = "org.freedesktop.UDisks2.Filesystem";
const BLOCK_INTERFACE: &str = "org.freedesktop.UDisks2.Block";
const DRIVE_INTERFACE: &str = "org.freedesktop.UDisks2.Drive";
const DBUS_PROPERTIES_INTERFACE: &str = "org.freedesktop.DBus.Properties";
const SAFE_MOUNT_OPTIONS: &str = "nosuid,noexec,nodev";

/// UDisks2-backed driver for removable filesystem operations.
pub struct UsbStorageDriver;

struct UsbDriveIdentity {
    block_path: OwnedObjectPath,
    drive_path: OwnedObjectPath,
}

impl Default for UsbStorageDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl UsbStorageDriver {
    pub fn new() -> Self {
        Self
    }

    fn validate_device_name<'a>(&self, device: &'a str) -> Result<&'a str, AgentOSError> {
        if device.is_empty() {
            return Err(AgentOSError::HalError("Missing 'device' param".into()));
        }

        if device.contains("..") {
            return Err(AgentOSError::HalError(
                "Invalid device name: path traversal rejected".into(),
            ));
        }

        if !device
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return Err(AgentOSError::HalError(
                "Invalid device name; use a UDisks2 block device id such as 'sdb1'".into(),
            ));
        }

        Ok(device)
    }

    fn device_from_params<'a>(&self, params: &'a Value) -> Result<&'a str, AgentOSError> {
        let device = params
            .get("device")
            .and_then(Value::as_str)
            .ok_or_else(|| AgentOSError::HalError("Missing 'device' param".into()))?;

        self.validate_device_name(device)
    }

    fn block_object_path(&self, device: &str) -> Result<OwnedObjectPath, AgentOSError> {
        OwnedObjectPath::try_from(format!("{UDISKS2_ROOT_PATH}/block_devices/{device}"))
            .map_err(|e| AgentOSError::HalError(format!("Invalid block object path: {e}")))
    }

    fn empty_options(&self) -> HashMap<&'static str, OwnedValue> {
        HashMap::new()
    }

    fn mount_options(&self) -> HashMap<&'static str, OwnedValue> {
        HashMap::from([("options", OwnedValue::from(Str::from(SAFE_MOUNT_OPTIONS)))])
    }

    async fn system_connection(&self) -> Result<zbus::Connection, AgentOSError> {
        zbus::Connection::system()
            .await
            .map_err(|e| AgentOSError::HalError(format!("D-Bus connect failed: {e}")))
    }

    async fn get_drive_path(
        &self,
        conn: &zbus::Connection,
        block_path: &OwnedObjectPath,
    ) -> Result<OwnedObjectPath, AgentOSError> {
        let reply = conn
            .call_method(
                Some(UDISKS2_SERVICE),
                block_path.clone(),
                Some(DBUS_PROPERTIES_INTERFACE),
                "Get",
                &(BLOCK_INTERFACE, "Drive"),
            )
            .await
            .map_err(|e| {
                AgentOSError::HalError(format!("Failed to resolve drive for device: {e}"))
            })?;

        let value = reply
            .body()
            .deserialize::<OwnedValue>()
            .map_err(|e| AgentOSError::HalError(format!("Invalid drive property response: {e}")))?;

        value.try_into().map_err(|e| {
            AgentOSError::HalError(format!("Drive property was not an object path: {e}"))
        })
    }

    async fn get_property<T>(
        &self,
        conn: &zbus::Connection,
        object_path: &OwnedObjectPath,
        interface: &str,
        property: &str,
    ) -> Result<T, AgentOSError>
    where
        T: TryFrom<OwnedValue>,
        <T as TryFrom<OwnedValue>>::Error: std::fmt::Display,
    {
        let reply = conn
            .call_method(
                Some(UDISKS2_SERVICE),
                object_path.clone(),
                Some(DBUS_PROPERTIES_INTERFACE),
                "Get",
                &(interface, property),
            )
            .await
            .map_err(|e| {
                AgentOSError::HalError(format!(
                    "Failed to read {interface}.{property} from UDisks2: {e}"
                ))
            })?;

        let value = reply.body().deserialize::<OwnedValue>().map_err(|e| {
            AgentOSError::HalError(format!(
                "Invalid {interface}.{property} response from UDisks2: {e}"
            ))
        })?;

        value.try_into().map_err(|e| {
            AgentOSError::HalError(format!(
                "Unexpected type for {interface}.{property} from UDisks2: {e}"
            ))
        })
    }

    async fn ensure_usb_device(
        &self,
        conn: &zbus::Connection,
        device: &str,
    ) -> Result<UsbDriveIdentity, AgentOSError> {
        let block_path = self.block_object_path(device)?;
        let drive_path = self.get_drive_path(conn, &block_path).await?;
        let connection_bus: String = self
            .get_property(conn, &drive_path, DRIVE_INTERFACE, "ConnectionBus")
            .await?;

        if !connection_bus.eq_ignore_ascii_case("usb") {
            return Err(AgentOSError::HalError(format!(
                "Device '{device}' is not a USB-backed drive"
            )));
        }

        Ok(UsbDriveIdentity {
            block_path,
            drive_path,
        })
    }

    fn is_usb_filesystem(
        &self,
        all_objects: &HashMap<
            OwnedObjectPath,
            HashMap<OwnedInterfaceName, HashMap<String, OwnedValue>>,
        >,
        interfaces: &HashMap<OwnedInterfaceName, HashMap<String, OwnedValue>>,
    ) -> bool {
        let Some(block) = interfaces.get(BLOCK_INTERFACE) else {
            return false;
        };

        let Some(drive_path): Option<OwnedObjectPath> = block
            .get("Drive")
            .and_then(|value| value.clone().try_into().ok())
        else {
            return false;
        };

        all_objects
            .get(&drive_path)
            .and_then(|drive_interfaces| drive_interfaces.get(DRIVE_INTERFACE))
            .and_then(|props| props.get("ConnectionBus"))
            .and_then(|value| {
                let bus: Result<String, _> = value.clone().try_into();
                bus.ok()
            })
            .is_some_and(|bus| bus.eq_ignore_ascii_case("usb"))
    }

    fn filesystem_entry_from_object(
        &self,
        object_path: &OwnedObjectPath,
        interfaces: &HashMap<OwnedInterfaceName, HashMap<String, OwnedValue>>,
    ) -> Value {
        let device = object_path
            .as_str()
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_string();

        let block = interfaces.get(BLOCK_INTERFACE);
        let filesystem = interfaces.get(FILESYSTEM_INTERFACE);

        let id_label: String = block
            .and_then(|props| props.get("IdLabel"))
            .and_then(|value| value.clone().try_into().ok())
            .unwrap_or_default();
        let id_type: String = block
            .and_then(|props| props.get("IdType"))
            .and_then(|value| value.clone().try_into().ok())
            .unwrap_or_default();
        let id_uuid: String = block
            .and_then(|props| props.get("IdUUID"))
            .and_then(|value| value.clone().try_into().ok())
            .unwrap_or_default();
        let read_only: bool = block
            .and_then(|props| props.get("ReadOnly"))
            .and_then(|value| value.clone().try_into().ok())
            .unwrap_or(false);
        let mount_points = filesystem
            .and_then(|props| props.get("MountPoints"))
            .map(Self::decode_mount_points)
            .unwrap_or_default();

        json!({
            "device": device,
            "object_path": object_path.as_str(),
            "id_label": id_label,
            "id_type": id_type,
            "id_uuid": id_uuid,
            "read_only": read_only,
            "mount_points": mount_points,
            "mounted": !mount_points.is_empty(),
        })
    }

    fn decode_mount_points(value: &OwnedValue) -> Vec<String> {
        let mounts: Result<Vec<Vec<u8>>, _> = value.clone().try_into();
        mounts
            .unwrap_or_default()
            .into_iter()
            .filter_map(|mount| {
                let trimmed = mount
                    .into_iter()
                    .take_while(|byte| *byte != 0)
                    .collect::<Vec<_>>();
                if trimmed.is_empty() {
                    None
                } else {
                    String::from_utf8(trimmed).ok()
                }
            })
            .collect()
    }

    async fn mount_device(&self, params: &Value) -> Result<Value, AgentOSError> {
        let device = self.device_from_params(params)?;
        let conn = self.system_connection().await?;
        let identity = self.ensure_usb_device(&conn, device).await?;

        let reply = conn
            .call_method(
                Some(UDISKS2_SERVICE),
                identity.block_path.clone(),
                Some(FILESYSTEM_INTERFACE),
                "Mount",
                &(self.mount_options(),),
            )
            .await
            .map_err(|e| AgentOSError::HalError(format!("Mount failed: {e}")))?;

        let mount_path = reply
            .body()
            .deserialize::<String>()
            .map_err(|e| AgentOSError::HalError(format!("Invalid mount response: {e}")))?;

        Ok(json!({
            "mounted": true,
            "device": device,
            "mount_path": mount_path,
            "options": ["nosuid", "noexec", "nodev"],
        }))
    }

    async fn unmount_device(&self, params: &Value) -> Result<Value, AgentOSError> {
        let device = self.device_from_params(params)?;
        let conn = self.system_connection().await?;
        let identity = self.ensure_usb_device(&conn, device).await?;

        conn.call_method(
            Some(UDISKS2_SERVICE),
            identity.block_path,
            Some(FILESYSTEM_INTERFACE),
            "Unmount",
            &(self.empty_options(),),
        )
        .await
        .map_err(|e| AgentOSError::HalError(format!("Unmount failed: {e}")))?;

        Ok(json!({
            "unmounted": true,
            "device": device,
        }))
    }

    async fn eject_device(&self, params: &Value) -> Result<Value, AgentOSError> {
        let device = self.device_from_params(params)?;
        let conn = self.system_connection().await?;
        let identity = self.ensure_usb_device(&conn, device).await?;

        conn.call_method(
            Some(UDISKS2_SERVICE),
            identity.drive_path,
            Some(DRIVE_INTERFACE),
            "PowerOff",
            &(self.empty_options(),),
        )
        .await
        .map_err(|e| AgentOSError::HalError(format!("Eject failed: {e}")))?;

        Ok(json!({
            "ejected": true,
            "device": device,
        }))
    }

    async fn list_filesystems(&self) -> Result<Value, AgentOSError> {
        let conn = self.system_connection().await?;
        let proxy = ObjectManagerProxy::builder(&conn)
            .destination(UDISKS2_SERVICE)
            .map_err(|e| AgentOSError::HalError(format!("Invalid UDisks2 destination: {e}")))?
            .path(UDISKS2_ROOT_PATH)
            .map_err(|e| {
                AgentOSError::HalError(format!("Invalid UDisks2 object manager path: {e}"))
            })?
            .build()
            .await
            .map_err(|e| {
                AgentOSError::HalError(format!(
                    "Failed to create UDisks2 object manager proxy: {e}"
                ))
            })?;

        let managed_objects = proxy
            .get_managed_objects()
            .await
            .map_err(|e| AgentOSError::HalError(format!("Failed to enumerate filesystems: {e}")))?;

        let filesystems = managed_objects
            .iter()
            .filter(|(_, interfaces)| interfaces.contains_key(FILESYSTEM_INTERFACE))
            .filter(|(_, interfaces)| self.is_usb_filesystem(&managed_objects, interfaces))
            .map(|(path, interfaces)| self.filesystem_entry_from_object(path, interfaces))
            .collect::<Vec<_>>();

        Ok(json!({ "filesystems": filesystems }))
    }
}

#[async_trait]
impl HalDriver for UsbStorageDriver {
    fn name(&self) -> &str {
        "usb-storage"
    }

    fn required_permission(&self) -> (&str, PermissionOp) {
        ("hardware.usb-storage", PermissionOp::Execute)
    }

    fn device_key(&self, params: &Value) -> Option<String> {
        params
            .get("device")
            .and_then(Value::as_str)
            .filter(|device| self.validate_device_name(device).is_ok())
            .map(|device| format!("usb-storage:{device}"))
    }

    async fn query(&self, params: Value) -> Result<Value, AgentOSError> {
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("list");

        match action {
            "mount" => self.mount_device(&params).await,
            "unmount" => self.unmount_device(&params).await,
            "eject" => self.eject_device(&params).await,
            "list" => self.list_filesystems().await,
            other => Err(AgentOSError::HalError(format!(
                "Unknown usb-storage action: {other}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_key_accepts_safe_block_device_ids() {
        let driver = UsbStorageDriver::new();
        assert_eq!(
            driver.device_key(&json!({ "device": "sdb1" })),
            Some("usb-storage:sdb1".to_string())
        );
    }

    #[test]
    fn device_key_accepts_hyphens_and_dots() {
        let driver = UsbStorageDriver::new();
        assert_eq!(
            driver.device_key(&json!({ "device": "dm-0" })),
            Some("usb-storage:dm-0".to_string())
        );
        assert_eq!(
            driver.device_key(&json!({ "device": "nvme0n1p1" })),
            Some("usb-storage:nvme0n1p1".to_string())
        );
        assert_eq!(
            driver.device_key(&json!({ "device": "sdb1.part" })),
            Some("usb-storage:sdb1.part".to_string())
        );
    }

    #[test]
    fn device_key_rejects_unsafe_device_ids() {
        let driver = UsbStorageDriver::new();
        assert_eq!(driver.device_key(&json!({ "device": "../etc" })), None);
        assert_eq!(driver.device_key(&json!({ "device": "/dev/sdb1" })), None);
        assert_eq!(driver.device_key(&json!({ "device": "foo..bar" })), None);
        assert_eq!(driver.device_key(&json!({ "device": "" })), None);
    }

    #[tokio::test]
    async fn rejects_unknown_actions() {
        let driver = UsbStorageDriver::new();
        let err = driver
            .query(json!({ "action": "format" }))
            .await
            .expect_err("unknown action should fail");

        assert!(matches!(
            err,
            AgentOSError::HalError(message) if message.contains("Unknown usb-storage action")
        ));
    }

    #[tokio::test]
    async fn rejects_invalid_mount_device_before_dbus() {
        let driver = UsbStorageDriver::new();
        let err = driver
            .query(json!({ "action": "mount", "device": "/dev/sdb1" }))
            .await
            .expect_err("invalid device should fail before D-Bus access");

        assert!(matches!(
            err,
            AgentOSError::HalError(message) if message.contains("Invalid device name")
        ));
    }

    #[test]
    fn requires_usb_storage_execute_permission() {
        let driver = UsbStorageDriver::new();
        assert_eq!(
            driver.required_permission(),
            ("hardware.usb-storage", PermissionOp::Execute)
        );
    }
}
