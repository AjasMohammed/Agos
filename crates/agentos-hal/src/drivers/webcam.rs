use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::{json, Value};
use uuid::Uuid;
use v4l::buffer::Type;
use v4l::io::mmap::Stream as MmapStream;
use v4l::io::traits::CaptureStream;
use v4l::prelude::*;
use v4l::video::Capture;

use crate::consent::ConsentStore;
use crate::hal::HalDriver;

const DEFAULT_DEVICE: &str = "/dev/video0";
const DEFAULT_WIDTH: u32 = 640;
const DEFAULT_HEIGHT: u32 = 480;
const DEFAULT_BUFFERS: u32 = 4;
const DEFAULT_BURST_COUNT: u64 = 3;
const MAX_BURST_COUNT: u64 = 60;
const DEFAULT_BURST_INTERVAL_MS: u64 = 200;
const MAX_BURST_INTERVAL_MS: u64 = 10_000;
const DEFAULT_CONSENT_TTL_SECONDS: u64 = 60;
const MAX_CONSENT_TTL_SECONDS: u64 = 3_600;
const WEBCAM_DEVICE_PREFIX: &str = "webcam:";
const CONSENT_RESOURCE_PREFIX: &str = "hardware.webcam.capture:";

pub struct WebcamDriver {
    consent_store: Arc<ConsentStore>,
}

impl Default for WebcamDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl WebcamDriver {
    pub fn new() -> Self {
        Self {
            consent_store: Arc::new(ConsentStore::new()),
        }
    }

    fn action_from_params<'a>(&self, params: &'a Value) -> Result<&'a str, AgentOSError> {
        params
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| AgentOSError::HalError("Missing 'action' param".into()))
    }

    fn consent_session<'a>(&self, params: &'a Value) -> Result<&'a str, AgentOSError> {
        params
            .get("agent_id")
            .or_else(|| params.get("session_id"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                AgentOSError::HalError(
                    "Missing 'agent_id' or 'session_id' for consent tracking".into(),
                )
            })
    }

    fn parse_device_path(&self, params: &Value) -> Result<PathBuf, AgentOSError> {
        let raw = params
            .get("device")
            .and_then(Value::as_str)
            .unwrap_or(DEFAULT_DEVICE);
        Self::parse_device_path_str(raw)
    }

    fn parse_device_path_str(raw: &str) -> Result<PathBuf, AgentOSError> {
        let path = Path::new(raw);
        if path.as_os_str().is_empty() {
            return Err(AgentOSError::HalError(
                "Invalid 'device' param: cannot be empty".into(),
            ));
        }
        if !path.is_absolute() {
            return Err(AgentOSError::HalError(
                "Invalid 'device' param: must be an absolute path".into(),
            ));
        }
        if path
            .components()
            .any(|component| component == Component::ParentDir)
        {
            return Err(AgentOSError::HalError(
                "Invalid 'device' param: path traversal rejected".into(),
            ));
        }

        let path_str = path.to_string_lossy();
        if !path_str.starts_with("/dev/video") {
            return Err(AgentOSError::HalError(
                "Invalid 'device' param: expected /dev/video*".into(),
            ));
        }
        let suffix = path_str.trim_start_matches("/dev/video");
        if suffix.is_empty() || !suffix.chars().all(|ch| ch.is_ascii_digit()) {
            return Err(AgentOSError::HalError(
                "Invalid 'device' param: expected /dev/video<index>".into(),
            ));
        }

        Ok(path.to_path_buf())
    }

    fn parse_output_path(&self, params: &Value) -> Result<Option<PathBuf>, AgentOSError> {
        let Some(raw) = params.get("output_path").and_then(Value::as_str) else {
            return Ok(None);
        };

        let path = Path::new(raw);
        if path.as_os_str().is_empty() {
            return Err(AgentOSError::HalError(
                "Invalid 'output_path' param: cannot be empty".into(),
            ));
        }
        if !path.is_absolute() {
            return Err(AgentOSError::HalError(
                "Invalid 'output_path' param: must be an absolute path".into(),
            ));
        }
        if path
            .components()
            .any(|component| component == Component::ParentDir)
        {
            return Err(AgentOSError::HalError(
                "Invalid 'output_path' param: path traversal rejected".into(),
            ));
        }
        Ok(Some(path.to_path_buf()))
    }

    fn parse_width_height(&self, params: &Value) -> Result<(u32, u32), AgentOSError> {
        let width = params
            .get("width")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_WIDTH as u64);
        let height = params
            .get("height")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_HEIGHT as u64);

        if !(1..=7_680).contains(&width) || !(1..=4_320).contains(&height) {
            return Err(AgentOSError::HalError(
                "Invalid dimensions: width/height out of supported bounds".into(),
            ));
        }
        Ok((width as u32, height as u32))
    }

    fn parse_buffers(&self, params: &Value) -> Result<u32, AgentOSError> {
        let buffers = params
            .get("buffers")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_BUFFERS as u64);
        if !(2..=16).contains(&buffers) {
            return Err(AgentOSError::HalError(
                "'buffers' must be between 2 and 16".into(),
            ));
        }
        Ok(buffers as u32)
    }

    fn parse_burst_params(&self, params: &Value) -> Result<(u64, u64), AgentOSError> {
        let count = params
            .get("count")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_BURST_COUNT);
        if count == 0 || count > MAX_BURST_COUNT {
            return Err(AgentOSError::HalError(format!(
                "'count' must be between 1 and {MAX_BURST_COUNT}"
            )));
        }

        let interval_ms = params
            .get("interval_ms")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_BURST_INTERVAL_MS);
        if interval_ms > MAX_BURST_INTERVAL_MS {
            return Err(AgentOSError::HalError(format!(
                "'interval_ms' must be <= {MAX_BURST_INTERVAL_MS}"
            )));
        }

        Ok((count, interval_ms))
    }

    fn parse_consent_ttl(&self, params: &Value) -> Result<Duration, AgentOSError> {
        let ttl_seconds = params
            .get("ttl_seconds")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_CONSENT_TTL_SECONDS);
        if ttl_seconds == 0 || ttl_seconds > MAX_CONSENT_TTL_SECONDS {
            return Err(AgentOSError::HalError(format!(
                "'ttl_seconds' must be between 1 and {MAX_CONSENT_TTL_SECONDS}"
            )));
        }
        Ok(Duration::from_secs(ttl_seconds))
    }

    fn consent_resource(device_path: &Path) -> String {
        format!("{CONSENT_RESOURCE_PREFIX}{}", device_path.display())
    }

    fn frame_extension(format_tag: &str) -> &'static str {
        if format_tag.contains("MJPG") || format_tag.contains("JPEG") {
            "jpg"
        } else {
            "frame"
        }
    }

    fn list_devices_from_root(root: &Path) -> Vec<Value> {
        let mut devices = Vec::new();
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !name.starts_with("video") {
                    continue;
                }

                let device_path = format!("/dev/{name}");
                let device_name = std::fs::read_to_string(entry.path().join("name"))
                    .map(|value| value.trim().to_string())
                    .unwrap_or_default();

                let mut capabilities = Value::Null;
                if let Ok(dev) = Device::with_path(&device_path) {
                    if let Ok(caps) = dev.query_caps() {
                        capabilities = json!({
                            "driver": caps.driver,
                            "card": caps.card,
                            "bus": caps.bus,
                            "version": caps.version,
                            "capabilities": format!("{:?}", caps.capabilities),
                        });
                    }
                }

                devices.push(json!({
                    "device": device_path,
                    "name": device_name,
                    "capabilities": capabilities,
                }));
            }
        }

        devices.sort_by(|left, right| {
            left.get("device")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .cmp(
                    right
                        .get("device")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                )
        });
        devices
    }

    async fn list_devices(&self) -> Result<Value, AgentOSError> {
        let devices = tokio::task::spawn_blocking(|| {
            Self::list_devices_from_root(Path::new("/sys/class/video4linux"))
        })
        .await
        .map_err(|error| AgentOSError::HalError(format!("webcam list task panicked: {error}")))?;

        Ok(json!({
            "devices": devices,
            "count": devices.len(),
        }))
    }

    async fn grant_capture_consent(&self, params: &Value) -> Result<Value, AgentOSError> {
        let session = self.consent_session(params)?;
        let device_path = self.parse_device_path(params)?;
        let ttl = self.parse_consent_ttl(params)?;
        let resource = Self::consent_resource(&device_path);

        self.consent_store.grant(session, &resource, ttl);
        Ok(json!({
            "consent_granted": true,
            "agent_id": session,
            "device": device_path.display().to_string(),
            "resource": resource,
            "ttl_seconds": ttl.as_secs(),
        }))
    }

    async fn revoke_capture_consent(&self, params: &Value) -> Result<Value, AgentOSError> {
        let session = self.consent_session(params)?;
        let device_path = self.parse_device_path(params)?;
        let resource = Self::consent_resource(&device_path);
        let revoked = self.consent_store.revoke(session, &resource);
        Ok(json!({
            "consent_revoked": revoked,
            "agent_id": session,
            "device": device_path.display().to_string(),
            "resource": resource,
        }))
    }

    async fn list_capture_consents(&self) -> Result<Value, AgentOSError> {
        let consents = self
            .consent_store
            .list()
            .into_iter()
            .filter(|(_, resource, _)| resource.starts_with(CONSENT_RESOURCE_PREFIX))
            .map(|(agent_id, resource, ttl_seconds)| {
                json!({
                    "agent_id": agent_id,
                    "resource": resource,
                    "ttl_seconds": ttl_seconds,
                })
            })
            .collect::<Vec<_>>();

        Ok(json!({ "consents": consents }))
    }

    fn ensure_capture_consent(
        &self,
        params: &Value,
        device_path: &Path,
    ) -> Result<(), AgentOSError> {
        let session = self.consent_session(params)?;
        let resource = Self::consent_resource(device_path);
        if self.consent_store.check(session, &resource) {
            return Ok(());
        }

        Err(AgentOSError::PermissionDenied {
            resource: resource.to_string(),
            operation: "consent_required".to_string(),
        })
    }

    fn capture_once_blocking(
        device_path: PathBuf,
        width: u32,
        height: u32,
        buffers: u32,
        output_path_override: Option<PathBuf>,
    ) -> Result<Value, AgentOSError> {
        let dev = Device::with_path(&device_path).map_err(|error| {
            AgentOSError::HalError(format!(
                "Open webcam '{}' failed: {error}",
                device_path.display()
            ))
        })?;

        let mut fmt = dev.format().map_err(|error| {
            AgentOSError::HalError(format!("Get webcam format failed: {error}"))
        })?;
        fmt.width = width;
        fmt.height = height;
        let fmt = dev.set_format(&fmt).map_err(|error| {
            AgentOSError::HalError(format!("Set webcam format failed: {error}"))
        })?;
        let format_tag = format!("{:?}", fmt.fourcc);

        let mut stream =
            MmapStream::with_buffers(&dev, Type::VideoCapture, buffers).map_err(|error| {
                AgentOSError::HalError(format!("Create webcam capture stream failed: {error}"))
            })?;
        let (buf, _) = stream.next().map_err(|error| {
            AgentOSError::HalError(format!("Capture webcam frame failed: {error}"))
        })?;
        if buf.is_empty() {
            return Err(AgentOSError::HalError(
                "Captured webcam frame is empty".into(),
            ));
        }

        let output_path = output_path_override.unwrap_or_else(|| {
            let ext = Self::frame_extension(&format_tag);
            std::env::temp_dir().join(format!("agentos-webcam-{}.{}", Uuid::new_v4(), ext))
        });

        let byte_count = buf.len();
        std::fs::write(&output_path, buf).map_err(|error| {
            AgentOSError::HalError(format!(
                "Write webcam frame '{}' failed: {error}",
                output_path.display()
            ))
        })?;

        Ok(json!({
            "captured": true,
            "image_path": output_path.display().to_string(),
            "device": device_path.display().to_string(),
            "width": fmt.width,
            "height": fmt.height,
            "format": format_tag,
            "bytes": byte_count,
        }))
    }

    async fn capture_frame(&self, params: &Value) -> Result<Value, AgentOSError> {
        let device_path = self.parse_device_path(params)?;
        let output_path = self.parse_output_path(params)?;
        let (width, height) = self.parse_width_height(params)?;
        let buffers = self.parse_buffers(params)?;
        self.ensure_capture_consent(params, &device_path)?;

        tokio::task::spawn_blocking(move || {
            Self::capture_once_blocking(device_path, width, height, buffers, output_path)
        })
        .await
        .map_err(|error| AgentOSError::HalError(format!("webcam capture task panicked: {error}")))?
    }

    fn capture_burst_blocking(
        device_path: PathBuf,
        width: u32,
        height: u32,
        buffers: u32,
        count: u64,
        interval: Duration,
    ) -> Result<(Vec<Value>, String), AgentOSError> {
        let dev = Device::with_path(&device_path).map_err(|error| {
            AgentOSError::HalError(format!(
                "Open webcam '{}' failed: {error}",
                device_path.display()
            ))
        })?;

        let mut fmt = dev.format().map_err(|error| {
            AgentOSError::HalError(format!("Get webcam format failed: {error}"))
        })?;
        fmt.width = width;
        fmt.height = height;
        let fmt = dev.set_format(&fmt).map_err(|error| {
            AgentOSError::HalError(format!("Set webcam format failed: {error}"))
        })?;
        let format_tag = format!("{:?}", fmt.fourcc);

        let mut stream =
            MmapStream::with_buffers(&dev, Type::VideoCapture, buffers).map_err(|error| {
                AgentOSError::HalError(format!("Create webcam capture stream failed: {error}"))
            })?;

        let mut frames = Vec::with_capacity(count as usize);
        for idx in 0..count {
            let (buf, _) = stream.next().map_err(|error| {
                AgentOSError::HalError(format!("Capture webcam frame {idx} failed: {error}"))
            })?;
            if buf.is_empty() {
                return Err(AgentOSError::HalError(format!(
                    "Captured webcam frame {idx} is empty"
                )));
            }

            let ext = Self::frame_extension(&format_tag);
            let output_path =
                std::env::temp_dir().join(format!("agentos-webcam-{}.{}", Uuid::new_v4(), ext));

            let byte_count = buf.len();
            std::fs::write(&output_path, buf).map_err(|error| {
                AgentOSError::HalError(format!(
                    "Write webcam frame '{}' failed: {error}",
                    output_path.display()
                ))
            })?;

            frames.push(json!({
                "captured": true,
                "image_path": output_path.display().to_string(),
                "device": device_path.display().to_string(),
                "width": fmt.width,
                "height": fmt.height,
                "format": format_tag,
                "bytes": byte_count,
            }));

            if idx + 1 < count && !interval.is_zero() {
                std::thread::sleep(interval);
            }
        }

        Ok((frames, device_path.display().to_string()))
    }

    async fn capture_burst(&self, params: &Value) -> Result<Value, AgentOSError> {
        let device_path = self.parse_device_path(params)?;
        let (width, height) = self.parse_width_height(params)?;
        let buffers = self.parse_buffers(params)?;
        let (count, interval_ms) = self.parse_burst_params(params)?;
        self.ensure_capture_consent(params, &device_path)?;

        let interval = Duration::from_millis(interval_ms);
        let (frames, device_display) = tokio::task::spawn_blocking(move || {
            Self::capture_burst_blocking(device_path, width, height, buffers, count, interval)
        })
        .await
        .map_err(|error| {
            AgentOSError::HalError(format!("webcam burst task panicked: {error}"))
        })??;

        Ok(json!({
            "captured": true,
            "burst": true,
            "count": frames.len(),
            "interval_ms": interval_ms,
            "device": device_display,
            "frames": frames,
        }))
    }
}

#[async_trait]
impl HalDriver for WebcamDriver {
    fn name(&self) -> &str {
        "webcam"
    }

    fn required_permission(&self) -> (&str, PermissionOp) {
        ("hardware.webcam.list", PermissionOp::Read)
    }

    fn required_permission_for(&self, params: &Value) -> (&str, PermissionOp) {
        match params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("list")
        {
            "list" => ("hardware.webcam.list", PermissionOp::Read),
            "capture" | "burst" => ("hardware.webcam.capture", PermissionOp::Execute),
            "grant_capture_consent" | "revoke_capture_consent" => {
                ("hardware.webcam.capture", PermissionOp::Execute)
            }
            "list_capture_consents" => ("hardware.webcam.capture", PermissionOp::Read),
            _ => self.required_permission(),
        }
    }

    fn device_key(&self, params: &Value) -> Option<String> {
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("list");
        if !matches!(
            action,
            "capture" | "burst" | "grant_capture_consent" | "revoke_capture_consent"
        ) {
            return None;
        }

        self.parse_device_path(params).ok().and_then(|path| {
            let node = path.file_name()?.to_string_lossy().to_string();
            Some(format!("{WEBCAM_DEVICE_PREFIX}{node}"))
        })
    }

    async fn query(&self, params: Value) -> Result<Value, AgentOSError> {
        match self.action_from_params(&params)? {
            "list" => self.list_devices().await,
            "capture" => self.capture_frame(&params).await,
            "burst" => self.capture_burst(&params).await,
            "grant_capture_consent" => self.grant_capture_consent(&params).await,
            "revoke_capture_consent" => self.revoke_capture_consent(&params).await,
            "list_capture_consents" => self.list_capture_consents().await,
            action => Err(AgentOSError::HalError(format!(
                "Unknown webcam action: {action}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn permission_scopes_match_actions() {
        let driver = WebcamDriver::new();
        assert_eq!(
            driver.required_permission_for(&json!({ "action": "list" })),
            ("hardware.webcam.list", PermissionOp::Read)
        );
        assert_eq!(
            driver.required_permission_for(&json!({ "action": "capture" })),
            ("hardware.webcam.capture", PermissionOp::Execute)
        );
        assert_eq!(
            driver.required_permission_for(&json!({ "action": "burst" })),
            ("hardware.webcam.capture", PermissionOp::Execute)
        );
    }

    #[tokio::test]
    async fn capture_requires_agent_id() {
        let driver = WebcamDriver::new();
        let error = driver
            .capture_frame(&json!({
                "action": "capture",
                "device": "/dev/video0",
            }))
            .await
            .expect_err("capture should require agent_id");

        assert!(matches!(error, AgentOSError::HalError(..)));
        assert!(error.to_string().contains("agent_id"));
    }

    #[tokio::test]
    async fn capture_requires_consent() {
        let driver = WebcamDriver::new();
        let error = driver
            .capture_frame(&json!({
                "action": "capture",
                "device": "/dev/video0",
                "agent_id": "test-agent",
            }))
            .await
            .expect_err("capture should require consent");

        assert!(matches!(error, AgentOSError::PermissionDenied { .. }));
    }

    #[tokio::test]
    async fn grant_and_revoke_consent_round_trip() {
        let driver = WebcamDriver::new();
        let grant = driver
            .grant_capture_consent(&json!({
                "action": "grant_capture_consent",
                "agent_id": "test-agent",
                "device": "/dev/video0",
                "ttl_seconds": 60,
            }))
            .await
            .expect("grant should succeed");
        assert_eq!(grant["consent_granted"], true);

        let list = driver
            .list_capture_consents()
            .await
            .expect("list should succeed");
        assert_eq!(list["consents"].as_array().unwrap().len(), 1);

        let revoke = driver
            .revoke_capture_consent(&json!({
                "action": "revoke_capture_consent",
                "agent_id": "test-agent",
                "device": "/dev/video0",
            }))
            .await
            .expect("revoke should succeed");
        assert_eq!(revoke["consent_revoked"], true);
    }

    #[test]
    fn device_path_validation_rejects_unsafe_paths() {
        let err = WebcamDriver::parse_device_path_str("video0").expect_err("must reject relative");
        assert!(err.to_string().contains("absolute path"));

        let err = WebcamDriver::parse_device_path_str("/dev/../../etc/passwd")
            .expect_err("must reject traversal");
        assert!(err.to_string().contains("path traversal"));
    }

    #[test]
    fn enumerate_devices_from_mock_sysfs() {
        let dir = tempdir().expect("tempdir");
        let webcam_dir = dir.path().join("video0");
        std::fs::create_dir_all(&webcam_dir).expect("create mock sysfs entry");
        std::fs::write(webcam_dir.join("name"), "MockCam").expect("write name");

        let devices = WebcamDriver::list_devices_from_root(dir.path());
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0]["device"], "/dev/video0");
        assert_eq!(devices[0]["name"], "MockCam");
    }
}
