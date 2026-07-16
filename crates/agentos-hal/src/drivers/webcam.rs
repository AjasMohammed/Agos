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
const WEBCAM_DEVICE_PREFIX: &str = "webcam:";

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
        Self::with_consent_store(Arc::new(ConsentStore::new()))
    }

    /// Construct with a shared consent store. The kernel passes its own store
    /// so that `agentos hal approve webcam:<node> <agent>` grants the capture
    /// consent window the driver checks.
    pub fn with_consent_store(consent_store: Arc<ConsentStore>) -> Self {
        Self { consent_store }
    }

    fn action_from_params<'a>(&self, params: &'a Value) -> Result<&'a str, AgentOSError> {
        params
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| AgentOSError::HalError("Missing 'action' param".into()))
    }

    /// The authenticated agent identity stamped into the payload by the tool
    /// wrapper (`WebcamTool`). Agent-supplied `agent_id`/`session_id` claims
    /// are never consulted — only the kernel-injected reserved key counts.
    fn authenticated_agent(params: &Value) -> Result<&str, AgentOSError> {
        params
            .get("__authenticated_agent_id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                AgentOSError::HalError(
                    "Capture consent requires an authenticated agent identity".into(),
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

    /// Consent resource key for a capture device — identical to the registry
    /// device key (`webcam:<node>`), so an operator `agentos hal approve` and
    /// the consent check use the same identifier.
    fn consent_resource(device_path: &Path) -> String {
        let node = device_path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| device_path.display().to_string());
        format!("{WEBCAM_DEVICE_PREFIX}{node}")
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

    /// Consent grants are operator-originated (`agentos hal approve`); an
    /// agent must never be able to grant or revoke its own capture consent.
    fn consent_is_operator_only() -> Result<Value, AgentOSError> {
        Err(AgentOSError::PermissionDenied {
            resource: "hardware.webcam.capture.consent".to_string(),
            operation: "operator_approval_required".to_string(),
        })
    }

    async fn list_capture_consents(&self) -> Result<Value, AgentOSError> {
        let consents = self
            .consent_store
            .list()
            .into_iter()
            .filter(|(_, resource, _)| resource.starts_with(WEBCAM_DEVICE_PREFIX))
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
        let agent_id = Self::authenticated_agent(params)?;
        let resource = Self::consent_resource(device_path);
        if self.consent_store.check(agent_id, &resource) {
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
            // Surfaced so the kernel's ToolExecuted audit records that this
            // capture passed an operator-granted consent check.
            "consent_checked": true,
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
            "consent_checked": true,
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
            "grant_capture_consent" | "revoke_capture_consent" => Self::consent_is_operator_only(),
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
    async fn capture_requires_authenticated_identity() {
        let driver = WebcamDriver::new();
        // A payload-supplied agent_id/session_id is NOT an authenticated identity.
        for spoof in [json!({"agent_id": "spoofed"}), json!({"session_id": "s1"})] {
            let mut params = json!({
                "action": "capture",
                "device": "/dev/video0",
            });
            params
                .as_object_mut()
                .unwrap()
                .extend(spoof.as_object().unwrap().clone());
            let error = driver
                .capture_frame(&params)
                .await
                .expect_err("capture should require authenticated identity");
            assert!(matches!(error, AgentOSError::HalError(..)));
            assert!(error.to_string().contains("authenticated agent identity"));
        }
    }

    #[tokio::test]
    async fn capture_requires_consent() {
        let driver = WebcamDriver::new();
        let error = driver
            .capture_frame(&json!({
                "action": "capture",
                "device": "/dev/video0",
                "__authenticated_agent_id": "test-agent",
            }))
            .await
            .expect_err("capture should require consent");

        assert!(matches!(error, AgentOSError::PermissionDenied { .. }));
    }

    #[tokio::test]
    async fn agent_cannot_grant_or_revoke_consent() {
        let driver = WebcamDriver::new();
        for action in ["grant_capture_consent", "revoke_capture_consent"] {
            let error = driver
                .query(json!({
                    "action": action,
                    "device": "/dev/video0",
                    "__authenticated_agent_id": "test-agent",
                }))
                .await
                .expect_err("agent-invoked consent grant/revoke must be rejected");
            match error {
                AgentOSError::PermissionDenied { operation, .. } => {
                    assert_eq!(operation, "operator_approval_required");
                }
                other => panic!("unexpected error: {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn consent_is_scoped_to_the_granted_agent() {
        let driver = WebcamDriver::new();
        // Operator-originated grant for agent-a (resource = registry device key).
        driver
            .consent_store
            .grant("agent-a", "webcam:video0", Duration::from_secs(60));

        let device = Path::new("/dev/video0");
        assert!(driver
            .ensure_capture_consent(&json!({"__authenticated_agent_id": "agent-a"}), device)
            .is_ok());
        // agent-b does not inherit, and a spoofed claim of agent-a's id via
        // the plain agent_id key is ignored.
        let err = driver
            .ensure_capture_consent(
                &json!({"__authenticated_agent_id": "agent-b", "agent_id": "agent-a"}),
                device,
            )
            .expect_err("another agent must not inherit consent");
        assert!(matches!(err, AgentOSError::PermissionDenied { .. }));

        let list = driver
            .list_capture_consents()
            .await
            .expect("list should succeed");
        let consents = list["consents"].as_array().unwrap();
        assert_eq!(consents.len(), 1);
        assert_eq!(consents[0]["agent_id"], "agent-a");
        assert_eq!(consents[0]["resource"], "webcam:video0");
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
