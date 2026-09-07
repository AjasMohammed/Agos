use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use nusb::transfer::{
    Buffer, Bulk, ControlIn, ControlOut, ControlType, In, Interrupt, Out, Recipient,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::hal::HalDriver;

const RAW_USB_DEVICE_PREFIX: &str = "raw-usb:";
const DEFAULT_TIMEOUT_MS: u64 = 1_000;
const MAX_TIMEOUT_MS: u64 = 120_000;
const MAX_TRANSFER_BYTES: usize = 1_048_576;

#[derive(Clone, Debug, PartialEq, Eq)]
struct DeviceSelector {
    vendor_id: u16,
    product_id: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OpenRequest {
    selector: DeviceSelector,
    interface_number: u8,
    detach_kernel_driver: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TransferKind {
    Bulk,
    Interrupt,
}

impl TransferKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Bulk => "bulk",
            Self::Interrupt => "interrupt",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ReadRequest {
    open: OpenRequest,
    endpoint: u8,
    length: usize,
    timeout: Duration,
    transfer_kind: TransferKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WriteRequest {
    open: OpenRequest,
    endpoint: u8,
    data: Vec<u8>,
    timeout: Duration,
    transfer_kind: TransferKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ControlDirection {
    In,
    Out,
}

impl ControlDirection {
    fn as_str(&self) -> &'static str {
        match self {
            Self::In => "in",
            Self::Out => "out",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ControlRequest {
    open: OpenRequest,
    direction: ControlDirection,
    control_type: ControlType,
    recipient: Recipient,
    request: u8,
    value: u16,
    index: u16,
    length: u16,
    data: Vec<u8>,
    timeout: Duration,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct UsbDeviceDescriptor {
    vendor_id: u16,
    product_id: u16,
    manufacturer: Option<String>,
    product: Option<String>,
    serial_number: Option<String>,
    bus_id: Option<String>,
    bus_number: Option<u8>,
    device_address: Option<u8>,
    interfaces: Vec<UsbInterfaceDescriptor>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct UsbInterfaceDescriptor {
    interface_number: u8,
    class: u8,
    subclass: u8,
    protocol: u8,
    description: Option<String>,
    endpoints: Vec<UsbEndpointDescriptor>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct UsbEndpointDescriptor {
    address: u8,
    direction: String,
    transfer_type: String,
    max_packet_size: usize,
    interval: u8,
}

#[derive(Clone, Debug)]
struct RawUsbSessionInfo {
    interface_number: u8,
    alt_setting: u8,
    endpoints: Vec<UsbEndpointDescriptor>,
}

#[async_trait]
trait RawUsbDeviceHandle: Send + Sync {
    fn session_info(&self) -> RawUsbSessionInfo;
    async fn bulk_read(
        &self,
        endpoint: u8,
        requested_length: usize,
        timeout: Duration,
    ) -> Result<Vec<u8>, AgentOSError>;
    async fn bulk_write(
        &self,
        endpoint: u8,
        data: Vec<u8>,
        timeout: Duration,
    ) -> Result<usize, AgentOSError>;
    async fn interrupt_read(
        &self,
        endpoint: u8,
        requested_length: usize,
        timeout: Duration,
    ) -> Result<Vec<u8>, AgentOSError>;
    async fn interrupt_write(
        &self,
        endpoint: u8,
        data: Vec<u8>,
        timeout: Duration,
    ) -> Result<usize, AgentOSError>;
    async fn control(&self, request: ControlRequest) -> Result<Value, AgentOSError>;
}

#[async_trait]
trait RawUsbBackend: Send + Sync {
    async fn list_devices(&self) -> Result<Vec<UsbDeviceDescriptor>, AgentOSError>;
    async fn open_device(
        &self,
        request: &OpenRequest,
    ) -> Result<Box<dyn RawUsbDeviceHandle>, AgentOSError>;
}

/// Stateless raw USB driver with strict per-device whitelisting.
///
/// The action surface is intentionally resumable for long-running agentic
/// workflows: every action re-opens and re-claims the interface from the
/// caller-supplied VID/PID instead of relying on hidden in-memory handles.
pub struct RawUsbDriver {
    whitelist: Arc<RwLock<HashSet<(u16, u16)>>>,
    allow_detach: bool,
    backend: Arc<dyn RawUsbBackend>,
}

impl Default for RawUsbDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl RawUsbDriver {
    pub fn new() -> Self {
        Self {
            whitelist: Arc::new(RwLock::new(HashSet::new())),
            allow_detach: false,
            backend: Arc::new(NusbRawUsbBackend),
        }
    }

    pub fn with_detach_permission(mut self, allow_detach: bool) -> Self {
        self.allow_detach = allow_detach;
        self
    }

    #[cfg(test)]
    fn with_backend(backend: Arc<dyn RawUsbBackend>) -> Self {
        Self {
            whitelist: Arc::new(RwLock::new(HashSet::new())),
            allow_detach: false,
            backend,
        }
    }

    pub fn allow_device(&self, vendor_id: u16, product_id: u16) {
        self.whitelist
            .write()
            .unwrap_or_else(|error| {
                tracing::warn!(
                    error = %error,
                    "Recovered from poisoned raw USB whitelist write lock"
                );
                error.into_inner()
            })
            .insert((vendor_id, product_id));
    }

    pub fn revoke_device(&self, vendor_id: u16, product_id: u16) {
        self.whitelist
            .write()
            .unwrap_or_else(|error| {
                tracing::warn!(
                    error = %error,
                    "Recovered from poisoned raw USB whitelist write lock"
                );
                error.into_inner()
            })
            .remove(&(vendor_id, product_id));
    }

    fn is_allowed(&self, vendor_id: u16, product_id: u16) -> bool {
        self.whitelist
            .read()
            .unwrap_or_else(|error| {
                tracing::warn!(
                    error = %error,
                    "Recovered from poisoned raw USB whitelist read lock"
                );
                error.into_inner()
            })
            .contains(&(vendor_id, product_id))
    }

    fn action_from_params<'a>(&self, params: &'a Value) -> &'a str {
        params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("list")
    }

    fn parse_u16_param(&self, params: &Value, field: &str) -> Result<u16, AgentOSError> {
        parse_u16_value(
            params
                .get(field)
                .ok_or_else(|| AgentOSError::HalError(format!("Missing '{field}' param")))?,
            field,
        )
    }

    fn parse_u8_param(&self, params: &Value, field: &str) -> Result<u8, AgentOSError> {
        parse_u8_value(
            params
                .get(field)
                .ok_or_else(|| AgentOSError::HalError(format!("Missing '{field}' param")))?,
            field,
        )
    }

    fn parse_transfer_kind(&self, params: &Value) -> Result<TransferKind, AgentOSError> {
        match params
            .get("transfer_kind")
            .or_else(|| params.get("kind"))
            .and_then(Value::as_str)
            .unwrap_or("bulk")
        {
            "bulk" => Ok(TransferKind::Bulk),
            "interrupt" => Ok(TransferKind::Interrupt),
            other => Err(AgentOSError::HalError(format!(
                "Invalid 'transfer_kind' param '{other}': expected 'bulk' or 'interrupt'"
            ))),
        }
    }

    fn parse_timeout(&self, params: &Value) -> Result<Duration, AgentOSError> {
        let timeout_ms = params
            .get("timeout_ms")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_TIMEOUT_MS);

        if timeout_ms == 0 || timeout_ms > MAX_TIMEOUT_MS {
            return Err(AgentOSError::HalError(format!(
                "'timeout_ms' must be between 1 and {MAX_TIMEOUT_MS}"
            )));
        }

        Ok(Duration::from_millis(timeout_ms))
    }

    fn parse_payload(&self, params: &Value, field: &str) -> Result<Vec<u8>, AgentOSError> {
        let base64_field = format!("{field}_base64");
        if let Some(value) = params.get(&base64_field).and_then(Value::as_str) {
            let decoded = BASE64_STANDARD.decode(value).map_err(|error| {
                AgentOSError::HalError(format!("Invalid '{base64_field}' payload: {error}"))
            })?;
            validate_transfer_length(decoded.len(), &base64_field)?;
            return Ok(decoded);
        }

        if let Some(values) = params.get(field).and_then(Value::as_array) {
            let mut bytes = Vec::with_capacity(values.len());
            for value in values {
                let byte = value.as_u64().ok_or_else(|| {
                    AgentOSError::HalError(format!(
                        "Invalid '{field}' payload: expected an array of byte integers"
                    ))
                })?;
                if byte > u8::MAX as u64 {
                    return Err(AgentOSError::HalError(format!(
                        "Invalid '{field}' payload: each byte must be between 0 and 255"
                    )));
                }
                bytes.push(byte as u8);
            }
            validate_transfer_length(bytes.len(), field)?;
            return Ok(bytes);
        }

        Err(AgentOSError::HalError(format!(
            "Missing USB payload: provide '{field}' or '{base64_field}'"
        )))
    }

    fn parse_selector(&self, params: &Value) -> Result<DeviceSelector, AgentOSError> {
        Ok(DeviceSelector {
            vendor_id: self.parse_u16_param(params, "vendor_id")?,
            product_id: self.parse_u16_param(params, "product_id")?,
        })
    }

    fn parse_open_request(&self, params: &Value) -> Result<OpenRequest, AgentOSError> {
        let selector = self.parse_selector(params)?;
        let interface_number = params
            .get("interface")
            .map(|value| parse_u8_value(value, "interface"))
            .transpose()?
            .unwrap_or(0);

        let detach_kernel_driver = params
            .get("detach_kernel_driver")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        if detach_kernel_driver && !self.allow_detach {
            return Err(AgentOSError::PermissionDenied {
                resource: "usb:kernel_driver_detach".to_string(),
                operation: "detach".to_string(),
            });
        }

        self.ensure_allowed(&selector)?;

        Ok(OpenRequest {
            selector,
            interface_number,
            detach_kernel_driver,
        })
    }

    fn parse_read_request(&self, params: &Value) -> Result<ReadRequest, AgentOSError> {
        let length = params
            .get("length")
            .map(|value| parse_usize_value(value, "length"))
            .transpose()?
            .unwrap_or(64);

        validate_transfer_length(length, "length")?;

        Ok(ReadRequest {
            open: self.parse_open_request(params)?,
            endpoint: self.parse_u8_param(params, "endpoint")?,
            length,
            timeout: self.parse_timeout(params)?,
            transfer_kind: self.parse_transfer_kind(params)?,
        })
    }

    fn parse_write_request(&self, params: &Value) -> Result<WriteRequest, AgentOSError> {
        Ok(WriteRequest {
            open: self.parse_open_request(params)?,
            endpoint: self.parse_u8_param(params, "endpoint")?,
            data: self.parse_payload(params, "data")?,
            timeout: self.parse_timeout(params)?,
            transfer_kind: self.parse_transfer_kind(params)?,
        })
    }

    fn parse_control_direction(&self, params: &Value) -> Result<ControlDirection, AgentOSError> {
        match params
            .get("direction")
            .and_then(Value::as_str)
            .or_else(|| {
                if params.get("data").is_some() || params.get("data_base64").is_some() {
                    Some("out")
                } else {
                    Some("in")
                }
            })
            .unwrap_or("in")
        {
            "in" => Ok(ControlDirection::In),
            "out" => Ok(ControlDirection::Out),
            other => Err(AgentOSError::HalError(format!(
                "Invalid 'direction' param '{other}': expected 'in' or 'out'"
            ))),
        }
    }

    fn parse_control_type(&self, params: &Value) -> Result<ControlType, AgentOSError> {
        match params
            .get("request_type")
            .and_then(Value::as_str)
            .unwrap_or("vendor")
        {
            "standard" => Ok(ControlType::Standard),
            "class" => Ok(ControlType::Class),
            "vendor" => Ok(ControlType::Vendor),
            other => Err(AgentOSError::HalError(format!(
                "Invalid 'request_type' param '{other}': expected 'standard', 'class', or 'vendor'"
            ))),
        }
    }

    fn parse_recipient(&self, params: &Value) -> Result<Recipient, AgentOSError> {
        match params
            .get("recipient")
            .and_then(Value::as_str)
            .unwrap_or("device")
        {
            "device" => Ok(Recipient::Device),
            "interface" => Ok(Recipient::Interface),
            "endpoint" => Ok(Recipient::Endpoint),
            "other" => Ok(Recipient::Other),
            other => Err(AgentOSError::HalError(format!(
                "Invalid 'recipient' param '{other}': expected 'device', 'interface', 'endpoint', or 'other'"
            ))),
        }
    }

    fn parse_control_request(&self, params: &Value) -> Result<ControlRequest, AgentOSError> {
        let open = self.parse_open_request(params)?;
        let direction = self.parse_control_direction(params)?;
        let data = if direction == ControlDirection::Out {
            self.parse_payload(params, "data")?
        } else {
            Vec::new()
        };
        let length = if direction == ControlDirection::In {
            let length = params
                .get("length")
                .map(|value| parse_usize_value(value, "length"))
                .transpose()?
                .unwrap_or(64);
            validate_transfer_length(length, "length")?;
            u16::try_from(length).map_err(|_| {
                AgentOSError::HalError(
                    "Control IN transfers are limited to 65535 bytes".to_string(),
                )
            })?
        } else {
            u16::try_from(data.len()).map_err(|_| {
                AgentOSError::HalError(
                    "Control OUT transfers are limited to 65535 bytes".to_string(),
                )
            })?
        };
        let recipient = self.parse_recipient(params)?;
        let index = match params.get("index") {
            Some(value) => parse_u16_value(value, "index")?,
            None if recipient == Recipient::Interface => open.interface_number as u16,
            None => 0,
        };

        Ok(ControlRequest {
            open,
            direction,
            control_type: self.parse_control_type(params)?,
            recipient,
            request: self.parse_u8_param(params, "request")?,
            value: match params.get("value") {
                Some(v) => parse_u16_value(v, "value")?,
                None => 0,
            },
            index,
            length,
            data,
            timeout: self.parse_timeout(params)?,
        })
    }

    fn ensure_allowed(&self, selector: &DeviceSelector) -> Result<(), AgentOSError> {
        if self.is_allowed(selector.vendor_id, selector.product_id) {
            return Ok(());
        }

        Err(AgentOSError::PermissionDenied {
            resource: self.device_key_from_selector(selector),
            operation: "raw_access".to_string(),
        })
    }

    fn device_key_from_selector(&self, selector: &DeviceSelector) -> String {
        format!(
            "{RAW_USB_DEVICE_PREFIX}{:04x}:{:04x}",
            selector.vendor_id, selector.product_id
        )
    }

    fn device_json(&self, device: &UsbDeviceDescriptor) -> Value {
        json!({
            "device_key": format!(
                "{RAW_USB_DEVICE_PREFIX}{:04x}:{:04x}",
                device.vendor_id,
                device.product_id
            ),
            "vendor_id": format!("{:04x}", device.vendor_id),
            "product_id": format!("{:04x}", device.product_id),
            "vendor_id_num": device.vendor_id,
            "product_id_num": device.product_id,
            "manufacturer": device.manufacturer,
            "product": device.product,
            "serial_number": device.serial_number,
            "bus_id": device.bus_id,
            "bus_number": device.bus_number,
            "device_address": device.device_address,
            "interfaces": device.interfaces.iter().map(interface_json).collect::<Vec<_>>(),
            "whitelisted": self.is_allowed(device.vendor_id, device.product_id),
        })
    }

    async fn list_devices(&self) -> Result<Value, AgentOSError> {
        let devices = self
            .backend
            .list_devices()
            .await?
            .into_iter()
            .map(|device| self.device_json(&device))
            .collect::<Vec<_>>();

        Ok(json!({ "devices": devices }))
    }

    async fn open_device(&self, params: &Value) -> Result<Value, AgentOSError> {
        let request = self.parse_open_request(params)?;
        let handle = self.backend.open_device(&request).await?;
        let session = handle.session_info();

        Ok(json!({
            "opened": true,
            "device_key": self.device_key_from_selector(&request.selector),
            "vendor_id": format!("{:04x}", request.selector.vendor_id),
            "product_id": format!("{:04x}", request.selector.product_id),
            "vendor_id_num": request.selector.vendor_id,
            "product_id_num": request.selector.product_id,
            "interface": session.interface_number,
            "alt_setting": session.alt_setting,
            "detach_kernel_driver": request.detach_kernel_driver,
            "endpoints": session.endpoints.iter().map(endpoint_json).collect::<Vec<_>>(),
        }))
    }

    async fn close_device(&self, params: &Value) -> Result<Value, AgentOSError> {
        // Stateless driver: no persistent handles to release. Validate the
        // selector and whitelist, then acknowledge. Opening the device just to
        // drop it would fail if the device is disconnected or busy.
        let request = self.parse_open_request(params)?;

        Ok(json!({
            "closed": true,
            "device_key": self.device_key_from_selector(&request.selector),
            "interface": request.interface_number,
        }))
    }

    async fn read(&self, params: &Value) -> Result<Value, AgentOSError> {
        let request = self.parse_read_request(params)?;
        let handle = self.backend.open_device(&request.open).await?;
        let data = match request.transfer_kind {
            TransferKind::Bulk => {
                handle
                    .bulk_read(request.endpoint, request.length, request.timeout)
                    .await?
            }
            TransferKind::Interrupt => {
                handle
                    .interrupt_read(request.endpoint, request.length, request.timeout)
                    .await?
            }
        };

        Ok(json!({
            "action": "read",
            "device_key": self.device_key_from_selector(&request.open.selector),
            "vendor_id": format!("{:04x}", request.open.selector.vendor_id),
            "product_id": format!("{:04x}", request.open.selector.product_id),
            "interface": request.open.interface_number,
            "transfer_kind": request.transfer_kind.as_str(),
            "endpoint": format_endpoint_address(request.endpoint),
            "bytes_read": data.len(),
            "data_base64": BASE64_STANDARD.encode(&data),
        }))
    }

    async fn write(&self, params: &Value) -> Result<Value, AgentOSError> {
        let request = self.parse_write_request(params)?;
        let byte_count = request.data.len();
        let handle = self.backend.open_device(&request.open).await?;
        let bytes_written = match request.transfer_kind {
            TransferKind::Bulk => {
                handle
                    .bulk_write(request.endpoint, request.data, request.timeout)
                    .await?
            }
            TransferKind::Interrupt => {
                handle
                    .interrupt_write(request.endpoint, request.data, request.timeout)
                    .await?
            }
        };

        Ok(json!({
            "action": "write",
            "device_key": self.device_key_from_selector(&request.open.selector),
            "vendor_id": format!("{:04x}", request.open.selector.vendor_id),
            "product_id": format!("{:04x}", request.open.selector.product_id),
            "interface": request.open.interface_number,
            "transfer_kind": request.transfer_kind.as_str(),
            "endpoint": format_endpoint_address(request.endpoint),
            "bytes_written": bytes_written,
            "payload_bytes": byte_count,
        }))
    }

    async fn control(&self, params: &Value) -> Result<Value, AgentOSError> {
        let request = self.parse_control_request(params)?;
        let handle = self.backend.open_device(&request.open).await?;
        let result = handle.control(request.clone()).await?;

        Ok(json!({
            "action": "control",
            "device_key": self.device_key_from_selector(&request.open.selector),
            "vendor_id": format!("{:04x}", request.open.selector.vendor_id),
            "product_id": format!("{:04x}", request.open.selector.product_id),
            "interface": request.open.interface_number,
            "direction": request.direction.as_str(),
            "request_type": format_control_type(request.control_type),
            "recipient": format_recipient(request.recipient),
            "request": request.request,
            "value": request.value,
            "index": request.index,
            "result": result,
        }))
    }
}

#[async_trait]
impl HalDriver for RawUsbDriver {
    fn name(&self) -> &str {
        "raw-usb"
    }

    fn required_permission(&self) -> (&str, PermissionOp) {
        ("hardware.raw-usb.list", PermissionOp::Read)
    }

    fn required_permission_for(&self, params: &Value) -> (&str, PermissionOp) {
        match params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("list")
        {
            "list" => ("hardware.raw-usb.list", PermissionOp::Read),
            "open" | "close" => ("hardware.raw-usb.session", PermissionOp::Execute),
            "read" => ("hardware.raw-usb.transfer", PermissionOp::Read),
            "write" => ("hardware.raw-usb.transfer", PermissionOp::Write),
            "control" => match self.parse_control_direction(params) {
                Ok(ControlDirection::In) => ("hardware.raw-usb.control", PermissionOp::Read),
                Ok(ControlDirection::Out) => ("hardware.raw-usb.control", PermissionOp::Write),
                Err(_) => ("hardware.raw-usb.control", PermissionOp::Write),
            },
            _ => self.required_permission(),
        }
    }

    fn device_key(&self, params: &Value) -> Option<String> {
        match params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("list")
        {
            "open" | "close" | "read" | "write" | "control" => self
                .parse_selector(params)
                .ok()
                .map(|selector| self.device_key_from_selector(&selector)),
            _ => None,
        }
    }

    async fn query(&self, params: Value) -> Result<Value, AgentOSError> {
        match self.action_from_params(&params) {
            "list" => self.list_devices().await,
            "open" => self.open_device(&params).await,
            "read" => self.read(&params).await,
            "write" => self.write(&params).await,
            "control" => self.control(&params).await,
            "close" => self.close_device(&params).await,
            other => Err(AgentOSError::HalError(format!(
                "Unknown raw-usb action: {other}"
            ))),
        }
    }
}

struct NusbRawUsbBackend;

#[async_trait]
impl RawUsbBackend for NusbRawUsbBackend {
    async fn list_devices(&self) -> Result<Vec<UsbDeviceDescriptor>, AgentOSError> {
        let devices = nusb::list_devices()
            .await
            .map_err(|error| AgentOSError::HalError(format!("USB enumerate failed: {error}")))?
            .map(|device| UsbDeviceDescriptor {
                vendor_id: device.vendor_id(),
                product_id: device.product_id(),
                manufacturer: device.manufacturer_string().map(ToString::to_string),
                product: device.product_string().map(ToString::to_string),
                serial_number: device.serial_number().map(ToString::to_string),
                bus_id: Some(device.bus_id().to_string()),
                #[cfg(target_os = "linux")]
                bus_number: Some(device.busnum()),
                #[cfg(not(target_os = "linux"))]
                bus_number: None,
                device_address: Some(device.device_address()),
                interfaces: device
                    .interfaces()
                    .map(|interface| UsbInterfaceDescriptor {
                        interface_number: interface.interface_number(),
                        class: interface.class(),
                        subclass: interface.subclass(),
                        protocol: interface.protocol(),
                        description: interface.interface_string().map(ToString::to_string),
                        endpoints: Vec::new(),
                    })
                    .collect(),
            })
            .collect::<Vec<_>>();

        Ok(devices)
    }

    async fn open_device(
        &self,
        request: &OpenRequest,
    ) -> Result<Box<dyn RawUsbDeviceHandle>, AgentOSError> {
        let device_info = nusb::list_devices()
            .await
            .map_err(|error| AgentOSError::HalError(format!("USB enumerate failed: {error}")))?
            .find(|device| {
                device.vendor_id() == request.selector.vendor_id
                    && device.product_id() == request.selector.product_id
            })
            .ok_or_else(|| {
                AgentOSError::HalError(format!(
                    "USB device {:04x}:{:04x} not found",
                    request.selector.vendor_id, request.selector.product_id
                ))
            })?;

        let device = device_info
            .open()
            .await
            .map_err(|error| AgentOSError::HalError(format!("USB open failed: {error}")))?;

        let interface = if request.detach_kernel_driver {
            device
                .detach_and_claim_interface(request.interface_number)
                .await
                .map_err(|error| {
                    AgentOSError::HalError(format!(
                        "USB interface claim with detach failed: {error}"
                    ))
                })?
        } else {
            device
                .claim_interface(request.interface_number)
                .await
                .map_err(|error| {
                    AgentOSError::HalError(format!("USB interface claim failed: {error}"))
                })?
        };

        let endpoints = interface
            .descriptor()
            .map(|descriptor| {
                descriptor
                    .endpoints()
                    .map(|endpoint| UsbEndpointDescriptor {
                        address: endpoint.address(),
                        direction: format!("{:?}", endpoint.direction()).to_ascii_lowercase(),
                        transfer_type: format!("{:?}", endpoint.transfer_type())
                            .to_ascii_lowercase(),
                        max_packet_size: endpoint.max_packet_size(),
                        interval: endpoint.interval(),
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        Ok(Box::new(NusbRawUsbHandle {
            interface,
            session_info: RawUsbSessionInfo {
                interface_number: request.interface_number,
                alt_setting: 0,
                endpoints,
            },
        }))
    }
}

struct NusbRawUsbHandle {
    interface: nusb::Interface,
    session_info: RawUsbSessionInfo,
}

#[async_trait]
impl RawUsbDeviceHandle for NusbRawUsbHandle {
    fn session_info(&self) -> RawUsbSessionInfo {
        RawUsbSessionInfo {
            interface_number: self.interface.interface_number(),
            alt_setting: self.interface.get_alt_setting(),
            endpoints: self.session_info.endpoints.clone(),
        }
    }

    async fn bulk_read(
        &self,
        endpoint: u8,
        requested_length: usize,
        timeout: Duration,
    ) -> Result<Vec<u8>, AgentOSError> {
        read_from_endpoint::<Bulk>(&self.interface, endpoint, requested_length, timeout).await
    }

    async fn bulk_write(
        &self,
        endpoint: u8,
        data: Vec<u8>,
        timeout: Duration,
    ) -> Result<usize, AgentOSError> {
        write_to_endpoint::<Bulk>(&self.interface, endpoint, data, timeout).await
    }

    async fn interrupt_read(
        &self,
        endpoint: u8,
        requested_length: usize,
        timeout: Duration,
    ) -> Result<Vec<u8>, AgentOSError> {
        read_from_endpoint::<Interrupt>(&self.interface, endpoint, requested_length, timeout).await
    }

    async fn interrupt_write(
        &self,
        endpoint: u8,
        data: Vec<u8>,
        timeout: Duration,
    ) -> Result<usize, AgentOSError> {
        write_to_endpoint::<Interrupt>(&self.interface, endpoint, data, timeout).await
    }

    async fn control(&self, request: ControlRequest) -> Result<Value, AgentOSError> {
        match request.direction {
            ControlDirection::In => {
                let data = self
                    .interface
                    .control_in(
                        ControlIn {
                            control_type: request.control_type,
                            recipient: request.recipient,
                            request: request.request,
                            value: request.value,
                            index: request.index,
                            length: request.length,
                        },
                        request.timeout,
                    )
                    .await
                    .map_err(|error| {
                        AgentOSError::HalError(format!("USB control IN transfer failed: {error}"))
                    })?;

                Ok(json!({
                    "bytes_read": data.len(),
                    "data_base64": BASE64_STANDARD.encode(&data),
                }))
            }
            ControlDirection::Out => {
                let payload_len = request.data.len();
                self.interface
                    .control_out(
                        ControlOut {
                            control_type: request.control_type,
                            recipient: request.recipient,
                            request: request.request,
                            value: request.value,
                            index: request.index,
                            data: &request.data,
                        },
                        request.timeout,
                    )
                    .await
                    .map_err(|error| {
                        AgentOSError::HalError(format!("USB control OUT transfer failed: {error}"))
                    })?;

                Ok(json!({
                    "bytes_written": payload_len,
                }))
            }
        }
    }
}

async fn read_from_endpoint<EpType: nusb::transfer::BulkOrInterrupt>(
    interface: &nusb::Interface,
    endpoint: u8,
    requested_length: usize,
    timeout: Duration,
) -> Result<Vec<u8>, AgentOSError> {
    let mut endpoint_handle = interface
        .endpoint::<EpType, In>(endpoint)
        .map_err(|error| AgentOSError::HalError(format!("USB endpoint open failed: {error}")))?;

    let actual_length = normalize_read_length(requested_length, endpoint_handle.max_packet_size())?;
    endpoint_handle.submit(Buffer::new(actual_length));
    let completion = tokio::time::timeout(timeout, endpoint_handle.next_complete())
        .await
        .map_err(|_| {
            endpoint_handle.cancel_all();
            AgentOSError::HalError(format!(
                "USB read transfer timed out after {}ms",
                timeout.as_millis()
            ))
        })?;
    let mut data = completion
        .into_result()
        .map_err(|error| AgentOSError::HalError(format!("USB read transfer failed: {error}")))?
        .into_vec();
    data.truncate(requested_length);

    Ok(data)
}

async fn write_to_endpoint<EpType: nusb::transfer::BulkOrInterrupt>(
    interface: &nusb::Interface,
    endpoint: u8,
    data: Vec<u8>,
    timeout: Duration,
) -> Result<usize, AgentOSError> {
    let mut endpoint_handle = interface
        .endpoint::<EpType, Out>(endpoint)
        .map_err(|error| AgentOSError::HalError(format!("USB endpoint open failed: {error}")))?;
    let submitted = data.len();
    endpoint_handle.submit(data.into());
    let completion = tokio::time::timeout(timeout, endpoint_handle.next_complete())
        .await
        .map_err(|_| {
            endpoint_handle.cancel_all();
            AgentOSError::HalError(format!(
                "USB write transfer timed out after {}ms",
                timeout.as_millis()
            ))
        })?;
    completion
        .into_result()
        .map_err(|error| AgentOSError::HalError(format!("USB write transfer failed: {error}")))?;

    Ok(submitted)
}

fn normalize_read_length(
    requested_length: usize,
    max_packet_size: usize,
) -> Result<usize, AgentOSError> {
    if max_packet_size == 0 {
        return Err(AgentOSError::HalError(
            "USB endpoint reported an invalid max packet size of 0".to_string(),
        ));
    }

    let remainder = requested_length % max_packet_size;
    if remainder == 0 {
        return Ok(requested_length);
    }

    requested_length
        .checked_add(max_packet_size - remainder)
        .ok_or_else(|| AgentOSError::HalError("USB read length overflowed".to_string()))
}

fn validate_transfer_length(length: usize, field: &str) -> Result<(), AgentOSError> {
    if length == 0 || length > MAX_TRANSFER_BYTES {
        return Err(AgentOSError::HalError(format!(
            "'{field}' must be between 1 and {MAX_TRANSFER_BYTES} bytes"
        )));
    }

    Ok(())
}

fn parse_u8_value(value: &Value, field: &str) -> Result<u8, AgentOSError> {
    let parsed = if let Some(value) = value.as_u64() {
        value
    } else if let Some(value) = value.as_str() {
        parse_integer_string(value, field)?
    } else {
        return Err(AgentOSError::HalError(format!(
            "Invalid '{field}' param: expected an integer or hex string"
        )));
    };

    u8::try_from(parsed).map_err(|_| {
        AgentOSError::HalError(format!("Invalid '{field}' param: value must fit in u8"))
    })
}

fn parse_u16_value(value: &Value, field: &str) -> Result<u16, AgentOSError> {
    let parsed = if let Some(value) = value.as_u64() {
        value
    } else if let Some(value) = value.as_str() {
        parse_integer_string(value, field)?
    } else {
        return Err(AgentOSError::HalError(format!(
            "Invalid '{field}' param: expected an integer or hex string"
        )));
    };

    u16::try_from(parsed).map_err(|_| {
        AgentOSError::HalError(format!("Invalid '{field}' param: value must fit in u16"))
    })
}

fn parse_usize_value(value: &Value, field: &str) -> Result<usize, AgentOSError> {
    let parsed = if let Some(value) = value.as_u64() {
        value
    } else if let Some(value) = value.as_str() {
        parse_integer_string(value, field)?
    } else {
        return Err(AgentOSError::HalError(format!(
            "Invalid '{field}' param: expected an integer or hex string"
        )));
    };

    usize::try_from(parsed).map_err(|_| {
        AgentOSError::HalError(format!("Invalid '{field}' param: value must fit in usize"))
    })
}

fn parse_integer_string(value: &str, field: &str) -> Result<u64, AgentOSError> {
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u64::from_str_radix(hex, 16).map_err(|error| {
            AgentOSError::HalError(format!("Invalid '{field}' hex value '{value}': {error}"))
        })
    } else {
        value.parse::<u64>().map_err(|error| {
            AgentOSError::HalError(format!("Invalid '{field}' value '{value}': {error}"))
        })
    }
}

fn interface_json(interface: &UsbInterfaceDescriptor) -> Value {
    json!({
        "interface_number": interface.interface_number,
        "class": interface.class,
        "subclass": interface.subclass,
        "protocol": interface.protocol,
        "description": interface.description,
        "endpoints": interface.endpoints.iter().map(endpoint_json).collect::<Vec<_>>(),
    })
}

fn endpoint_json(endpoint: &UsbEndpointDescriptor) -> Value {
    json!({
        "address": format_endpoint_address(endpoint.address),
        "address_num": endpoint.address,
        "direction": endpoint.direction,
        "transfer_type": endpoint.transfer_type,
        "max_packet_size": endpoint.max_packet_size,
        "interval": endpoint.interval,
    })
}

fn format_endpoint_address(endpoint: u8) -> String {
    format!("0x{endpoint:02x}")
}

fn format_control_type(control_type: ControlType) -> &'static str {
    match control_type {
        ControlType::Standard => "standard",
        ControlType::Class => "class",
        ControlType::Vendor => "vendor",
    }
}

fn format_recipient(recipient: Recipient) -> &'static str {
    match recipient {
        Recipient::Device => "device",
        Recipient::Interface => "interface",
        Recipient::Endpoint => "endpoint",
        Recipient::Other => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MockBackend {
        devices: Vec<UsbDeviceDescriptor>,
        opens: Mutex<Vec<OpenRequest>>,
    }

    #[async_trait]
    impl RawUsbBackend for MockBackend {
        async fn list_devices(&self) -> Result<Vec<UsbDeviceDescriptor>, AgentOSError> {
            Ok(self.devices.clone())
        }

        async fn open_device(
            &self,
            request: &OpenRequest,
        ) -> Result<Box<dyn RawUsbDeviceHandle>, AgentOSError> {
            self.opens.lock().unwrap().push(request.clone());
            Ok(Box::new(MockHandle))
        }
    }

    #[derive(Default)]
    struct MockHandle;

    #[async_trait]
    impl RawUsbDeviceHandle for MockHandle {
        fn session_info(&self) -> RawUsbSessionInfo {
            RawUsbSessionInfo {
                interface_number: 0,
                alt_setting: 0,
                endpoints: vec![UsbEndpointDescriptor {
                    address: 0x81,
                    direction: "in".to_string(),
                    transfer_type: "bulk".to_string(),
                    max_packet_size: 64,
                    interval: 1,
                }],
            }
        }

        async fn bulk_read(
            &self,
            _endpoint: u8,
            requested_length: usize,
            _timeout: Duration,
        ) -> Result<Vec<u8>, AgentOSError> {
            Ok(vec![0xAB; requested_length.min(4)])
        }

        async fn bulk_write(
            &self,
            _endpoint: u8,
            data: Vec<u8>,
            _timeout: Duration,
        ) -> Result<usize, AgentOSError> {
            Ok(data.len())
        }

        async fn interrupt_read(
            &self,
            _endpoint: u8,
            requested_length: usize,
            _timeout: Duration,
        ) -> Result<Vec<u8>, AgentOSError> {
            Ok(vec![0xCD; requested_length.min(2)])
        }

        async fn interrupt_write(
            &self,
            _endpoint: u8,
            data: Vec<u8>,
            _timeout: Duration,
        ) -> Result<usize, AgentOSError> {
            Ok(data.len())
        }

        async fn control(&self, request: ControlRequest) -> Result<Value, AgentOSError> {
            Ok(match request.direction {
                ControlDirection::In => json!({
                    "bytes_read": request.length,
                    "data_base64": BASE64_STANDARD.encode(vec![0xEF; request.length as usize]),
                }),
                ControlDirection::Out => json!({
                    "bytes_written": request.data.len(),
                }),
            })
        }
    }

    fn test_driver() -> RawUsbDriver {
        let backend = Arc::new(MockBackend {
            devices: vec![UsbDeviceDescriptor {
                vendor_id: 0x1234,
                product_id: 0x5678,
                manufacturer: Some("Test Vendor".to_string()),
                product: Some("Test Device".to_string()),
                serial_number: Some("SER123".to_string()),
                bus_id: Some("001".to_string()),
                bus_number: Some(1),
                device_address: Some(2),
                interfaces: vec![UsbInterfaceDescriptor {
                    interface_number: 0,
                    class: 0xff,
                    subclass: 0,
                    protocol: 0,
                    description: Some("Vendor".to_string()),
                    endpoints: vec![],
                }],
            }],
            opens: Mutex::new(Vec::new()),
        });
        let driver = RawUsbDriver::with_backend(backend);
        driver.allow_device(0x1234, 0x5678);
        driver
    }

    #[test]
    fn whitelist_enforcement_blocks_unknown_devices() {
        let backend = Arc::new(MockBackend::default());
        let driver = RawUsbDriver::with_backend(backend);
        assert!(!driver.is_allowed(0x1234, 0x5678));

        driver.allow_device(0x1234, 0x5678);
        assert!(driver.is_allowed(0x1234, 0x5678));
    }

    #[test]
    fn kernel_detach_is_blocked_by_default() {
        let driver = test_driver();
        let err = driver
            .parse_open_request(&json!({
                "vendor_id": "0x1234",
                "product_id": "0x5678",
                "detach_kernel_driver": true
            }))
            .expect_err("detach should be rejected by default");

        assert!(matches!(
            err,
            AgentOSError::PermissionDenied { resource, operation }
            if resource == "usb:kernel_driver_detach" && operation == "detach"
        ));
    }

    #[test]
    fn device_key_is_stable() {
        let driver = test_driver();
        assert_eq!(
            driver.device_key(&json!({
                "action": "read",
                "vendor_id": "0x1234",
                "product_id": "0x5678"
            })),
            Some("raw-usb:1234:5678".to_string())
        );
    }

    #[tokio::test]
    async fn list_marks_whitelisted_devices() {
        let driver = test_driver();
        let result = driver
            .query(json!({ "action": "list" }))
            .await
            .expect("list should succeed");
        let devices = result["devices"].as_array().expect("devices array");
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0]["whitelisted"], Value::Bool(true));
    }

    #[tokio::test]
    async fn read_and_write_actions_are_stateless() {
        let driver = test_driver();
        let read = driver
            .query(json!({
                "action": "read",
                "vendor_id": "0x1234",
                "product_id": "0x5678",
                "endpoint": "0x81",
                "length": 4
            }))
            .await
            .expect("read should succeed");
        assert_eq!(read["bytes_read"], 4);

        let write = driver
            .query(json!({
                "action": "write",
                "vendor_id": "0x1234",
                "product_id": "0x5678",
                "endpoint": "0x01",
                "data": [1, 2, 3]
            }))
            .await
            .expect("write should succeed");
        assert_eq!(write["bytes_written"], 3);
    }
}
