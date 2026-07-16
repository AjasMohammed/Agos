use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use ipp::prelude::*;
use ipp::value::IppName;
use serde_json::{json, Value};
use tokio::sync::{Mutex, RwLock};

use crate::hal::HalDriver;

const DEFAULT_CUPS_URI: &str = "ipp://localhost:631/";
const DEFAULT_MAX_JOBS_PER_HOUR: u32 = 10;
const DEFAULT_MAX_DOCUMENT_BYTES: u64 = 50 * 1024 * 1024;
const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Clone)]
struct RateLimitState {
    count: u32,
    window_start: Instant,
}

#[derive(Debug, Clone)]
struct TrackedPrintJob {
    printer_uri: Uri,
    printer_name: String,
    document_name: String,
    job_name: String,
    submitted_by: String,
}

/// CUPS/IPP-backed printer driver for printer discovery, job submission,
/// job status polling, and cancellation.
pub struct PrinterDriver {
    rate_limits: Arc<Mutex<HashMap<String, RateLimitState>>>,
    tracked_jobs: Arc<RwLock<HashMap<i32, TrackedPrintJob>>>,
    max_jobs_per_hour: u32,
    max_document_bytes: u64,
    default_server_uri: Uri,
}

impl Default for PrinterDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl PrinterDriver {
    pub fn new() -> Self {
        Self {
            rate_limits: Arc::new(Mutex::new(HashMap::new())),
            tracked_jobs: Arc::new(RwLock::new(HashMap::new())),
            max_jobs_per_hour: DEFAULT_MAX_JOBS_PER_HOUR,
            max_document_bytes: DEFAULT_MAX_DOCUMENT_BYTES,
            // Parses a compile-time constant; cannot fail at runtime.
            default_server_uri: DEFAULT_CUPS_URI
                .parse()
                .expect("default CUPS IPP URI must be valid"),
        }
    }

    fn client_for(&self, uri: Uri) -> AsyncIppClient {
        AsyncIppClient::new(uri)
    }

    fn server_uri_from_params(&self, params: &Value) -> Result<Uri, AgentOSError> {
        if let Some(server_uri) = params.get("server_uri").and_then(Value::as_str) {
            let parsed = Self::parse_server_uri(server_uri)?;
            self.reject_ssrf_host(&parsed)?;
            return Ok(parsed);
        }

        if let Some(printer_uri) = params.get("printer_uri").and_then(Value::as_str) {
            let parsed = Self::parse_printer_uri(printer_uri)?;
            self.reject_ssrf_host(&parsed)?;
            return Ok(Self::server_uri_from_printer_uri(&parsed));
        }

        Ok(self.default_server_uri.clone())
    }

    /// Host guard for agent-supplied printer URIs. Only the operator-configured
    /// default CUPS authority is reachable; every other host is rejected.
    ///
    /// This blocks two attacks at once:
    /// - exfiltration — a public IPP host would let an agent read a local file
    ///   (via `document_path`) and ship its bytes off-box to an attacker;
    /// - SSRF — private/loopback/link-local hosts (10.x, 127.x, `169.254.169.254`
    ///   cloud metadata, internal admin endpoints) would turn the IPP client
    ///   into an internal-network probe.
    ///
    /// Network printers are expected to be registered with the local CUPS daemon
    /// and addressed by name through the default server, not by arbitrary URI.
    fn reject_ssrf_host(&self, uri: &Uri) -> Result<(), AgentOSError> {
        let Some(authority) = uri.authority() else {
            return Ok(());
        };
        if Some(authority.as_str()) == self.default_server_uri.authority().map(|a| a.as_str()) {
            return Ok(());
        }
        Err(AgentOSError::HalError(format!(
            "Printer host '{}' is not the configured CUPS server \
             (SSRF/exfiltration blocked); register network printers with the local \
             CUPS daemon and print by name",
            authority.as_str()
        )))
    }

    fn parse_server_uri(uri: &str) -> Result<Uri, AgentOSError> {
        let parsed: Uri = uri
            .parse()
            .map_err(|e| AgentOSError::HalError(format!("Invalid 'server_uri': {e}")))?;
        let scheme = parsed.scheme_str().unwrap_or_default();
        if !matches!(scheme, "ipp" | "ipps" | "http" | "https") {
            return Err(AgentOSError::HalError(
                "Invalid 'server_uri': use ipp://, ipps://, http://, or https://".into(),
            ));
        }
        if parsed.authority().is_none() {
            return Err(AgentOSError::HalError(
                "Invalid 'server_uri': missing host".into(),
            ));
        }
        Ok(parsed)
    }

    fn printer_name_from_params<'a>(&self, params: &'a Value) -> Result<&'a str, AgentOSError> {
        let printer = params
            .get("printer")
            .and_then(Value::as_str)
            .ok_or_else(|| AgentOSError::HalError("Missing 'printer' param".into()))?;

        Self::validate_printer_name(printer)?;
        Ok(printer)
    }

    fn validate_printer_name(printer: &str) -> Result<(), AgentOSError> {
        if printer.is_empty() {
            return Err(AgentOSError::HalError("Missing 'printer' param".into()));
        }
        if !printer
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        {
            return Err(AgentOSError::HalError(
                "Invalid printer name; use letters, digits, '.', '_' or '-'".into(),
            ));
        }
        Ok(())
    }

    fn printer_uri_from_params(&self, params: &Value) -> Result<Uri, AgentOSError> {
        if let Some(uri) = params.get("printer_uri").and_then(Value::as_str) {
            let parsed = Self::parse_printer_uri(uri)?;
            self.reject_ssrf_host(&parsed)?;
            return Ok(parsed);
        }

        let printer = self.printer_name_from_params(params)?;
        let server_uri = self.server_uri_from_params(params)?;
        Self::build_printer_uri(&server_uri, printer)
    }

    fn parse_printer_uri(uri: &str) -> Result<Uri, AgentOSError> {
        let parsed: Uri = uri
            .parse()
            .map_err(|e| AgentOSError::HalError(format!("Invalid 'printer_uri': {e}")))?;
        let scheme = parsed.scheme_str().unwrap_or_default();
        if !matches!(scheme, "ipp" | "ipps" | "http" | "https") {
            return Err(AgentOSError::HalError(
                "Invalid 'printer_uri': use ipp://, ipps://, http://, or https://".into(),
            ));
        }
        if parsed.authority().is_none() {
            return Err(AgentOSError::HalError(
                "Invalid 'printer_uri': missing host".into(),
            ));
        }
        Ok(parsed)
    }

    fn build_printer_uri(server_uri: &Uri, printer: &str) -> Result<Uri, AgentOSError> {
        let scheme = server_uri.scheme_str().unwrap_or("ipp");
        let authority = server_uri.authority().ok_or_else(|| {
            AgentOSError::HalError("Invalid printer server URI: missing host".into())
        })?;
        let uri = format!("{scheme}://{authority}/printers/{printer}");
        uri.parse()
            .map_err(|e| AgentOSError::HalError(format!("Failed to build printer URI: {e}")))
    }

    fn server_uri_from_printer_uri(printer_uri: &Uri) -> Uri {
        let scheme = printer_uri.scheme_str().unwrap_or("ipp");
        let authority = printer_uri
            .authority()
            .map(|authority| authority.as_str())
            .unwrap_or("localhost:631");
        format!("{scheme}://{authority}/")
            .parse()
            .expect("printer authority should produce a valid server URI")
    }

    fn document_path_from_params(&self, params: &Value) -> Result<PathBuf, AgentOSError> {
        let raw = params
            .get("document_path")
            .and_then(Value::as_str)
            .ok_or_else(|| AgentOSError::HalError("Missing 'document_path' param".into()))?;

        let path = Path::new(raw);
        if path.as_os_str().is_empty() {
            return Err(AgentOSError::HalError(
                "Missing 'document_path' param".into(),
            ));
        }

        if path
            .components()
            .any(|component| component == Component::ParentDir)
        {
            return Err(AgentOSError::HalError("Path traversal blocked".into()));
        }

        // symlink_metadata does NOT follow symlinks — reject them outright so
        // a link under an allowed directory can't smuggle /etc/shadow to the
        // printer (matches the audio driver's playback-path policy).
        let metadata = std::fs::symlink_metadata(path).map_err(|e| {
            AgentOSError::HalError(format!("Unable to read document metadata '{}': {e}", raw))
        })?;

        if metadata.file_type().is_symlink() {
            return Err(AgentOSError::HalError(
                "Document path is a symlink; refusing to print".into(),
            ));
        }

        if !metadata.is_file() {
            return Err(AgentOSError::HalError(format!(
                "Document path '{}' is not a regular file",
                raw
            )));
        }

        if metadata.len() > self.max_document_bytes {
            return Err(AgentOSError::HalError(format!(
                "Document exceeds the {} byte print limit",
                self.max_document_bytes
            )));
        }

        Ok(path.to_path_buf())
    }

    fn document_format_from_params(&self, params: &Value) -> Result<String, AgentOSError> {
        let format = params
            .get("format")
            .and_then(Value::as_str)
            .unwrap_or("application/pdf");

        if format.is_empty()
            || !format
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '+' | '-' | '_'))
        {
            return Err(AgentOSError::HalError(
                "Invalid 'format'; expected a MIME type such as 'application/pdf'".into(),
            ));
        }

        Ok(format.to_string())
    }

    fn copies_from_params(&self, params: &Value) -> Result<u32, AgentOSError> {
        let copies = params.get("copies").and_then(Value::as_u64).unwrap_or(1);
        if copies == 0 || copies > 99 {
            return Err(AgentOSError::HalError(
                "'copies' must be between 1 and 99".into(),
            ));
        }
        Ok(copies as u32)
    }

    fn requesting_user_from_params(&self, params: &Value, agent_id: &str) -> String {
        params
            .get("requesting_user")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or(agent_id)
            .to_string()
    }

    async fn check_rate_limit(&self, params: &Value) -> Result<String, AgentOSError> {
        let agent_id = params
            .get("agent_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or("anonymous")
            .to_string();

        let mut state = self.rate_limits.lock().await;
        let now = Instant::now();
        let entry = state
            .entry(agent_id.clone())
            .or_insert_with(|| RateLimitState {
                count: 0,
                window_start: now,
            });

        if now.duration_since(entry.window_start) >= RATE_LIMIT_WINDOW {
            entry.count = 0;
            entry.window_start = now;
        }

        if entry.count >= self.max_jobs_per_hour {
            return Err(AgentOSError::RateLimited {
                detail: format!(
                    "printer job limit exceeded for agent '{agent_id}' ({}/hour)",
                    self.max_jobs_per_hour
                ),
            });
        }

        entry.count += 1;
        Ok(agent_id)
    }

    async fn list_printers(&self, params: &Value) -> Result<Value, AgentOSError> {
        let server_uri = self.server_uri_from_params(params)?;
        let client = self.client_for(server_uri.clone());
        let response = client
            .send(IppOperationBuilder::cups().get_printers())
            .await
            .map_err(|e| AgentOSError::HalError(format!("IPP printer discovery failed: {e}")))?;

        Self::ensure_success(&response, "discover printers")?;

        let printers = response
            .attributes()
            .groups_of(DelimiterTag::PrinterAttributes)
            .filter_map(Self::printer_summary_from_group)
            .collect::<Vec<_>>();

        Ok(json!({
            "server_uri": server_uri.to_string(),
            "printers": printers,
        }))
    }

    async fn print_document(&self, params: &Value) -> Result<Value, AgentOSError> {
        let printer_uri = self.printer_uri_from_params(params)?;
        let printer_name = Self::printer_name_for_display(params, &printer_uri);
        let document_path = self.document_path_from_params(params)?;
        let document_name = document_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("document")
            .to_string();
        let format = self.document_format_from_params(params)?;
        let copies = self.copies_from_params(params)?;
        let agent_id = self.check_rate_limit(params).await?;
        let requesting_user = self.requesting_user_from_params(params, &agent_id);
        let job_name = params
            .get("job_name")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or(&document_name)
            .to_string();

        let bytes = tokio::fs::read(&document_path).await.map_err(|e| {
            AgentOSError::HalError(format!(
                "Unable to read document '{}': {e}",
                document_path.display()
            ))
        })?;

        let copies_attr =
            IppAttribute::with_name(IppAttribute::COPIES, IppValue::Integer(copies as i32))
                .map_err(|e| {
                    AgentOSError::HalError(format!("Failed to encode copies attribute: {e}"))
                })?;

        let client = self.client_for(printer_uri.clone());
        let create_job = IppOperationBuilder::create_job(printer_uri.clone())
            .job_name(&job_name)
            .attribute(copies_attr)
            .build()
            .map_err(|e| {
                AgentOSError::HalError(format!("Failed to build Create-Job request: {e}"))
            })?;
        let create_response = client
            .send(create_job)
            .await
            .map_err(|e| AgentOSError::HalError(format!("IPP Create-Job failed: {e}")))?;
        Self::ensure_success(&create_response, "create print job")?;

        let job_id = Self::response_i32_attribute(&create_response, IppAttribute::JOB_ID)
            .ok_or_else(|| {
                AgentOSError::HalError("IPP Create-Job response missing job-id".into())
            })?;

        let send_document = IppOperationBuilder::send_document(
            printer_uri.clone(),
            job_id,
            IppPayload::new(Cursor::new(bytes)),
        )
        .user_name(&requesting_user)
        .document_format(&format)
        .build()
        .map_err(|e| {
            AgentOSError::HalError(format!("Failed to build Send-Document request: {e}"))
        })?;

        let send_response = client
            .send(send_document)
            .await
            .map_err(|e| AgentOSError::HalError(format!("IPP Send-Document failed: {e}")))?;
        Self::ensure_success(&send_response, "submit print document")?;

        self.tracked_jobs.write().await.insert(
            job_id,
            TrackedPrintJob {
                printer_uri: printer_uri.clone(),
                printer_name: printer_name.clone(),
                document_name: document_name.clone(),
                job_name: job_name.clone(),
                submitted_by: agent_id.clone(),
            },
        );

        Ok(json!({
            "submitted": true,
            "job_id": job_id,
            "printer": printer_name,
            "printer_uri": printer_uri.to_string(),
            "format": format,
            "copies": copies,
            "job_name": job_name,
            "document_name": document_name,
            "agent_id": agent_id,
        }))
    }

    async fn job_status(&self, params: &Value) -> Result<Value, AgentOSError> {
        let job_id = Self::job_id_from_params(params)?;
        let tracked = self.tracked_jobs.read().await.get(&job_id).cloned();
        let printer_uri = match params.get("printer").or_else(|| params.get("printer_uri")) {
            Some(_) => self.printer_uri_from_params(params)?,
            None => tracked
                .as_ref()
                .map(|job| job.printer_uri.clone())
                .ok_or_else(|| {
                    AgentOSError::HalError(
                        "Missing printer context for job status; pass 'printer' or 'printer_uri' when querying a job created in another runtime".into(),
                    )
                })?,
        };
        let requesting_user = tracked
            .as_ref()
            .map(|job| job.submitted_by.clone())
            .unwrap_or_else(|| self.requesting_user_from_params(params, "anonymous"));

        let response = self
            .client_for(printer_uri.clone())
            .send(
                IppOperationBuilder::get_job_attributes(printer_uri.clone(), job_id)
                    .user_name(&requesting_user)
                    .build()
                    .map_err(|e| {
                        AgentOSError::HalError(format!(
                            "Failed to build Get-Job-Attributes request: {e}"
                        ))
                    })?,
            )
            .await
            .map_err(|e| AgentOSError::HalError(format!("IPP Get-Job-Attributes failed: {e}")))?;
        Self::ensure_success(&response, "query print job status")?;

        let state_code = Self::response_i32_attribute(&response, IppAttribute::JOB_STATE);
        let state_label = state_code.and_then(Self::job_state_label);
        let reasons =
            Self::response_string_array_attribute(&response, IppAttribute::JOB_STATE_REASONS);
        let status_message =
            Self::response_string_attribute(&response, IppAttribute::STATUS_MESSAGE);

        Ok(json!({
            "job_id": job_id,
            "printer": tracked.as_ref().map(|job| job.printer_name.clone()),
            "printer_uri": printer_uri.to_string(),
            "job_name": tracked.as_ref().map(|job| job.job_name.clone()),
            "document_name": tracked.as_ref().map(|job| job.document_name.clone()),
            "state": state_label,
            "state_code": state_code,
            "state_reasons": reasons,
            "status_message": status_message,
            "is_terminal": state_code.is_some_and(Self::job_state_is_terminal),
        }))
    }

    async fn cancel_job(&self, params: &Value) -> Result<Value, AgentOSError> {
        let job_id = Self::job_id_from_params(params)?;
        let tracked = self.tracked_jobs.read().await.get(&job_id).cloned();
        let printer_uri = match params.get("printer").or_else(|| params.get("printer_uri")) {
            Some(_) => self.printer_uri_from_params(params)?,
            None => tracked
                .as_ref()
                .map(|job| job.printer_uri.clone())
                .ok_or_else(|| {
                    AgentOSError::HalError(
                        "Missing printer context for cancel; pass 'printer' or 'printer_uri' when cancelling a job created in another runtime".into(),
                    )
                })?,
        };
        let requesting_user = tracked
            .as_ref()
            .map(|job| job.submitted_by.clone())
            .unwrap_or_else(|| self.requesting_user_from_params(params, "anonymous"));

        let response = self
            .client_for(printer_uri.clone())
            .send(
                IppOperationBuilder::cancel_job(printer_uri.clone(), job_id)
                    .user_name(&requesting_user)
                    .build()
                    .map_err(|e| {
                        AgentOSError::HalError(format!("Failed to build Cancel-Job request: {e}"))
                    })?,
            )
            .await
            .map_err(|e| AgentOSError::HalError(format!("IPP Cancel-Job failed: {e}")))?;
        Self::ensure_success(&response, "cancel print job")?;

        Ok(json!({
            "cancelled": true,
            "job_id": job_id,
            "printer": tracked.as_ref().map(|job| job.printer_name.clone()),
            "printer_uri": printer_uri.to_string(),
        }))
    }

    fn ensure_success(response: &IppRequestResponse, operation: &str) -> Result<(), AgentOSError> {
        let status = response.header().status_code();
        if status.is_success() {
            return Ok(());
        }

        Err(AgentOSError::HalError(format!(
            "IPP failed to {operation}: {status:?}"
        )))
    }

    fn job_id_from_params(params: &Value) -> Result<i32, AgentOSError> {
        let Some(job_id) = params.get("job_id").and_then(Value::as_i64) else {
            return Err(AgentOSError::HalError("Missing 'job_id' param".into()));
        };
        if job_id <= 0 || job_id > i32::MAX as i64 {
            return Err(AgentOSError::HalError(
                "'job_id' must be a positive 32-bit integer".into(),
            ));
        }
        Ok(job_id as i32)
    }

    fn printer_name_for_display(params: &Value, printer_uri: &Uri) -> String {
        params
            .get("printer")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| {
                printer_uri
                    .path()
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
            })
            .unwrap_or_else(|| "printer".to_string())
    }

    fn printer_summary_from_group(group: &IppAttributeGroup) -> Option<Value> {
        let printer_name = Self::group_string_attribute(group, IppAttribute::PRINTER_NAME)?;
        let printer_uri = Self::group_string_attribute(group, IppAttribute::PRINTER_URI_SUPPORTED)
            .or_else(|| Self::group_string_attribute(group, IppAttribute::PRINTER_URI));
        let state_code = Self::group_i32_attribute(group, IppAttribute::PRINTER_STATE);

        Some(json!({
            "printer": printer_name,
            "printer_uri": printer_uri,
            "info": Self::group_string_attribute(group, IppAttribute::PRINTER_INFO),
            "location": Self::group_string_attribute(group, IppAttribute::PRINTER_LOCATION),
            "make_and_model": Self::group_string_attribute(group, IppAttribute::PRINTER_MAKE_AND_MODEL),
            "accepting_jobs": Self::group_bool_attribute(group, IppAttribute::PRINTER_IS_ACCEPTING_JOBS),
            "queued_job_count": Self::group_i32_attribute(group, IppAttribute::QUEUED_JOB_COUNT),
            "state": state_code.and_then(Self::printer_state_label),
            "state_code": state_code,
            "state_message": Self::group_string_attribute(group, IppAttribute::PRINTER_STATE_MESSAGE),
            "state_reasons": Self::group_string_array_attribute(group, IppAttribute::PRINTER_STATE_REASONS),
            "document_formats": Self::group_string_array_attribute(group, IppAttribute::DOCUMENT_FORMAT_SUPPORTED),
        }))
    }

    fn response_string_attribute(response: &IppRequestResponse, name: &str) -> Option<String> {
        response
            .attributes()
            .groups()
            .iter()
            .find_map(|group| Self::group_string_attribute(group, name))
    }

    fn response_string_array_attribute(response: &IppRequestResponse, name: &str) -> Vec<String> {
        response
            .attributes()
            .groups()
            .iter()
            .find_map(|group| {
                let values = Self::group_string_array_attribute(group, name);
                (!values.is_empty()).then_some(values)
            })
            .unwrap_or_default()
    }

    fn response_i32_attribute(response: &IppRequestResponse, name: &str) -> Option<i32> {
        response
            .attributes()
            .groups()
            .iter()
            .find_map(|group| Self::group_i32_attribute(group, name))
    }

    fn group_attribute<'a>(group: &'a IppAttributeGroup, name: &str) -> Option<&'a IppAttribute> {
        let key = IppName::new(name).ok()?;
        group.attributes().get(&key)
    }

    fn group_string_attribute(group: &IppAttributeGroup, name: &str) -> Option<String> {
        Self::group_attribute(group, name).and_then(|attribute| {
            Self::ipp_value_strings(attribute.value())
                .into_iter()
                .next()
        })
    }

    fn group_string_array_attribute(group: &IppAttributeGroup, name: &str) -> Vec<String> {
        Self::group_attribute(group, name)
            .map(|attribute| Self::ipp_value_strings(attribute.value()))
            .unwrap_or_default()
    }

    fn group_i32_attribute(group: &IppAttributeGroup, name: &str) -> Option<i32> {
        Self::group_attribute(group, name).and_then(|attribute| {
            attribute
                .value()
                .as_integer()
                .copied()
                .or_else(|| attribute.value().as_enum().copied())
        })
    }

    fn group_bool_attribute(group: &IppAttributeGroup, name: &str) -> Option<bool> {
        Self::group_attribute(group, name)
            .and_then(|attribute| attribute.value().as_boolean().copied())
    }

    fn ipp_value_strings(value: &IppValue) -> Vec<String> {
        if let Some(values) = value.as_array() {
            return values
                .iter()
                .flat_map(Self::ipp_value_strings)
                .collect::<Vec<_>>();
        }

        value
            .as_keyword()
            .map(ToString::to_string)
            .or_else(|| value.as_name_without_language().map(ToString::to_string))
            .or_else(|| value.as_text_without_language().map(ToString::to_string))
            .or_else(|| value.as_uri().map(ToString::to_string))
            .or_else(|| value.as_mime_media_type().map(ToString::to_string))
            .or_else(|| value.as_charset().map(ToString::to_string))
            .or_else(|| value.as_natural_language().map(ToString::to_string))
            .or_else(|| value.as_integer().map(ToString::to_string))
            .or_else(|| value.as_enum().map(ToString::to_string))
            .or_else(|| value.as_boolean().map(ToString::to_string))
            .map(|value| vec![value])
            .unwrap_or_default()
    }

    fn printer_state_label(state: i32) -> Option<&'static str> {
        PrinterState::from_i32(state).map(|state| match state {
            PrinterState::Idle => "idle",
            PrinterState::Processing => "processing",
            PrinterState::Stopped => "stopped",
        })
    }

    fn job_state_label(state: i32) -> Option<&'static str> {
        JobState::from_i32(state).map(|state| match state {
            JobState::Pending => "pending",
            JobState::PendingHeld => "pending-held",
            JobState::Processing => "processing",
            JobState::ProcessingStopped => "processing-stopped",
            JobState::Canceled => "canceled",
            JobState::Aborted => "aborted",
            JobState::Completed => "completed",
        })
    }

    fn job_state_is_terminal(state: i32) -> bool {
        matches!(
            JobState::from_i32(state),
            Some(JobState::Canceled | JobState::Aborted | JobState::Completed)
        )
    }
}

#[async_trait]
impl HalDriver for PrinterDriver {
    fn name(&self) -> &str {
        "printer"
    }

    fn required_permission(&self) -> (&str, PermissionOp) {
        ("hardware.printer", PermissionOp::Execute)
    }

    fn device_key(&self, params: &Value) -> Option<String> {
        match params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("list")
        {
            "list" => None,
            _ => params
                .get("printer")
                .and_then(Value::as_str)
                .filter(|printer| Self::validate_printer_name(printer).is_ok())
                .map(|printer| format!("printer:{printer}")),
        }
    }

    async fn query(&self, params: Value) -> Result<Value, AgentOSError> {
        match params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("list")
        {
            "list" => self.list_printers(&params).await,
            "print" => self.print_document(&params).await,
            "status" => self.job_status(&params).await,
            "cancel" => self.cancel_job(&params).await,
            other => Err(AgentOSError::HalError(format!(
                "Unknown printer action: {other}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_key_uses_safe_printer_names() {
        let driver = PrinterDriver::new();
        assert_eq!(
            driver.device_key(&json!({ "action": "print", "printer": "office-hp" })),
            Some("printer:office-hp".to_string())
        );
    }

    #[test]
    fn device_key_rejects_unsafe_printer_names() {
        let driver = PrinterDriver::new();
        assert_eq!(
            driver.device_key(&json!({ "action": "print", "printer": "../hp" })),
            None
        );
        assert_eq!(
            driver.device_key(&json!({ "action": "print", "printer": "hp main" })),
            None
        );
    }

    #[tokio::test]
    async fn rate_limit_rejects_eleventh_job_in_window() {
        let driver = PrinterDriver::new();
        let params = json!({ "agent_id": "agent-printer" });
        for _ in 0..DEFAULT_MAX_JOBS_PER_HOUR {
            driver.check_rate_limit(&params).await.unwrap();
        }

        let err = driver.check_rate_limit(&params).await.unwrap_err();
        assert!(matches!(err, AgentOSError::RateLimited { .. }));
    }

    #[test]
    fn server_uri_rejects_all_non_default_hosts() {
        let driver = PrinterDriver::new();
        // Only the configured default CUPS authority is reachable. Private,
        // loopback, link-local AND public hosts are all rejected: private/
        // loopback would be an SSRF probe, public an exfiltration channel.
        for blocked in [
            "http://10.0.0.5:631",
            "http://169.254.169.254/latest/meta-data",
            "http://127.0.0.1:8080",
            "ipp://192.168.1.50:631",
            "http://[::1]:631",
            // public hosts are blocked too — exfiltration channel
            "ipp://printserver.example.com:631",
            "https://attacker.example.com/ipp",
        ] {
            let err = driver
                .server_uri_from_params(&json!({ "server_uri": blocked }))
                .expect_err("non-default host must be blocked");
            assert!(
                err.to_string().contains("SSRF"),
                "expected SSRF error for {blocked}, got: {err}"
            );
        }

        // The configured default CUPS authority is always allowed, and the
        // no-URI default path never trips the guard.
        driver
            .server_uri_from_params(&json!({ "server_uri": "ipp://localhost:631" }))
            .expect("default CUPS authority must be allowed");
        driver
            .server_uri_from_params(&json!({}))
            .expect("default server uri must be allowed");

        // The same guard covers agent-supplied printer_uri (private and public).
        for blocked in [
            "ipp://10.1.2.3:631/printers/hp",
            "ipp://printserver.example.com:631/printers/hp",
        ] {
            let err = driver
                .printer_uri_from_params(&json!({ "printer_uri": blocked }))
                .expect_err("non-default printer_uri must be blocked");
            assert!(err.to_string().contains("SSRF"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn document_path_rejects_symlink() {
        let dir = tempfile::tempdir().expect("tempdir");
        let real = dir.path().join("doc.pdf");
        std::fs::write(&real, b"%PDF-1.4").expect("write doc");
        let link = dir.path().join("link.pdf");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");

        let driver = PrinterDriver::new();
        // The real file passes.
        driver
            .document_path_from_params(&json!({ "document_path": real.to_str().unwrap() }))
            .expect("regular file should pass");
        // The symlink is refused.
        let err = driver
            .document_path_from_params(&json!({ "document_path": link.to_str().unwrap() }))
            .expect_err("symlink must be refused");
        assert!(
            err.to_string().contains("symlink"),
            "expected symlink rejection, got: {err}"
        );
    }

    #[test]
    fn path_traversal_is_blocked() {
        let driver = PrinterDriver::new();
        let err = driver
            .document_path_from_params(&json!({ "document_path": "../etc/passwd" }))
            .unwrap_err();

        assert!(
            matches!(err, AgentOSError::HalError(message) if message.contains("Path traversal blocked"))
        );
    }

    #[test]
    fn malformed_printer_uri_is_rejected_when_deriving_server_uri() {
        let driver = PrinterDriver::new();
        let err = driver
            .server_uri_from_params(&json!({ "printer_uri": "ipp://[bad-uri" }))
            .unwrap_err();

        assert!(
            matches!(err, AgentOSError::HalError(message) if message.contains("Invalid 'printer_uri'"))
        );
    }
}
