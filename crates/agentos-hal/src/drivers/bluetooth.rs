use std::collections::BTreeSet;
use std::time::Duration;

use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use bluer::{Adapter, AdapterEvent, Address, Device, Session};
use futures::{pin_mut, StreamExt};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::hal::HalDriver;

const BLUETOOTH_DEVICE_PREFIX: &str = "bluetooth:";
const DEFAULT_SCAN_DURATION_SECONDS: u64 = 10;
const MAX_SCAN_DURATION_SECONDS: u64 = 30;
const MAX_GATT_WRITE_BYTES: usize = 512;

/// BlueZ D-Bus Bluetooth driver designed for long-running agent workflows.
///
/// The JSON outputs intentionally include adapter, address, connection state,
/// and characteristic identifiers so a task can resume or reason about prior
/// results without hidden in-memory driver state.
pub struct BluetoothDriver;

impl Default for BluetoothDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl BluetoothDriver {
    pub fn new() -> Self {
        Self
    }

    fn action_from_params<'a>(&self, params: &'a Value) -> Result<&'a str, AgentOSError> {
        params
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| AgentOSError::HalError("Missing 'action' param".into()))
    }

    fn normalized_scan_duration(&self, params: &Value) -> u64 {
        params
            .get("duration_seconds")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_SCAN_DURATION_SECONDS)
            .min(MAX_SCAN_DURATION_SECONDS)
    }

    fn adapter_name_from_params<'a>(&self, params: &'a Value) -> Option<&'a str> {
        params
            .get("adapter")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
    }

    fn normalized_bt_address(address: &str) -> Option<String> {
        let parts: Vec<_> = address.split(':').collect();
        if parts.len() != 6 {
            return None;
        }
        let mut normalized = Vec::with_capacity(6);
        for part in parts {
            if part.len() != 2 || !part.chars().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            normalized.push(part.to_ascii_uppercase());
        }
        Some(normalized.join(":"))
    }

    fn parse_address_param(&self, params: &Value) -> Result<Address, AgentOSError> {
        let raw = params
            .get("address")
            .and_then(Value::as_str)
            .ok_or_else(|| AgentOSError::HalError("Missing 'address' param".into()))?;
        let normalized = Self::normalized_bt_address(raw).ok_or_else(|| {
            AgentOSError::HalError(
                "Invalid 'address' param: expected a Bluetooth MAC like 'AA:BB:CC:DD:EE:FF'".into(),
            )
        })?;
        normalized
            .parse()
            .map_err(|error| AgentOSError::HalError(format!("Invalid Bluetooth address: {error}")))
    }

    fn parse_uuid_param(&self, params: &Value, field_name: &str) -> Result<Uuid, AgentOSError> {
        let raw = params
            .get(field_name)
            .and_then(Value::as_str)
            .ok_or_else(|| AgentOSError::HalError(format!("Missing '{field_name}' param")))?;
        Uuid::parse_str(raw).map_err(|error| {
            AgentOSError::HalError(format!("Invalid '{field_name}' UUID: {error}"))
        })
    }

    fn decode_write_value(&self, params: &Value) -> Result<Vec<u8>, AgentOSError> {
        if let Some(value) = params.get("value_base64").and_then(Value::as_str) {
            let bytes = BASE64_STANDARD.decode(value).map_err(|error| {
                AgentOSError::HalError(format!("Invalid 'value_base64' payload: {error}"))
            })?;
            if bytes.len() > MAX_GATT_WRITE_BYTES {
                return Err(AgentOSError::HalError(format!(
                    "GATT writes are limited to {MAX_GATT_WRITE_BYTES} bytes"
                )));
            }
            return Ok(bytes);
        }

        if let Some(values) = params.get("value").and_then(Value::as_array) {
            let mut bytes = Vec::with_capacity(values.len());
            for value in values {
                let byte = value.as_u64().ok_or_else(|| {
                    AgentOSError::HalError(
                        "Invalid 'value' payload: expected an array of byte integers".into(),
                    )
                })?;
                if byte > u8::MAX as u64 {
                    return Err(AgentOSError::HalError(
                        "Invalid 'value' payload: each byte must be between 0 and 255".into(),
                    ));
                }
                bytes.push(byte as u8);
            }
            if bytes.len() > MAX_GATT_WRITE_BYTES {
                return Err(AgentOSError::HalError(format!(
                    "GATT writes are limited to {MAX_GATT_WRITE_BYTES} bytes"
                )));
            }
            return Ok(bytes);
        }

        Err(AgentOSError::HalError(
            "Missing GATT payload: provide 'value_base64' or a 'value' byte array".into(),
        ))
    }

    async fn session_and_adapter(
        &self,
        params: &Value,
    ) -> Result<(Session, Adapter), AgentOSError> {
        let session = Session::new()
            .await
            .map_err(|error| AgentOSError::HalError(format!("BlueZ session failed: {error}")))?;

        let adapter = if let Some(name) = self.adapter_name_from_params(params) {
            session.adapter(name).map_err(|error| {
                AgentOSError::HalError(format!("Bluetooth adapter '{name}' not found: {error}"))
            })?
        } else {
            session.default_adapter().await.map_err(|error| {
                AgentOSError::HalError(format!("No Bluetooth adapter available: {error}"))
            })?
        };

        Ok((session, adapter))
    }

    async fn device_from_params(
        &self,
        params: &Value,
    ) -> Result<(Session, Adapter, Device), AgentOSError> {
        let address = self.parse_address_param(params)?;
        let (session, adapter) = self.session_and_adapter(params).await?;
        let device = adapter.device(address).map_err(|error| {
            AgentOSError::HalError(format!(
                "Failed to access Bluetooth device '{}': {error}",
                address
            ))
        })?;
        Ok((session, adapter, device))
    }

    async fn ensure_adapter_powered(&self, adapter: &Adapter) -> Result<(), AgentOSError> {
        adapter
            .set_powered(true)
            .await
            .map_err(|error| AgentOSError::HalError(format!("Failed to power adapter: {error}")))
    }

    async fn device_snapshot(
        &self,
        adapter_name: &str,
        device: &Device,
    ) -> Result<Value, AgentOSError> {
        let address = device.address().to_string();
        let uuids = device
            .uuids()
            .await
            .map_err(|error| {
                AgentOSError::HalError(format!("Failed to query device UUIDs: {error}"))
            })?
            .unwrap_or_default()
            .into_iter()
            .map(|uuid| uuid.to_string())
            .collect::<Vec<_>>();
        let manufacturer_data = device
            .manufacturer_data()
            .await
            .map_err(|error| {
                AgentOSError::HalError(format!("Failed to query manufacturer data: {error}"))
            })?
            .unwrap_or_default()
            .into_iter()
            .map(|(company_id, bytes)| {
                json!({
                    "company_id": company_id,
                    "data_base64": BASE64_STANDARD.encode(bytes),
                })
            })
            .collect::<Vec<_>>();

        Ok(json!({
            "adapter": adapter_name,
            "address": address,
            "name": device.name().await.ok().flatten(),
            "alias": device.alias().await.ok(),
            "icon": device.icon().await.ok().flatten(),
            "rssi": device.rssi().await.ok().flatten(),
            "paired": device.is_paired().await.unwrap_or(false),
            "connected": device.is_connected().await.unwrap_or(false),
            "trusted": device.is_trusted().await.unwrap_or(false),
            "blocked": device.is_blocked().await.unwrap_or(false),
            "uuids": uuids,
            "manufacturer_data": manufacturer_data,
        }))
    }

    async fn list_adapters(&self, params: &Value) -> Result<Value, AgentOSError> {
        let session = Session::new()
            .await
            .map_err(|error| AgentOSError::HalError(format!("BlueZ session failed: {error}")))?;
        let mut names = session
            .adapter_names()
            .await
            .map_err(|error| AgentOSError::HalError(format!("Failed to list adapters: {error}")))?;
        names.sort();

        let default_adapter = session
            .default_adapter()
            .await
            .ok()
            .map(|adapter| adapter.name().to_string());
        let include_properties = params
            .get("include_properties")
            .and_then(Value::as_bool)
            .unwrap_or(true);

        let mut adapters = Vec::with_capacity(names.len());
        for name in names {
            let adapter = session.adapter(&name).map_err(|error| {
                AgentOSError::HalError(format!("Failed to load adapter '{name}': {error}"))
            })?;

            let mut entry = json!({
                "name": name,
                "is_default": default_adapter.as_deref() == Some(adapter.name()),
            });

            if include_properties {
                entry["address"] = adapter
                    .address()
                    .await
                    .ok()
                    .map(|address| Value::String(address.to_string()))
                    .unwrap_or(Value::Null);
                entry["address_type"] = adapter
                    .address_type()
                    .await
                    .ok()
                    .map(|kind| Value::String(kind.to_string()))
                    .unwrap_or(Value::Null);
                entry["alias"] = adapter
                    .alias()
                    .await
                    .ok()
                    .map(Value::String)
                    .unwrap_or(Value::Null);
                entry["powered"] = Value::Bool(adapter.is_powered().await.unwrap_or(false));
                entry["discoverable"] =
                    Value::Bool(adapter.is_discoverable().await.unwrap_or(false));
                entry["pairable"] = Value::Bool(adapter.is_pairable().await.unwrap_or(false));
                entry["discovering"] = Value::Bool(adapter.is_discovering().await.unwrap_or(false));
            }

            adapters.push(entry);
        }

        Ok(json!({
            "adapters": adapters,
            "default_adapter": default_adapter,
        }))
    }

    async fn scan_devices(&self, params: &Value) -> Result<Value, AgentOSError> {
        let (_session, adapter) = self.session_and_adapter(params).await?;
        self.ensure_adapter_powered(&adapter).await?;

        let duration = self.normalized_scan_duration(params);
        let discover = adapter.discover_devices().await.map_err(|error| {
            AgentOSError::HalError(format!("Bluetooth discovery failed: {error}"))
        })?;

        let mut addresses = BTreeSet::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(duration);
        pin_mut!(discover);

        loop {
            tokio::select! {
                maybe_event = discover.next() => {
                    match maybe_event {
                        Some(AdapterEvent::DeviceAdded(address)) => {
                            addresses.insert(address);
                        }
                        Some(_) => {}
                        None => break,
                    }
                }
                _ = tokio::time::sleep_until(deadline) => break,
            }
        }

        let mut devices = Vec::with_capacity(addresses.len());
        for address in addresses {
            let device = adapter.device(address).map_err(|error| {
                AgentOSError::HalError(format!(
                    "Failed to reopen discovered device '{}': {error}",
                    address
                ))
            })?;
            devices.push(self.device_snapshot(adapter.name(), &device).await?);
        }

        Ok(json!({
            "adapter": adapter.name(),
            "devices": devices,
            "scan_duration_seconds": duration,
        }))
    }

    /// Pair with a Bluetooth device.
    ///
    /// SAFETY: Pairing is security-sensitive and requires kernel-level escalation
    /// approval before reaching this method. The kernel's DeviceAccessGate enforces
    /// this. If unexpected pairing occurs, check the escalation pipeline.
    async fn pair_device(&self, params: &Value) -> Result<Value, AgentOSError> {
        let (_session, adapter, device) = self.device_from_params(params).await?;
        self.ensure_adapter_powered(&adapter).await?;

        tracing::info!(
            address = %device.address(),
            adapter = %adapter.name(),
            "Bluetooth pairing initiated (escalation assumed pre-approved by kernel)"
        );

        device.pair().await.map_err(|error| {
            AgentOSError::HalError(format!("Bluetooth pairing failed: {error}"))
        })?;

        Ok(json!({
            "adapter": adapter.name(),
            "address": device.address().to_string(),
            "name": device.name().await.ok().flatten(),
            "paired": true,
            "connected": device.is_connected().await.unwrap_or(false),
        }))
    }

    async fn connect_device(&self, params: &Value) -> Result<Value, AgentOSError> {
        let (_session, adapter, device) = self.device_from_params(params).await?;
        self.ensure_adapter_powered(&adapter).await?;

        if !device.is_connected().await.unwrap_or(false) {
            device.connect().await.map_err(|error| {
                AgentOSError::HalError(format!("Bluetooth connect failed: {error}"))
            })?;
        }

        Ok(json!({
            "adapter": adapter.name(),
            "address": device.address().to_string(),
            "name": device.name().await.ok().flatten(),
            "connected": device.is_connected().await.unwrap_or(true),
            "paired": device.is_paired().await.unwrap_or(false),
        }))
    }

    async fn disconnect_device(&self, params: &Value) -> Result<Value, AgentOSError> {
        let (_session, adapter, device) = self.device_from_params(params).await?;

        if device.is_connected().await.unwrap_or(false) {
            device.disconnect().await.map_err(|error| {
                AgentOSError::HalError(format!("Bluetooth disconnect failed: {error}"))
            })?;
        }

        Ok(json!({
            "adapter": adapter.name(),
            "address": device.address().to_string(),
            "name": device.name().await.ok().flatten(),
            "connected": device.is_connected().await.unwrap_or(false),
        }))
    }

    async fn gatt_characteristic(
        &self,
        params: &Value,
    ) -> Result<
        (
            Session,
            Adapter,
            Device,
            bluer::gatt::remote::Characteristic,
            Uuid,
            Uuid,
        ),
        AgentOSError,
    > {
        let service_uuid = self.parse_uuid_param(params, "service_uuid")?;
        let characteristic_uuid = self.parse_uuid_param(params, "characteristic_uuid")?;
        let (session, adapter, device) = self.device_from_params(params).await?;
        self.ensure_adapter_powered(&adapter).await?;

        if !device.is_connected().await.unwrap_or(false) {
            device.connect().await.map_err(|error| {
                AgentOSError::HalError(format!("Bluetooth connect for GATT failed: {error}"))
            })?;
        }

        for service in device.services().await.map_err(|error| {
            AgentOSError::HalError(format!("Failed to enumerate GATT services: {error}"))
        })? {
            let uuid = service.uuid().await.map_err(|error| {
                AgentOSError::HalError(format!("Failed to read GATT service UUID: {error}"))
            })?;
            if uuid != service_uuid {
                continue;
            }

            for characteristic in service.characteristics().await.map_err(|error| {
                AgentOSError::HalError(format!(
                    "Failed to enumerate GATT characteristics for service {service_uuid}: {error}"
                ))
            })? {
                let uuid = characteristic.uuid().await.map_err(|error| {
                    AgentOSError::HalError(format!(
                        "Failed to read GATT characteristic UUID for service {service_uuid}: {error}"
                    ))
                })?;
                if uuid == characteristic_uuid {
                    return Ok((
                        session,
                        adapter,
                        device,
                        characteristic,
                        service_uuid,
                        characteristic_uuid,
                    ));
                }
            }
        }

        Err(AgentOSError::HalError(format!(
            "GATT characteristic {characteristic_uuid} under service {service_uuid} was not found"
        )))
    }

    async fn gatt_read(&self, params: &Value) -> Result<Value, AgentOSError> {
        let (_session, adapter, device, characteristic, service_uuid, characteristic_uuid) =
            self.gatt_characteristic(params).await?;
        let bytes = characteristic.read().await.map_err(|error| {
            AgentOSError::HalError(format!(
                "GATT read failed for {characteristic_uuid}: {error}"
            ))
        })?;

        Ok(json!({
            "adapter": adapter.name(),
            "address": device.address().to_string(),
            "service_uuid": service_uuid.to_string(),
            "characteristic_uuid": characteristic_uuid.to_string(),
            "value_base64": BASE64_STANDARD.encode(&bytes),
            "value_len": bytes.len(),
            "connected": device.is_connected().await.unwrap_or(true),
        }))
    }

    async fn gatt_write(&self, params: &Value) -> Result<Value, AgentOSError> {
        let payload = self.decode_write_value(params)?;
        let (_session, adapter, device, characteristic, service_uuid, characteristic_uuid) =
            self.gatt_characteristic(params).await?;
        characteristic.write(&payload).await.map_err(|error| {
            AgentOSError::HalError(format!(
                "GATT write failed for {characteristic_uuid}: {error}"
            ))
        })?;

        Ok(json!({
            "adapter": adapter.name(),
            "address": device.address().to_string(),
            "service_uuid": service_uuid.to_string(),
            "characteristic_uuid": characteristic_uuid.to_string(),
            "written_bytes": payload.len(),
            "connected": device.is_connected().await.unwrap_or(true),
        }))
    }
}

#[async_trait]
impl HalDriver for BluetoothDriver {
    fn name(&self) -> &str {
        "bluetooth"
    }

    fn required_permission(&self) -> (&str, PermissionOp) {
        ("hardware.bluetooth.list", PermissionOp::Read)
    }

    fn required_permission_for(&self, params: &Value) -> (&str, PermissionOp) {
        match params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("list_adapters")
        {
            "list_adapters" => ("hardware.bluetooth.list", PermissionOp::Read),
            "scan" => ("hardware.bluetooth.scan", PermissionOp::Observe),
            "pair" => ("hardware.bluetooth.pair", PermissionOp::Execute),
            "connect" | "disconnect" => ("hardware.bluetooth.connection", PermissionOp::Execute),
            "gatt_read" => ("hardware.bluetooth.gatt", PermissionOp::Read),
            "gatt_write" => ("hardware.bluetooth.gatt", PermissionOp::Write),
            _ => ("hardware.bluetooth.list", PermissionOp::Read),
        }
    }

    fn device_key(&self, params: &Value) -> Option<String> {
        match params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("list_adapters")
        {
            "pair" | "connect" | "disconnect" | "gatt_read" | "gatt_write" => params
                .get("address")
                .and_then(Value::as_str)
                .and_then(Self::normalized_bt_address)
                .map(|address| format!("{BLUETOOTH_DEVICE_PREFIX}{address}")),
            _ => None,
        }
    }

    async fn query(&self, params: Value) -> Result<Value, AgentOSError> {
        match self.action_from_params(&params)? {
            "list_adapters" => self.list_adapters(&params).await,
            "scan" => self.scan_devices(&params).await,
            "pair" => self.pair_device(&params).await,
            "connect" => self.connect_device(&params).await,
            "disconnect" => self.disconnect_device(&params).await,
            "gatt_read" => self.gatt_read(&params).await,
            "gatt_write" => self.gatt_write(&params).await,
            action => Err(AgentOSError::HalError(format!(
                "Unsupported bluetooth action '{action}'"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_duration_is_capped() {
        let driver = BluetoothDriver::new();
        assert_eq!(driver.normalized_scan_duration(&json!({})), 10);
        assert_eq!(
            driver.normalized_scan_duration(&json!({ "duration_seconds": 120 })),
            30
        );
    }

    #[test]
    fn malformed_addresses_are_rejected() {
        let driver = BluetoothDriver::new();
        let err = driver
            .parse_address_param(&json!({ "address": "not-a-mac" }))
            .expect_err("invalid address should fail");
        assert!(
            matches!(err, AgentOSError::HalError(message) if message.contains("Invalid 'address'"))
        );
    }

    #[test]
    fn device_key_is_stable_for_device_actions() {
        let driver = BluetoothDriver::new();
        assert_eq!(
            driver.device_key(&json!({ "action": "connect", "address": "aa:bb:cc:dd:ee:ff" })),
            Some("bluetooth:AA:BB:CC:DD:EE:FF".to_string())
        );
        assert_eq!(driver.device_key(&json!({ "action": "scan" })), None);
    }

    #[test]
    fn write_payload_accepts_base64_or_byte_array() {
        let driver = BluetoothDriver::new();
        assert_eq!(
            driver
                .decode_write_value(&json!({ "value": [1, 2, 3] }))
                .unwrap(),
            vec![1, 2, 3]
        );
        assert_eq!(
            driver
                .decode_write_value(&json!({ "value_base64": "AQID" }))
                .unwrap(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn permissions_are_action_scoped() {
        let driver = BluetoothDriver::new();
        assert_eq!(
            driver.required_permission_for(&json!({ "action": "scan" })),
            ("hardware.bluetooth.scan", PermissionOp::Observe)
        );
        assert_eq!(
            driver.required_permission_for(&json!({ "action": "gatt_write" })),
            ("hardware.bluetooth.gatt", PermissionOp::Write)
        );
    }
}
