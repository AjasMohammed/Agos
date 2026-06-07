use crate::agent_registry::AgentRegistry;
use crate::background_pool::BackgroundPool;
use crate::config::{load_config, KernelConfig};
use crate::context::ContextManager;
use crate::event_dispatch::emit_signed_event;
use crate::schedule_manager::ScheduleManager;
use crate::scheduler::TaskScheduler;
use crate::tool_registry::ToolRegistry;
use agentos_audit::AuditLog;
use agentos_bus::BusServer;
use agentos_capability::profiles::ProfileManager;
use agentos_capability::CapabilityEngine;
#[cfg(feature = "audio")]
use agentos_hal::drivers::audio::AudioDriver;
#[cfg(feature = "bluetooth")]
use agentos_hal::drivers::bluetooth::BluetoothDriver;
#[cfg(feature = "display")]
use agentos_hal::drivers::display::DisplayDriver;
#[cfg(feature = "homeassistant")]
use agentos_hal::drivers::homeassistant::HomeAssistantDriver;
#[cfg(feature = "mqtt")]
use agentos_hal::drivers::mqtt::MqttDriver;
#[cfg(feature = "printer")]
use agentos_hal::drivers::printer::PrinterDriver;
#[cfg(feature = "raw-usb")]
use agentos_hal::drivers::raw_usb::RawUsbDriver;
#[cfg(feature = "usb-storage")]
use agentos_hal::drivers::usb_storage::UsbStorageDriver;
#[cfg(feature = "webcam")]
use agentos_hal::drivers::webcam::WebcamDriver;
use agentos_hal::{
    discover_available_devices,
    drivers::{
        gpu::GpuDriver, log_reader::LogReaderDriver, network::NetworkDriver,
        process::ProcessDriver, sensor::SensorDriver, storage::StorageDriver, system::SystemDriver,
    },
    DeviceAccessGate, DeviceStatus, HalEventSink, HalOperation, HardwareAbstractionLayer,
    HardwareRegistry,
};
use agentos_llm::{LLMCore, NoopImageResolver};
use agentos_memory::Embedder;
use agentos_pipeline::{PipelineEngine, PipelineStore};
use agentos_sandbox::SandboxExecutor;
use agentos_tools::runner::ToolRunner;
use agentos_tools::traits::ToolExecutionContext;
use agentos_types::*;
use agentos_vault::{SecretsVault, ZeroizingString};
use agentos_wasm::WasmToolExecutor;
use async_trait::async_trait;
use rand::RngCore;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

struct KernelHalEventSink {
    capability_engine: Arc<CapabilityEngine>,
    audit: Arc<AuditLog>,
    event_sender: tokio::sync::mpsc::Sender<agentos_types::EventMessage>,
}

struct KernelDeviceAccessGate {
    registry: Arc<HardwareRegistry>,
    escalation_manager: Arc<crate::escalation::EscalationManager>,
    audit: Arc<AuditLog>,
}

impl KernelDeviceAccessGate {
    fn new(
        registry: Arc<HardwareRegistry>,
        escalation_manager: Arc<crate::escalation::EscalationManager>,
        audit: Arc<AuditLog>,
    ) -> Self {
        Self {
            registry,
            escalation_manager,
            audit,
        }
    }

    fn default_status_for(device_type: &str) -> DeviceStatus {
        match device_type {
            "cpu" | "memory" => DeviceStatus::Approved,
            _ => DeviceStatus::Pending,
        }
    }

    fn default_status_for_discovered_device(device_id: &str, device_type: &str) -> DeviceStatus {
        if device_type != "block-device" {
            return Self::default_status_for(device_type);
        }

        let Some(device_name) = device_id.strip_prefix("storage:") else {
            return DeviceStatus::Pending;
        };
        let removable_path = Path::new("/sys/block").join(device_name).join("removable");
        match std::fs::read_to_string(removable_path) {
            Ok(value) if value.trim() == "1" => DeviceStatus::Pending,
            Ok(_) => DeviceStatus::Approved,
            Err(_) => DeviceStatus::Pending,
        }
    }

    fn audit(
        &self,
        event_type: agentos_audit::AuditEventType,
        severity: agentos_audit::AuditSeverity,
        agent_id: Option<AgentID>,
        task_id: Option<TaskID>,
        details: serde_json::Value,
    ) -> Result<(), AgentOSError> {
        self.audit.append(agentos_audit::AuditEntry {
            timestamp: chrono::Utc::now(),
            trace_id: TraceID::new(),
            event_type,
            agent_id,
            task_id,
            tool_id: None,
            details,
            severity,
            reversible: false,
            rollback_ref: None,
        })
    }
}

impl KernelHalEventSink {
    fn new(
        capability_engine: Arc<CapabilityEngine>,
        audit: Arc<AuditLog>,
        event_sender: tokio::sync::mpsc::Sender<agentos_types::EventMessage>,
    ) -> Self {
        Self {
            capability_engine,
            audit,
            event_sender,
        }
    }
}

#[async_trait]
impl HalEventSink for KernelHalEventSink {
    async fn emit_driver_event(
        &self,
        driver_name: &str,
        params: &Value,
        result: &Value,
        agent_id: Option<&AgentID>,
    ) -> Result<(), AgentOSError> {
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("list");

        let Some((event_type, audit_type, payload)) = (match driver_name {
            "usb-storage" => {
                let device = result
                    .get("device")
                    .or_else(|| params.get("device"))
                    .and_then(Value::as_str);

                match action {
                    "mount" => Some((
                        EventType::DeviceMounted,
                        None,
                        json!({
                            "driver": driver_name,
                            "device": device,
                            "mount_path": result.get("mount_path").and_then(Value::as_str),
                        }),
                    )),
                    "unmount" => Some((
                        EventType::DeviceUnmounted,
                        None,
                        json!({
                            "driver": driver_name,
                            "device": device,
                        }),
                    )),
                    "eject" => Some((
                        EventType::DeviceEjected,
                        None,
                        json!({
                            "driver": driver_name,
                            "device": device,
                        }),
                    )),
                    _ => None,
                }
            }
            "printer" => {
                let printer = result
                    .get("printer")
                    .or_else(|| params.get("printer"))
                    .and_then(Value::as_str);
                let job_id = result.get("job_id").and_then(Value::as_i64);

                match action {
                    "print" => Some((
                        EventType::PrintJobSubmitted,
                        Some(agentos_audit::AuditEventType::PrintJobSubmitted),
                        json!({
                            "driver": driver_name,
                            "printer": printer,
                            "printer_uri": result.get("printer_uri").and_then(Value::as_str),
                            "job_id": job_id,
                            "job_name": result.get("job_name").and_then(Value::as_str),
                            "document_name": result.get("document_name").and_then(Value::as_str),
                        }),
                    )),
                    "cancel" => Some((
                        EventType::PrintJobCancelled,
                        Some(agentos_audit::AuditEventType::PrintJobCancelled),
                        json!({
                            "driver": driver_name,
                            "printer": printer,
                            "printer_uri": result.get("printer_uri").and_then(Value::as_str),
                            "job_id": job_id.or_else(|| params.get("job_id").and_then(Value::as_i64)),
                        }),
                    )),
                    _ => None,
                }
            }
            "audio" => match action {
                "capture" => {
                    let source = result
                        .get("source")
                        .or_else(|| params.get("source"))
                        .or_else(|| params.get("node_id"))
                        .and_then(Value::as_str);
                    let path = result.get("audio_path").and_then(Value::as_str);
                    let sample_rate = result.get("sample_rate").and_then(Value::as_u64);
                    let duration = result.get("duration_seconds").and_then(Value::as_u64);

                    for (event_type, event_payload) in [
                        (
                            EventType::AudioCaptureStarted,
                            json!({
                                "driver": driver_name,
                                "source": source,
                                "audio_path": path,
                                "sample_rate": sample_rate,
                                "duration_seconds": duration,
                            }),
                        ),
                        (
                            EventType::AudioCaptureStopped,
                            json!({
                                "driver": driver_name,
                                "source": source,
                                "audio_path": path,
                                "sample_rate": sample_rate,
                                "duration_seconds": duration,
                            }),
                        ),
                    ] {
                        emit_signed_event(
                            &self.capability_engine,
                            &self.audit,
                            &self.event_sender,
                            event_type,
                            EventSource::HardwareAbstractionLayer,
                            EventSeverity::Info,
                            event_payload,
                            0,
                            TraceID::new(),
                            agent_id.cloned(),
                            None,
                        );
                    }

                    return Ok(());
                }
                "playback" => Some((
                    EventType::AudioPlaybackStarted,
                    None,
                    json!({
                        "driver": driver_name,
                        "sink": result.get("sink").or_else(|| params.get("sink")).and_then(Value::as_str),
                        "audio_path": result.get("audio_path").or_else(|| params.get("audio_path")).and_then(Value::as_str),
                    }),
                )),
                _ => None,
            },
            "webcam" => match action {
                "capture" => Some((
                    EventType::WebcamCaptureStopped,
                    None,
                    json!({
                        "driver": driver_name,
                        "device": result.get("device").or_else(|| params.get("device")).and_then(Value::as_str),
                        "image_path": result.get("image_path").and_then(Value::as_str),
                        "width": result.get("width").and_then(Value::as_u64),
                        "height": result.get("height").and_then(Value::as_u64),
                        "format": result.get("format").and_then(Value::as_str),
                    }),
                )),
                "burst" => Some((
                    EventType::WebcamCaptureStopped,
                    None,
                    json!({
                        "driver": driver_name,
                        "device": result.get("device").or_else(|| params.get("device")).and_then(Value::as_str),
                        "count": result.get("count").and_then(Value::as_u64),
                        "interval_ms": result.get("interval_ms").and_then(Value::as_u64),
                        "first_image_path": result
                            .get("frames")
                            .and_then(Value::as_array)
                            .and_then(|frames| frames.first())
                            .and_then(|frame| frame.get("image_path"))
                            .and_then(Value::as_str),
                    }),
                )),
                _ => None,
            },
            "bluetooth" => match action {
                "scan" => Some((
                    EventType::BluetoothScanStarted,
                    None,
                    json!({
                        "driver": driver_name,
                        "adapter": result.get("adapter").or_else(|| params.get("adapter")).and_then(Value::as_str),
                        "scan_duration_seconds": result.get("scan_duration_seconds").and_then(Value::as_u64),
                        "device_count": result.get("devices").and_then(Value::as_array).map(|devices| devices.len()),
                    }),
                )),
                "pair" => Some((
                    EventType::BluetoothPairRequested,
                    None,
                    json!({
                        "driver": driver_name,
                        "adapter": result.get("adapter").or_else(|| params.get("adapter")).and_then(Value::as_str),
                        "address": result.get("address").or_else(|| params.get("address")).and_then(Value::as_str),
                        "name": result.get("name").and_then(Value::as_str),
                    }),
                )),
                "connect" => Some((
                    EventType::BluetoothConnected,
                    None,
                    json!({
                        "driver": driver_name,
                        "adapter": result.get("adapter").or_else(|| params.get("adapter")).and_then(Value::as_str),
                        "address": result.get("address").or_else(|| params.get("address")).and_then(Value::as_str),
                        "name": result.get("name").and_then(Value::as_str),
                    }),
                )),
                _ => None,
            },
            "display" => {
                let output = result
                    .get("operation")
                    .and_then(|operation| operation.get("output"))
                    .or_else(|| params.get("output"))
                    .and_then(Value::as_str);
                let config_id = result.get("config_id").and_then(Value::as_str);

                match action {
                    "set_mode" | "set_position" | "set_scale" | "enable" | "disable" => Some((
                        EventType::DisplayConfigApplied,
                        Some(agentos_audit::AuditEventType::DisplayConfigApplied),
                        json!({
                            "driver": driver_name,
                            "output": output,
                            "config_id": config_id,
                            "operation": result.get("operation"),
                            "auto_revert_timeout_secs": result.get("auto_revert_timeout_secs"),
                            "confirmation_deadline": result.get("confirmation_deadline"),
                        }),
                    )),
                    "revert" => Some((
                        EventType::DisplayConfigReverted,
                        Some(agentos_audit::AuditEventType::DisplayConfigReverted),
                        json!({
                            "driver": driver_name,
                            "output": output,
                            "config_id": config_id,
                            "operation": result.get("operation"),
                            "reverted_at": result.get("reverted_at"),
                        }),
                    )),
                    _ => None,
                }
            }
            "raw-usb" => match action {
                "open" => {
                    let payload = RawUsbDeviceOpened {
                        device_key: result["device_key"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        vendor_id: result["vendor_id"].as_str().unwrap_or_default().to_string(),
                        product_id: result["product_id"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        interface: result["interface"].as_u64().unwrap_or(0) as u8,
                        alt_setting: result["alt_setting"].as_u64().unwrap_or(0) as u8,
                        detach_kernel_driver: result["detach_kernel_driver"]
                            .as_bool()
                            .unwrap_or(false),
                    };
                    Some((
                        EventType::RawUsbDeviceOpened,
                        None,
                        serde_json::to_value(payload).unwrap_or_default(),
                    ))
                }
                "read" | "write" | "control" => {
                    let payload = RawUsbTransfer {
                        action: action.to_string(),
                        device_key: result["device_key"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        vendor_id: result["vendor_id"].as_str().unwrap_or_default().to_string(),
                        product_id: result["product_id"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        interface: result["interface"].as_u64().unwrap_or(0) as u8,
                        transfer_kind: result
                            .get("transfer_kind")
                            .and_then(Value::as_str)
                            .map(ToString::to_string),
                        endpoint: result
                            .get("endpoint")
                            .and_then(Value::as_str)
                            .map(ToString::to_string),
                        direction: result
                            .get("direction")
                            .and_then(Value::as_str)
                            .map(ToString::to_string),
                        bytes_read: result.get("bytes_read").and_then(Value::as_u64).or_else(
                            || {
                                result
                                    .get("result")
                                    .and_then(|inner| inner.get("bytes_read"))
                                    .and_then(Value::as_u64)
                            },
                        ),
                        bytes_written: result.get("bytes_written").and_then(Value::as_u64).or_else(
                            || {
                                result
                                    .get("result")
                                    .and_then(|inner| inner.get("bytes_written"))
                                    .and_then(Value::as_u64)
                            },
                        ),
                    };
                    Some((
                        EventType::RawUsbTransferCompleted,
                        None,
                        serde_json::to_value(payload).unwrap_or_default(),
                    ))
                }
                _ => None,
            },
            _ => None,
        }) else {
            return Ok(());
        };

        if let Some(audit_type) = audit_type {
            let _ = self.audit.append(agentos_audit::AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: TraceID::new(),
                event_type: audit_type,
                agent_id: agent_id.cloned(),
                task_id: None,
                tool_id: None,
                details: payload.clone(),
                severity: agentos_audit::AuditSeverity::Info,
                reversible: false,
                rollback_ref: None,
            });
        }

        emit_signed_event(
            &self.capability_engine,
            &self.audit,
            &self.event_sender,
            event_type,
            EventSource::HardwareAbstractionLayer,
            EventSeverity::Info,
            payload,
            0,
            TraceID::new(),
            agent_id.cloned(),
            None,
        );

        Ok(())
    }
}

#[async_trait]
impl DeviceAccessGate for KernelDeviceAccessGate {
    async fn check(
        &self,
        agent_id: &AgentID,
        task_id: &TaskID,
        device_id: &str,
        device_type: &str,
        operation: HalOperation,
    ) -> Result<(), AgentOSError> {
        if self.registry.get_device_status(device_id).is_none() {
            self.registry.register_device(
                device_id,
                device_type,
                Self::default_status_for(device_type),
            );
        }

        let Some(device) = self.registry.get_device(device_id) else {
            return Err(AgentOSError::HalError(format!(
                "Device '{}' was not found after registration",
                device_id
            )));
        };

        match device.status {
            DeviceStatus::Approved if device.denied_to.contains(agent_id) => {
                self.audit(
                    agentos_audit::AuditEventType::DeviceAccessDenied,
                    agentos_audit::AuditSeverity::Warn,
                    Some(*agent_id),
                    Some(*task_id),
                    json!({
                        "device_id": device_id,
                        "device_type": device.device_type,
                        "operation": operation.to_string(),
                        "reason": "agent-specific device denial",
                    }),
                )?;
                Err(AgentOSError::PermissionDenied {
                    resource: device_id.to_string(),
                    operation: "device_access".to_string(),
                })
            }
            DeviceStatus::Approved
                if device.granted_to.is_empty() || device.granted_to.contains(agent_id) =>
            {
                self.audit(
                    agentos_audit::AuditEventType::DeviceAccessGranted,
                    agentos_audit::AuditSeverity::Info,
                    Some(*agent_id),
                    Some(*task_id),
                    json!({
                        "device_id": device_id,
                        "device_type": device.device_type,
                        "operation": operation.to_string(),
                    }),
                )?;
                Ok(())
            }
            DeviceStatus::Approved | DeviceStatus::Pending => {
                let (escalation_id, created) = self
                    .escalation_manager
                    .create_device_access_escalation(
                        *task_id,
                        *agent_id,
                        device_id,
                        &operation.to_string(),
                        TraceID::new(),
                    )
                    .await;

                if created {
                    self.audit(
                        agentos_audit::AuditEventType::DeviceAccessEscalated,
                        agentos_audit::AuditSeverity::Warn,
                        Some(*agent_id),
                        Some(*task_id),
                        json!({
                            "device_id": device_id,
                            "device_type": device.device_type,
                            "operation": operation.to_string(),
                            "escalation_id": escalation_id,
                        }),
                    )?;
                }

                Err(AgentOSError::DeviceAccessPending {
                    device_id: device_id.to_string(),
                    escalation_id: escalation_id.to_string(),
                })
            }
            DeviceStatus::Quarantined => {
                self.audit(
                    agentos_audit::AuditEventType::DeviceAccessDenied,
                    agentos_audit::AuditSeverity::Warn,
                    Some(*agent_id),
                    Some(*task_id),
                    json!({
                        "device_id": device_id,
                        "device_type": device.device_type,
                        "operation": operation.to_string(),
                        "reason": "device quarantined",
                    }),
                )?;
                Err(AgentOSError::DeviceQuarantined(device_id.to_string()))
            }
        }
    }
}

/// Per-agent, mode-bucketed view of which host directories file tools may
/// touch. Produced by [`Kernel::workspace_paths_for_agent`] at task setup
/// time; the three lists are baked into [`agentos_tools::ToolExecutionContext`]
/// so each tool consults the bucket matching the operation it performs.
#[derive(Debug, Clone, Default)]
pub struct AgentWorkspacePaths {
    pub read: Vec<PathBuf>,
    pub writable: Vec<PathBuf>,
    pub executable: Vec<PathBuf>,
}

pub struct Kernel {
    pub config: KernelConfig,
    pub audit: Arc<AuditLog>,
    pub vault: Arc<SecretsVault>,
    pub capability_engine: Arc<CapabilityEngine>,
    pub scheduler: Arc<TaskScheduler>,
    pub context_manager: Arc<ContextManager>,
    pub context_compiler: Arc<crate::context_compiler::ContextCompiler>,
    pub tool_registry: Arc<RwLock<ToolRegistry>>,
    pub agent_registry: Arc<RwLock<AgentRegistry>>,
    pub bus: Arc<BusServer>,
    pub tool_runner: Arc<ToolRunner>,
    /// Live tool catalogue shared with agent-manual. Refreshed on tool install/remove.
    pub tool_summaries: agentos_tools::agent_manual::SharedToolSummaries,
    /// Per-agent tool usage rankings (SQLite-backed, spawn_blocking writes).
    pub tool_usage: Arc<crate::tool_usage_store::ToolUsageStore>,
    pub sandbox: Arc<SandboxExecutor>,
    pub router: Arc<crate::router::TaskRouter>,
    pub active_llms: Arc<RwLock<HashMap<AgentID, Arc<dyn LLMCore>>>>,
    /// Resolves chat `ImageSource::FileRef` to base64; replaced by the web UI with a file-store implementation.
    pub image_resolver: std::sync::RwLock<Arc<dyn agentos_llm::ImageResolver>>,
    /// Persists inbound channel media (Telegram photos/docs/voice). Replaced by
    /// the web UI with a FileStore-backed sink. `Arc`-wrapped so the InboundRouter
    /// shares the same slot and sees a post-boot `set_attachment_sink`.
    pub attachment_sink: Arc<std::sync::RwLock<Arc<dyn crate::attachment_sink::AttachmentSink>>>,
    pub message_bus: Arc<crate::agent_message_bus::AgentMessageBus>,
    pub profile_manager: Arc<ProfileManager>,
    pub episodic_memory: Arc<agentos_memory::EpisodicStore>,
    pub semantic_memory: Arc<agentos_memory::SemanticStore>,
    pub procedural_memory: Arc<agentos_memory::ProceduralStore>,
    pub retrieval_gate: Arc<crate::retrieval_gate::RetrievalGate>,
    pub retrieval_executor: Arc<crate::retrieval_gate::RetrievalExecutor>,
    pub memory_extraction: Arc<crate::memory_extraction::MemoryExtractionEngine>,
    pub consolidation_engine: Arc<crate::consolidation::ConsolidationEngine>,
    pub memory_blocks: Arc<crate::memory_blocks::MemoryBlockStore>,
    pub context_memory_store: Arc<crate::context_memory_store::ContextMemoryStore>,
    pub scratchpad_store: Arc<agentos_scratch::ScratchpadStore>,
    /// SQLite-backed store for uploaded/inbound files. Owned by the kernel so
    /// both the web UI and the REST API (`KernelService`) share one instance;
    /// also backs the `AttachmentSink` for inbound channel media.
    pub file_store: Arc<crate::file_store::FileStore>,
    /// SQLite-backed chat-session store (shared by the web UI + REST API).
    pub chat_store: Arc<crate::chat_store::ChatStore>,
    /// SQLite-backed agent-to-agent conversation store (shared by web + API).
    pub convo_store: Arc<crate::convo_store::ConvoStore>,
    /// SQLite-backed user-profile/preference store.
    pub user_profile_store: Arc<crate::user_profile_store::UserProfileStore>,
    /// Version-gated cache of the rendered L0 `## User Profile` block (Phase 2).
    /// Holds `(profile_store_version, rendered_block)`. The read-back path reuses
    /// the cached string while the profile version is unchanged, keeping the
    /// prompt-cached prefix byte-identical across iterations. Invalidated
    /// automatically when any profile mutation bumps the store version.
    pub user_profile_l0_cache: std::sync::Mutex<Option<(u64, String)>>,
    /// Background interest aggregator (Phase 3). Decays behavioral signals into
    /// `user_interests.db`; zero task-context cost (driven only by the periodic
    /// tick + `on_task_completed`). Consumed by the Phase 4 recommendation engine.
    pub interest_model: Arc<crate::interest_model::InterestModel>,
    /// Proactive recommendation engine (Phase 4). Generates + delivers out-of-loop
    /// tips from the interest model; zero task-context cost.
    pub recommendation_engine: Arc<crate::recommendation_engine::RecommendationEngine>,
    /// Feedback-loop processor (Phase 5). Applies accept/dismiss/restate signals to
    /// the interest model and profile store; also runs the hourly decay/archival sweep.
    pub feedback_processor: Arc<crate::personalization_feedback::FeedbackProcessor>,
    pub skill_registry: Arc<RwLock<agentos_skills::SkillRegistry>>,
    pub schedule_manager: Arc<ScheduleManager>,
    pub background_pool: Arc<BackgroundPool>,
    pub hal: Arc<HardwareAbstractionLayer>,
    pub hardware_registry: Arc<HardwareRegistry>,
    pub schema_registry: Arc<crate::schema_registry::SchemaRegistry>,
    pub pipeline_engine: Arc<PipelineEngine>,
    pub intent_validator: Arc<crate::intent_validator::IntentValidator>,
    pub escalation_manager: Arc<crate::escalation::EscalationManager>,
    pub cost_tracker: Arc<crate::cost_tracker::CostTracker>,
    pub risk_classifier: Arc<crate::risk_classifier::RiskClassifier>,
    /// Classifies a task prompt into tool categories for native-array scoping (Phase 3).
    pub tool_classifier: Arc<dyn crate::tool_scoping::TaskToolClassifier>,
    pub identity_manager: Arc<crate::identity::IdentityManager>,
    pub injection_scanner: Arc<crate::injection_scanner::InjectionScanner>,
    pub resource_arbiter: Arc<crate::resource_arbiter::ResourceArbiter>,
    pub checkpoint_store: Arc<crate::checkpoint_store::CheckpointStore>,
    pub workspace_grants: Arc<crate::workspace_grant_store::WorkspaceGrantRegistry>,
    /// Atomic, crash-safe task ownership claims. A task is claimed before
    /// dispatch (single-owner guarantee) and released on terminal completion;
    /// expired leases are swept by the `TimeoutChecker`.
    pub task_checkout_store: Arc<crate::task_checkout_store::TaskCheckoutStore>,
    /// Opt-in claude-code session-resume cache (`[llm] claude_code_resume`).
    /// `None` when resume is disabled (the default) — the adapter then sends the
    /// full flattened context every turn. The store is a pure cache: deleted on
    /// task completion, and every resume is fingerprint-guarded.
    pub claude_session_lookup: Option<Arc<crate::claude_session_store::KernelClaudeSessionLookup>>,
    /// Active approval-mode resolver. Populated during boot after the
    /// `ApprovalHook` is registered; `None` only during the narrow window
    /// between Kernel struct construction and hook registration. The CLI
    /// approval commands and the `ConfigWatcher` reload path both reach
    /// through this field to mutate the live mode.
    pub approval_mode_resolver: Option<Arc<crate::hooks::ApprovalModeResolver>>,
    /// Operator-curated learned-allow policy. `None` when the kernel
    /// chose not to open the policy DB (e.g. file lock failure) — the
    /// approval hook still functions, just without learned overrides.
    pub approval_policy_matcher: Option<Arc<crate::approval_policy_store::ApprovalPolicyMatcher>>,
    pub mcp_attachment_store: Arc<crate::mcp_attachment_store::McpAttachmentStore>,
    pub user_pref_proposal_store: Arc<crate::user_pref_proposals::UserPrefProposalStore>,
    pub snapshot_manager: Arc<crate::snapshot::SnapshotManager>,
    pub trace_collector: Arc<crate::trace_collector::TraceCollector>,
    pub rpc_manager: Arc<crate::rpc_manager::RpcManager>,
    pub otel: Arc<crate::otel_exporter::OtelExporter>,
    pub event_bus: Arc<crate::event_bus::EventBus>,
    /// Unified notification router — dispatches UserMessages to delivery adapters
    /// and persists them to the user inbox.
    pub notification_router: Arc<crate::notification_router::NotificationRouter>,
    /// Agent-facing notification inbox for scheduled/event/background deliveries.
    pub agent_inbox: Arc<crate::agent_inbox::AgentInbox>,
    /// Agent-facing peer message inbox.
    pub agent_message_inbox: Arc<crate::agent_message_inbox::AgentMessageInbox>,
    /// Writes agent inbox/message entries from kernel delivery paths.
    pub agent_inbox_writer: Arc<crate::agent_inbox_writer::AgentInboxWriter>,
    /// Registry of user-connected bidirectional channels (Phase 6).
    pub channel_registry: Arc<crate::user_channel_registry::UserChannelRegistry>,
    /// Manages background listener tasks for bidirectional channels (Phase 6).
    pub channel_listener_registry: Arc<crate::user_channel_registry::ChannelListenerRegistry>,
    /// Live snapshot of connected channels surfaced into the system prompt's
    /// `## Channels` block and the agent-manual filter. Refreshed on every
    /// channel register/deregister via `refresh_connected_channels_snapshot`.
    pub connected_channels_snapshot: agentos_tools::agent_manual::SharedConnectedChannels,
    /// Live snapshot of installed skills surfaced by the agent-manual `skills`
    /// section (inventory + drill-down). Refreshed on every skill install/remove
    /// via `refresh_installed_skills_snapshot`.
    pub installed_skills_snapshot: agentos_tools::agent_manual::SharedInstalledSkills,
    /// Sender for inbound messages from channel listeners to InboundRouter (Phase 6).
    pub inbound_tx: tokio::sync::mpsc::Sender<crate::notification_router::InboundMessage>,
    /// Resolves channel inbound chat to `chat_infer_with_tools` after `wire_inbound_chat_bridge`.
    pub inbound_chat_bridge: Arc<crate::channel_chat_bridge::KernelChatBridge>,
    /// Pending receiver consumed once by `wire_inbound_chat_bridge` to spawn the InboundRouter.
    /// Stored here so the router is guaranteed to start after the bridge is wired.
    pub(crate) pending_inbound_rx: std::sync::Mutex<
        Option<tokio::sync::mpsc::Receiver<crate::notification_router::InboundMessage>>,
    >,
    /// Webhook secret tokens keyed by channel instance ID.
    /// Used by the API webhook handler to verify `X-Telegram-Bot-Api-Secret-Token`.
    pub webhook_secrets: Arc<RwLock<HashMap<ChannelInstanceID, String>>>,
    /// Broadcast channel for task status updates.
    /// Phase 2 SSE and external adapters subscribe via `status_update_sender.subscribe()`.
    /// Messages are silently dropped if there are no active receivers.
    /// API connector registry — routes namespaced tool calls to external services.
    pub connector_registry: Arc<agentos_connectors::ConnectorRegistry>,
    /// Container runtime — provisions and manages ephemeral compute containers.
    pub compute_runtime: Option<Arc<dyn agentos_runtime::ComputeRuntime>>,
    /// Per-agent container quota enforcement.
    pub quota_enforcer: Arc<agentos_runtime::QuotaEnforcer>,
    /// Webhook endpoint registry — manages inbound webhook endpoints for agents.
    pub webhook_registry: Arc<crate::webhook_registry::WebhookRegistry>,
    /// Webhook rate limiter — per-endpoint token bucket.
    pub webhook_throttle: Arc<crate::webhook_throttle::WebhookThrottle>,
    /// Webhook event batcher — debounces and aggregates events before agent wake-up.
    pub webhook_batcher: Arc<crate::webhook_batcher::WebhookBatcher>,
    /// Receiver for batched webhook events ready for agent task creation.
    /// Consumed once at boot by the webhook wake-up loop.
    pub(crate) webhook_batch_rx: Arc<
        tokio::sync::Mutex<Option<tokio::sync::mpsc::Receiver<crate::webhook_batcher::BatchReady>>>,
    >,
    pub status_update_sender: tokio::sync::broadcast::Sender<agentos_bus::StatusUpdate>,
    /// Lossy broadcast of coarse realtime events for WS/SSE fan-out to the control
    /// panel. Fed from `process_event` (every kernel event), consumed by the API's
    /// `WsBroadcaster::start_realtime_relay`. Capacity-bounded; old events evicted.
    pub realtime_event_sender: tokio::sync::broadcast::Sender<agentos_types::RealtimeEvent>,
    /// Task-scoped subscriptions that should be removed when a task reaches terminal state.
    pub(crate) task_scoped_subscriptions: Arc<RwLock<HashMap<TaskID, Vec<SubscriptionID>>>>,
    pub(crate) event_sender: tokio::sync::mpsc::Sender<agentos_types::EventMessage>,
    /// Receiver for event channel — owned behind a mutex so EventDispatcher can be restarted.
    pub(crate) event_receiver:
        Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<agentos_types::EventMessage>>>,
    /// Receiver for tool lifecycle notifications from ToolRegistry.
    pub(crate) tool_lifecycle_receiver: Arc<
        tokio::sync::Mutex<tokio::sync::mpsc::Receiver<crate::tool_registry::ToolLifecycleEvent>>,
    >,
    /// Receiver for communication notifications from AgentMessageBus.
    pub(crate) comm_notification_receiver: Arc<
        tokio::sync::Mutex<tokio::sync::mpsc::Receiver<crate::agent_message_bus::CommNotification>>,
    >,
    /// Receiver for schedule notifications from ScheduleManager.
    pub(crate) schedule_notification_receiver: Arc<
        tokio::sync::Mutex<
            tokio::sync::mpsc::Receiver<crate::schedule_manager::ScheduleNotification>,
        >,
    >,
    /// Receiver for resource arbiter notifications (preemption/deadlock events).
    pub(crate) arbiter_notification_receiver: Arc<
        tokio::sync::Mutex<
            tokio::sync::mpsc::Receiver<crate::resource_arbiter::ArbiterNotification>,
        >,
    >,
    /// Per-agent rate limiter: enforces command-rate limits across all connections per agent.
    pub(crate) per_agent_rate_limiter:
        Arc<tokio::sync::Mutex<crate::rate_limit::PerAgentRateLimiter>>,
    pub(crate) data_dir: PathBuf,
    /// Canonical path to the config file used to boot this kernel instance.
    pub(crate) config_path: PathBuf,
    /// Pre-canonicalized workspace paths from `tools.workspace.allowed_paths`.
    pub(crate) workspace_paths: Vec<PathBuf>,
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// MCP supervisor managing all server connections with health monitoring.
    pub mcp_supervisor: Arc<agentos_mcp::McpSupervisor>,
    /// MCP security gate for output validation, rate limiting, and audit logging.
    pub mcp_security_gate: Arc<agentos_mcp::McpSecurityGate>,
    /// Provider catalog for auto-configuring OpenAI-compatible LLM providers.
    pub provider_catalog: Arc<std::sync::RwLock<agentos_llm::ProviderCatalog>>,
    /// Path to `providers.toml` so runtime URL overrides can be persisted.
    pub(crate) catalog_path: Option<PathBuf>,
    /// Manages bidirectional channel adapters (Discord, Slack, Telegram, etc.).
    pub channel_manager: Arc<agentos_channels::manager::ChannelManager>,
    /// Receiver for inbound messages from ChannelManager adapters.
    pub(crate) channel_manager_rx: Arc<
        tokio::sync::Mutex<tokio::sync::mpsc::Receiver<agentos_channels::types::InboundMessage>>,
    >,
    /// DM pairing allowlist used by `/pair`, `/approve <id>`, and
    /// `/deny <id>` inbound commands. Shared with `ChannelBroadcastSink`
    /// so escalations only fan out to paired senders.
    pub pairing_manager: Arc<agentos_channels::pairing::PairingManager>,
    /// Hot-reloadable handle to the `host-package-install` allowlist and
    /// manager priority list. The `ConfigWatcher` reload path writes
    /// fresh values here on `[tools.host_package]` changes so revocations
    /// take effect without a kernel restart.
    ///
    /// `pub(crate)` so external callers cannot bypass the audited
    /// `Kernel::reload_host_package_policy` write path (R3 finding I2).
    pub(crate) host_package_policy: agentos_tools::host_package::HostPackagePolicy,
    /// Lifecycle hook registry — fired at task/tool/agent lifecycle points.
    pub hook_registry: Arc<crate::hooks::HookRegistry>,
    /// Plugin registry — discovers and activates plugin manifests.
    pub plugin_registry: Arc<crate::plugin_registry::PluginRegistry>,
    /// Kernel-Mediated Capabilities registry — managed capability providers.
    pub capability_registry: Arc<RwLock<crate::capability_registry::CapabilityRegistry>>,
    /// Shared storage zone table for dynamic filesystem access (KMC Phase 3).
    pub zone_table: crate::managed_storage::ZoneTable,
    /// Shared managed-process table — owned by the kernel so the
    /// `ProcessProvider` and the kernel's `ProcessCrashed` emitter share a
    /// single source of truth.
    pub process_table: crate::managed_process::ProcessTable,
    /// Dynamic capability broker for runtime capability negotiation (KMC Phase 7).
    pub capability_broker: Arc<crate::capability_broker::CapabilityBroker>,
    /// Policy engine for capability request evaluation (KMC Phase 8).
    pub policy_engine: Arc<RwLock<crate::policy_engine::PolicyEngine>>,
    /// Capability dispatcher for routing tool calls to providers (KMC).
    pub capability_dispatcher: Arc<crate::capability_dispatch::KernelCapabilityDispatcher>,
    /// Token used to signal graceful shutdown to all kernel loops.
    pub cancellation_token: CancellationToken,
    /// Set to `true` once the first `KernelShutdown` audit entry has been written.
    /// Guards against double-writes when multiple shutdown paths converge
    /// (e.g., `KernelCommand::Shutdown` writes the entry, then `cancel()` also
    /// triggers the `cancelled()` arm in `run()` which would write a second one).
    pub(crate) shutdown_audited: std::sync::atomic::AtomicBool,
    /// In-memory LRU of recently used tool names per agent (cap 10).
    /// Used to append a "Recently used: ..." hint to the L0 tool description in context.
    pub agent_tool_lru: Arc<RwLock<HashMap<AgentID, std::collections::VecDeque<String>>>>,
    /// Per-chat-session tool-call dedup cache, keyed by chat session id.
    /// Inner map keys are `(tool_name, canonical_payload_json)`. Each entry
    /// stores `(inserted_at, result)` so the cap-eviction can drop oldest
    /// entries (LRU-by-insertion). Outer tuple is `(last_touched, inner)` so
    /// `TimeoutChecker` can sweep idle sessions. Survives across
    /// `chat_infer_streaming` invocations within the same session — small
    /// models forget prior tool calls (no tool-result replay in chat history)
    /// without this. Cap: 128 entries per session.
    pub chat_session_dedup: Arc<RwLock<ChatSessionDedupMap>>,
}

/// Inner per-session dedup cache: `(tool_name, canonical_payload_json) →
/// (inserted_at, result)`.
pub type ChatSessionDedupCache = HashMap<(String, String), (std::time::Instant, serde_json::Value)>;

/// Outer kernel-wide map: `session_id → (last_touched, dedup_cache)`.
pub type ChatSessionDedupMap = HashMap<String, (std::time::Instant, ChatSessionDedupCache)>;

/// Record of a single tool call made during chat inference.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChatToolCallRecord {
    pub tool_name: String,
    pub intent_type: String,
    /// Provider-native tool call id (Anthropic `tool_use.id`, OpenAI `tool_calls[].id`,
    /// or None for Gemini / fallback paths that don't carry an id). Surfaced to
    /// operators so multi-tool turns can be traced from audit + chat history alone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub payload: serde_json::Value,
    pub result: serde_json::Value,
    pub duration_ms: u64,
}

/// Result of chat inference with tool execution.
#[derive(Debug, Clone)]
pub struct ChatInferenceResult {
    /// The final natural-language answer from the LLM.
    pub answer: String,
    /// Tool calls that were executed during inference (in order).
    pub tool_calls: Vec<ChatToolCallRecord>,
    /// Total number of LLM inference iterations.
    pub iterations: u32,
    /// Aggregate token usage across all inference iterations.
    pub tokens_used: u64,
    /// Aggregate estimated USD cost across all inference iterations.
    pub cost_usd: f64,
}

/// Events emitted during streaming chat inference.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type")]
pub enum ChatStreamEvent {
    /// Inference started — LLM is thinking.
    Thinking { iteration: u32 },
    /// An incremental text chunk from the LLM (one or more tokens).
    TextChunk { text: String },
    /// A tool call was detected; execution is starting.
    ToolStart { tool_name: String, iteration: u32 },
    /// A tool call completed.
    ToolResult {
        tool_name: String,
        result_preview: String,
        duration_ms: u64,
        success: bool,
    },
    /// The complete final response.
    Done {
        answer: String,
        tool_calls: Vec<ChatToolCallRecord>,
        iterations: u32,
        tokens_used: u64,
        cost_usd: f64,
    },
    /// An error occurred.
    Error { message: String },
}

/// Fallback chat tool-iteration cap when config is missing. Live config value
/// is `chat.max_tool_iterations` and is read per-call via `self.config.chat`.
const CHAT_MAX_TOOL_ITERATIONS_FALLBACK: u32 = 25;

const EMPTY_LLM_ANSWER_PLACEHOLDER: &str =
    "_(no response from model — the provider returned an empty answer; please retry)_";

/// Max consecutive iterations the model is allowed to spend in
/// meta-tool calls (any combination) before the chat loop aborts.
/// Four is enough to scan an index, search by keyword, and inspect a
/// candidate; a fifth iteration without invoking a real tool is the
/// loop signature observed in 2026-05-08 logs (Sandae ran
/// `search-tools×12 + describe-tool×12 + agent-manual×8` over 21
/// iterations before returning a 98-char answer). The list of meta
/// tool names is the canonical
/// [`agentos_tools::META_TOOL_NAMES`] — single source of truth so
/// the dedup-cache and streak guard cannot drift apart.
const META_TOOL_STREAK_LIMIT: u32 = 1_000_000;

/// Returns true if every tool call in the batch is a meta-tool.
/// `[]` returns false (an empty batch is not a meta-tool iteration —
/// it is a *no-op* and is handled by the text-only reset path so a
/// non-meta thinking iteration breaks the streak).
fn iteration_is_all_meta(tool_names: &[String]) -> bool {
    !tool_names.is_empty()
        && tool_names
            .iter()
            .all(|n| agentos_tools::META_TOOL_NAMES.contains(&n.as_str()))
}

pub fn resolve_boot_vault_passphrase(
    config: &KernelConfig,
) -> Result<Option<ZeroizingString>, anyhow::Error> {
    if let Ok(passphrase) = std::env::var("AGENTOS_VAULT_PASSPHRASE") {
        if !passphrase.trim().is_empty() {
            return Ok(Some(ZeroizingString::new(passphrase)));
        }
    }

    // Docker/K8s secret-mount sourcing: AGENTOS_VAULT_PASSPHRASE_FILE points at
    // a file (e.g. /run/secrets/vault_pass) whose contents are the passphrase,
    // so the secret never needs to live in the process environment. Read at
    // boot, trailing whitespace trimmed, held in ZeroizingString.
    if let Ok(passphrase_file) = std::env::var("AGENTOS_VAULT_PASSPHRASE_FILE") {
        if !passphrase_file.trim().is_empty() {
            let contents = std::fs::read_to_string(&passphrase_file).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to read AGENTOS_VAULT_PASSPHRASE_FILE ({passphrase_file}): {e}"
                )
            })?;
            let passphrase = contents.trim().to_string();
            anyhow::ensure!(
                !passphrase.is_empty(),
                "AGENTOS_VAULT_PASSPHRASE_FILE ({passphrase_file}) is empty"
            );
            return Ok(Some(ZeroizingString::new(passphrase)));
        }
    }

    let vault_path = Path::new(&config.secrets.vault_path);
    let passphrase_path = vault_passphrase_path(vault_path);

    if passphrase_path.exists() {
        let passphrase = std::fs::read_to_string(&passphrase_path)?;
        let passphrase = passphrase.trim().to_string();
        anyhow::ensure!(
            !passphrase.is_empty(),
            "Stored vault passphrase file is empty: {}",
            passphrase_path.display()
        );
        return Ok(Some(ZeroizingString::new(passphrase)));
    }

    if SecretsVault::is_initialized(vault_path) {
        anyhow::bail!(
            "Vault already exists at {} but no AGENTOS_VAULT_PASSPHRASE is set and no managed passphrase file was found at {}",
            vault_path.display(),
            passphrase_path.display()
        );
    }

    let auto_init_enabled = std::env::var("AGENTOS_AUTO_INIT_VAULT")
        .ok()
        .and_then(|value| value.parse::<bool>().ok())
        .unwrap_or(false);
    if !auto_init_enabled {
        return Ok(None);
    }

    if let Some(parent) = passphrase_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let generated = generate_vault_passphrase();
    persist_generated_passphrase(&passphrase_path, &generated)?;
    let persisted = std::fs::read_to_string(&passphrase_path)?;
    let persisted = persisted.trim().to_string();
    anyhow::ensure!(
        !persisted.is_empty(),
        "Stored vault passphrase file is empty: {}",
        passphrase_path.display()
    );
    tracing::warn!(
        vault_path = %vault_path.display(),
        passphrase_path = %passphrase_path.display(),
        "First boot detected: generated a managed vault passphrase file; this is convenience mode and should not replace an external secret manager in production"
    );
    Ok(Some(ZeroizingString::new(persisted)))
}

fn vault_passphrase_path(vault_path: &Path) -> PathBuf {
    vault_path.with_extension("passphrase")
}

fn generate_vault_passphrase() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Some kernel actions require a real running task context (parent/child
/// linkage, scheduler state, blocking task suspension). Chat sessions do not
/// own a registered task — synthetic tasks would corrupt scheduler state or
/// deadlock the chat HTTP request. Reject those with a clean message instead
/// of dispatching them.
fn chat_incompatible_action_error(
    action: &crate::kernel_action::KernelAction,
) -> Option<&'static str> {
    use crate::kernel_action::KernelAction;
    match action {
        KernelAction::SpawnAgent { .. }
        | KernelAction::AwaitAgents { .. }
        | KernelAction::PollAgents { .. }
        | KernelAction::CancelAgent { .. }
        | KernelAction::DelegateTask { .. }
        | KernelAction::SpawnAsync { .. }
        | KernelAction::AgentRpcCall { .. } => Some(
            "This action requires a running task context. Run it from `agentos task run …` (or have an agent invoke it inside an executing task), not from chat.",
        ),
        _ => None,
    }
}

fn persist_generated_passphrase(path: &Path, passphrase: &str) -> Result<(), anyhow::Error> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => {
            use std::io::Write;
            file.write_all(passphrase.as_bytes())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
            }
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = std::fs::read_to_string(path)?;
            anyhow::ensure!(
                !existing.trim().is_empty(),
                "Stored vault passphrase file is empty: {}",
                path.display()
            );
            Ok(())
        }
        Err(err) => Err(err.into()),
    }
}

/// Merge the per-call dedup cache back into the kernel's session-keyed map.
/// Existing entries for unchanged keys are preserved, new entries override.
/// When the cap is exceeded, evicts oldest by insertion timestamp (LRU). The
/// merge guards against concurrent same-session writers clobbering each
/// other's results — last writer wins per-key, but no entries are wholesale
/// dropped.
async fn persist_session_dedup_cache(
    map: &Arc<RwLock<ChatSessionDedupMap>>,
    session_id: &str,
    cache: ChatSessionDedupCache,
    cap: usize,
) {
    let now = std::time::Instant::now();
    let mut guard = map.write().await;
    let entry = guard
        .entry(session_id.to_string())
        .or_insert_with(|| (now, HashMap::new()));
    entry.0 = now;
    entry.1.extend(cache);
    if entry.1.len() > cap {
        let mut by_age: Vec<((String, String), std::time::Instant)> =
            entry.1.iter().map(|(k, (t, _))| (k.clone(), *t)).collect();
        by_age.sort_by_key(|(_, t)| *t);
        let drop_count = entry.1.len() - cap;
        for (k, _) in by_age.into_iter().take(drop_count) {
            entry.1.remove(&k);
        }
    }
}

impl Kernel {
    /// Returns the kernel data directory (used by the web server to co-locate stores).
    pub fn data_dir(&self) -> &std::path::Path {
        &self.data_dir
    }

    /// Canonical path to the config file this kernel was booted from.
    pub fn config_path(&self) -> &std::path::Path {
        &self.config_path
    }

    /// Drop the dedup cache for a chat session. Call when the session is
    /// deleted from `chat_store` so the kernel doesn't leak memory keyed by a
    /// session id no caller can reach again.
    pub async fn forget_chat_session_dedup(&self, session_id: &str) {
        self.chat_session_dedup.write().await.remove(session_id);
    }

    /// Evict dedup-cache entries for sessions untouched longer than `max_age`.
    /// Wired into `TimeoutChecker` (24h sweep). Bounds memory on long-running
    /// kernels with many distinct chat sessions.
    pub async fn sweep_chat_session_dedup(&self, max_age: std::time::Duration) -> usize {
        let now = std::time::Instant::now();
        let mut guard = self.chat_session_dedup.write().await;
        let before = guard.len();
        guard.retain(|_, (last, _)| now.duration_since(*last) <= max_age);
        before - guard.len()
    }

    /// Re-pull the connected channel list from `UserChannelRegistry` and update
    /// the shared snapshot used by `agent-manual` filtering. Called after every
    /// channel register/deregister so the agent-facing view stays current.
    pub(crate) async fn refresh_connected_channels_snapshot(&self) {
        let new_list: Vec<agentos_tools::agent_manual::ConnectedChannel> = match self
            .channel_registry
            .list_active()
            .await
        {
            Ok(list) => list
                .into_iter()
                .filter(|c| c.active)
                .map(|c| agentos_tools::agent_manual::ConnectedChannel {
                    name: c.display_name,
                    kind: c.kind.to_string(),
                })
                .collect(),
            Err(e) => {
                tracing::warn!(error = %e, "refresh_connected_channels_snapshot: list_active failed");
                return;
            }
        };
        let mut guard = self.connected_channels_snapshot.write().await;
        *guard = new_list;
    }

    /// Build a flat `SkillSummary` snapshot from a SkillRegistry. Pure helper —
    /// no IO, no awaits — so it can be called inline under either an `await`ed
    /// `read().await` or a synchronous `try_read()` without lifetime gymnastics.
    /// Mirrors how `refresh_connected_channels_snapshot` materializes
    /// `ConnectedChannel` records out of `UserChannelRegistry`.
    pub(crate) fn build_skill_snapshot(
        registry: &agentos_skills::SkillRegistry,
    ) -> Vec<agentos_tools::agent_manual::SkillSummary> {
        // `list()` returns only manifests; iterate the names so we can pull the
        // full `InstalledSkill` (manifest + system_prompt) via `get`. If the two
        // ever disagree (registry race or future bug) we'd silently drop a skill
        // from the snapshot — log so the drift is visible rather than invisible.
        registry
            .list()
            .iter()
            .filter_map(|m| {
                let Some(skill) = registry.get(&m.skill.name) else {
                    tracing::warn!(
                        skill = %m.skill.name,
                        version = %m.skill.version,
                        "SkillRegistry::list returned a manifest but get() found none — \
                         dropping from agent-manual snapshot. Indicates registry drift."
                    );
                    return None;
                };
                let m = &skill.manifest;
                Some(agentos_tools::agent_manual::SkillSummary {
                    name: m.skill.name.clone(),
                    version: m.skill.version.clone(),
                    description: m.skill.description.clone(),
                    author: m.skill.author.clone(),
                    trust_tier: m.skill.trust_tier.clone(),
                    roles: m.agent.roles.clone(),
                    schedule: m.triggers.schedule.clone(),
                    events: m.triggers.events.clone(),
                    tools_required: m.tools.required.clone(),
                    tools_optional: m.tools.optional.clone(),
                    permissions_required: m.permissions.required.clone(),
                    max_cost_per_run: m.budget.max_cost_per_run,
                    max_tokens_per_run: m.budget.max_tokens_per_run,
                    system_prompt: skill.system_prompt.clone().into(),
                })
            })
            .collect()
    }

    /// Refresh the agent-manual's installed-skills snapshot from the live
    /// `SkillRegistry`. Called from `cmd_skill_install` / `cmd_skill_remove`
    /// so the manual's `skills` section reflects reality without the manual
    /// holding a direct registry reference. Mirrors
    /// `refresh_connected_channels_snapshot`.
    pub(crate) async fn refresh_installed_skills_snapshot(&self) {
        let snapshot = {
            let sr = self.skill_registry.read().await;
            Self::build_skill_snapshot(&sr)
        };
        let mut guard = self.installed_skills_snapshot.write().await;
        *guard = snapshot;
    }

    /// Re-register all active channels that were persisted from the previous run.
    ///
    /// Called once during `boot()` after the kernel struct is constructed.  For each
    /// active channel in `UserChannelRegistry`, the corresponding delivery adapter is
    /// rebuilt (credentials re-fetched from vault) and its listener task is started.
    async fn restore_channels(&self) {
        let channels = match self.channel_registry.list_active().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to restore channels from registry");
                return;
            }
        };

        for ch in channels {
            let adapter_result = self
                .build_channel_adapter(
                    &ch.kind,
                    &ch.external_id,
                    &ch.credential_key,
                    &ch.reply_topic,
                    &ch.server_url,
                    &ch.webhook_url,
                    ch.id,
                )
                .await;

            match adapter_result {
                Ok(Some(adapter)) => {
                    let adapter: Arc<dyn crate::notification_router::DeliveryAdapter> =
                        Arc::from(adapter);
                    self.notification_router
                        .register_adapter(adapter.clone())
                        .await;
                    self.channel_listener_registry
                        .start(ch.id, adapter, self.inbound_tx.clone())
                        .await;
                    tracing::info!(
                        channel_id = %ch.id,
                        kind = %ch.kind,
                        "Restored channel from registry"
                    );
                }
                Ok(None) => match self.register_channel_manager_adapter(&ch.id).await {
                    Ok(()) => {
                        tracing::info!(
                            channel_id = %ch.id,
                            kind = %ch.kind,
                            "Restored channel-manager adapter from registry"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            channel_id = %ch.id,
                            kind = %ch.kind,
                            error = %e,
                            "Failed to restore channel-manager adapter"
                        );
                    }
                },
                Err(e) => {
                    tracing::warn!(
                        channel_id = %ch.id,
                        kind = %ch.kind,
                        error = %e,
                        "Failed to restore channel adapter"
                    );
                }
            }
        }
    }

    /// Install the image resolver used by LLM adapters for [`ImageSource::FileRef`] (e.g. web uploads).
    pub fn set_image_resolver(&self, resolver: Arc<dyn agentos_llm::ImageResolver>) {
        *self
            .image_resolver
            .write()
            .expect("image_resolver lock poisoned") = resolver;
    }

    /// Install the sink used to persist inbound channel media (web `FileStore`).
    /// The InboundRouter shares this slot, so the change is visible to it.
    pub fn set_attachment_sink(&self, sink: Arc<dyn crate::attachment_sink::AttachmentSink>) {
        // Tolerate poisoning to match the InboundRouter reader; the critical
        // section is panic-free so a poisoned lock is practically impossible.
        *self
            .attachment_sink
            .write()
            .unwrap_or_else(|e| e.into_inner()) = sink;
    }

    /// Resolve a channel's vaulted secret stored under `{credential_key}.{suffix}`.
    /// Used for the WhatsApp webhook app-secret / verify-token (vault convention,
    /// so no `RegisteredChannel`/connect-flow changes are needed).
    async fn channel_aux_secret(&self, channel_id: &str, suffix: &str) -> Option<String> {
        let cid: ChannelInstanceID = channel_id.parse().ok()?;
        let cred = match self.channel_registry.get_by_id(&cid).await {
            Ok(Some(ch)) => ch.credential_key,
            _ => return None,
        };
        if cred.is_empty() {
            return None;
        }
        self.vault
            .get(&format!("{cred}.{suffix}"))
            .await
            .ok()
            .map(|s| s.as_str().to_string())
    }

    /// Verify a WhatsApp webhook `X-Hub-Signature-256` against the app secret in
    /// the vault (`{credential_key}.app_secret`). Fail-closed if absent.
    pub async fn whatsapp_verify_signature(
        &self,
        channel_id: &str,
        body: &[u8],
        signature: &str,
    ) -> bool {
        match self.channel_aux_secret(channel_id, "app_secret").await {
            Some(secret) => agentos_channels::whatsapp::verify_whatsapp_signature(
                secret.as_bytes(),
                body,
                signature,
            ),
            None => false,
        }
    }

    /// The WhatsApp webhook GET verify-token (`{credential_key}.verify_token`).
    pub async fn whatsapp_verify_token(&self, channel_id: &str) -> Option<String> {
        self.channel_aux_secret(channel_id, "verify_token").await
    }

    fn merge_chat_user_parts(
        new_message: &str,
        user_parts: Option<Vec<agentos_types::ContentPart>>,
    ) -> Vec<agentos_types::ContentPart> {
        match user_parts {
            Some(p) if !p.is_empty() => p,
            _ => vec![agentos_types::ContentPart::Text {
                text: new_message.to_string(),
            }],
        }
    }

    /// Build the LLM tool-schema list for a chat turn.
    ///
    /// Selection: `CHAT_DEFAULT_TOOL_NAMES` ∪ tools recently invoked in this
    /// session (from `chat_session_dedup`) ∪ top-N tools by recency-weighted
    /// usage for this agent (from `tool_usage_store`). Deduped by name, sorted
    /// alphabetically, with non-default extras capped at `CHAT_MANIFEST_EXTRA_BUDGET`.
    ///
    /// Without this, the chat path always sent the static default set, so a
    /// follow-up turn could not re-invoke an MCP tool the previous turn had
    /// already used (e.g. `gmail_send`) — the LLM had no schema for it, and
    /// burned iterations re-running `agent-manual`/`search-tools`/`describe-tool`
    /// every turn. Anthropic prompt caching tolerates this: extras grow only
    /// once per newly-used tool and stabilize within a few turns.
    ///
    /// Per-turn semantics: this is invoked once at the start of each chat
    /// inference. The manifest set is FIXED for that turn — a tool first
    /// invoked at iteration 1 of turn N becomes visible in turn N+1, not
    /// iteration 2. That's intentional: the LLM's tools= block is a prompt
    /// cache prefix and changing it mid-loop would invalidate the cache and
    /// cost a full reprice on every iteration.
    ///
    /// Stability: kept `pub` (not `pub(crate)`) only so integration tests in
    /// `tests/e2e/` can call it. Treat as semver-unstable internal API; do
    /// not call from out-of-tree.
    pub async fn build_chat_tool_manifests(
        &self,
        agent_id: &AgentID,
        session_id: Option<&str>,
    ) -> Vec<ToolManifest> {
        const CHAT_MANIFEST_EXTRA_BUDGET: usize = 25;
        // Floor on usage-rank score to suppress boundary churn at the cap edge.
        // Anything that hasn't been used recently enough to clear this won't
        // win an extras slot — the prompt-cache prefix stays stable instead
        // of flipping a near-zero name in and out across turns.
        const USAGE_RANK_MIN_SCORE: f64 = 0.1;

        // Names of tools actually executed in this session, most recent first.
        // `chat_session_dedup` already filters out `META_TOOL_NAMES` at insert
        // time, so we don't have to filter again here.
        let session_recent: Vec<String> = if let Some(sid) = session_id {
            let guard = self.chat_session_dedup.read().await;
            if let Some((_, inner)) = guard.get(sid) {
                let mut by_recency: Vec<(String, std::time::Instant)> = inner
                    .iter()
                    .map(|((name, _), (ts, _))| (name.clone(), *ts))
                    .collect();
                by_recency.sort_by_key(|x| std::cmp::Reverse(x.1));
                let mut seen = std::collections::HashSet::new();
                by_recency
                    .into_iter()
                    .filter_map(|(n, _)| seen.insert(n.clone()).then_some(n))
                    .collect()
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        // Cross-session usage rank (count * exp(-age/168h)).
        let usage_rank = self.tool_usage.rank_snapshot(&agent_id.to_string()).await;
        let mut ranked: Vec<(String, f64)> = usage_rank
            .into_iter()
            .filter(|(_, score)| *score >= USAGE_RANK_MIN_SCORE)
            .collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let top_usage: Vec<String> = ranked.into_iter().map(|(n, _)| n).collect();

        let mut allowed: std::collections::HashSet<String> =
            agentos_tools::factory::CHAT_DEFAULT_TOOL_NAMES
                .iter()
                .map(|s| (*s).to_string())
                .collect();
        let mut extras_added = 0usize;
        // Session-recent first so a tool the model just used in this conversation
        // wins over a globally popular but session-irrelevant tool when the budget
        // is tight.
        for name in session_recent.iter().chain(top_usage.iter()) {
            if extras_added >= CHAT_MANIFEST_EXTRA_BUDGET {
                break;
            }
            if agentos_tools::META_TOOL_NAMES.contains(&name.as_str()) {
                continue;
            }
            if allowed.insert(name.clone()) {
                extras_added += 1;
            }
        }

        let registry = self.tool_registry.read().await;
        let mut manifests: Vec<ToolManifest> = registry
            .list_all()
            .into_iter()
            .filter(|tool| {
                if allowed.contains(&tool.manifest.manifest.name) {
                    return true;
                }
                // Always surface tools attached at runtime by an MCP server.
                // Identified by the `mcp` tag set in commands/mcp.rs when a
                // server connects. Without this, a fresh chat session has no
                // gmail_*/linkedin_*/etc schemas and the LLM either hallucinates
                // a refusal ("I can't send emails") or burns iterations on
                // search-tools/agent-manual to rediscover what is already
                // attached. The list is stable across turns (changes only on
                // server attach/detach), so prompt caching still benefits.
                tool.manifest
                    .manifest
                    .tags
                    .as_ref()
                    .is_some_and(|t| t.iter().any(|s| s == "mcp"))
            })
            .map(|tool| tool.manifest.clone())
            .collect();
        manifests.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
        manifests
    }

    /// Direct chat inference — calls the agent's LLM with the conversation history.
    ///
    /// Does NOT create a task or touch the scheduler. Used exclusively by the web UI
    /// chat interface so conversations are stored separately from task execution.
    ///
    /// Thin wrapper around `chat_infer_with_tools` for backward compatibility.
    pub async fn chat_infer(
        &self,
        agent_name: &str,
        history: &[(String, String)],
        new_message: &str,
    ) -> Result<String, String> {
        let result = self
            .chat_infer_with_tools(agent_name, history, new_message, None, None)
            .await?;
        Ok(result.answer)
    }

    /// Chat inference with tool execution loop.
    ///
    /// Detects tool call JSON in LLM responses, executes the tool via `ToolRunner`,
    /// injects the result back into the context window, and re-infers until the LLM
    /// produces a final natural-language answer. Cap is `chat.max_tool_iterations`
    /// (default 25) — `CHAT_MAX_TOOL_ITERATIONS_FALLBACK` is used when config is 0.
    ///
    /// When `user_parts` is `Some(non-empty)`, those parts become the user
    /// turn's content verbatim and `new_message` is used ONLY for history
    /// persistence — callers must therefore pass the same text in `new_message`
    /// as in the leading `ContentPart::Text` of `user_parts` (see
    /// `merge_chat_user_parts`). When `user_parts` is `None`, `new_message`
    /// becomes the single text part.
    pub async fn chat_infer_with_tools(
        &self,
        agent_name: &str,
        history: &[(String, String)],
        new_message: &str,
        user_parts: Option<Vec<agentos_types::ContentPart>>,
        session_id: Option<&str>,
    ) -> Result<ChatInferenceResult, String> {
        let (agent_id, agent_permissions, agent_description, agent_roles, agent_system_prompt) = {
            let registry = self.agent_registry.read().await;
            match registry.get_by_name(agent_name) {
                Some(a) if a.status != AgentStatus::Offline => (
                    a.id,
                    a.permissions.clone(),
                    a.description.clone(),
                    a.roles.clone(),
                    a.system_prompt.clone(),
                ),
                Some(_) => return Err(format!("Agent '{}' is offline", agent_name)),
                None => return Err(format!("Agent '{}' not found", agent_name)),
            }
        };

        let llm = {
            let active = self.active_llms.read().await;
            active.get(&agent_id).cloned()
        };
        let llm = match llm {
            Some(a) => a,
            None => {
                return Err(format!(
                    "No LLM adapter connected for agent '{}'",
                    agent_name
                ))
            }
        };

        // Build system prompt from the canonical builder — same structure as task execution.
        let llm_tool_manifests: Vec<ToolManifest> =
            self.build_chat_tool_manifests(&agent_id, session_id).await;
        let connected_channels: Vec<crate::system_prompt::ChannelHint> =
            match self.channel_registry.list_active().await {
                Ok(list) => list
                    .into_iter()
                    .filter(|c| c.active)
                    .map(|c| crate::system_prompt::ChannelHint {
                        name: c.display_name,
                        kind: c.kind.to_string(),
                    })
                    .collect(),
                Err(_) => Vec::new(),
            };
        let system_prompt =
            crate::system_prompt::build_system_prompt(&crate::system_prompt::SystemPromptContext {
                agent_name: agent_name.to_string(),
                agent_description,
                agent_roles,
                custom_instructions: agent_system_prompt,
                sub_agent: None,
                enforce_final_tag: self.config.chat.enforce_final_tag,
                timezone: crate::system_prompt::local_timezone_str(),
                connected_channels,
                native_tool_calling: llm.supports_native_tool_calling(),
            });

        let mut ctx = agentos_types::ContextWindow::new(256);
        ctx.push(agentos_types::ContextEntry {
            role: agentos_types::ContextRole::System,
            parts: vec![agentos_types::ContentPart::Text {
                text: system_prompt,
            }],
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 1.0,
            pinned: true,
            reference_count: 0,
            partition: agentos_types::ContextPartition::Active,
            category: agentos_types::ContextCategory::Task,
            is_summary: false,
        });
        for (role, content) in history {
            let ctx_role = if role == "assistant" {
                agentos_types::ContextRole::Assistant
            } else {
                agentos_types::ContextRole::User
            };
            ctx.push(agentos_types::ContextEntry {
                role: ctx_role,
                parts: vec![agentos_types::ContentPart::Text {
                    text: content.clone(),
                }],
                timestamp: chrono::Utc::now(),
                metadata: None,
                importance: 0.5,
                pinned: false,
                reference_count: 0,
                partition: agentos_types::ContextPartition::Active,
                category: agentos_types::ContextCategory::History,
                is_summary: false,
            });
        }
        ctx.push(agentos_types::ContextEntry {
            role: agentos_types::ContextRole::User,
            parts: Self::merge_chat_user_parts(new_message, user_parts),
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: agentos_types::ContextPartition::Active,
            category: agentos_types::ContextCategory::Task,
            is_summary: false,
        });

        let mut tool_calls: Vec<ChatToolCallRecord> = Vec::new();
        let mut iterations = 0u32;
        let mut total_tokens_used = 0u64;
        let mut total_cost_usd = 0.0f64;
        let chat_max_tool_iterations = if self.config.chat.max_tool_iterations == 0 {
            CHAT_MAX_TOOL_ITERATIONS_FALLBACK
        } else {
            self.config.chat.max_tool_iterations
        };

        // Circuit breakers for stuck small-model loops. Reset whenever the
        // model makes progress (different tool / non-empty text / different
        // error). See logs around 2026-04-30T07:46 — gemma4:31b-cloud spammed
        // the same failing `agent-manual` call 8x with empty assistant text.
        // Streak guards disabled: thresholds set arbitrarily high so the
        // chat loop relies on `chat_max_tool_iterations` as the sole backstop.
        const REPEAT_TOOL_ERROR_LIMIT: u32 = 1_000_000;
        const EMPTY_TEXT_TOOLCALL_STREAK_LIMIT: u32 = 1_000_000;
        const DEDUP_STREAK_LIMIT: u32 = 1_000_000;
        let mut repeated_tool_errors: std::collections::HashMap<(String, String), u32> =
            std::collections::HashMap::new();
        let mut empty_text_streak_signature: Option<String> = None;
        let mut empty_text_streak_count: u32 = 0;
        // Meta-tool streak: catches alternating discovery loops
        // (search-tools → describe-tool → agent-manual → …) that the
        // identical-tool guard misses because the signature changes
        // every iteration. Resets on the first real tool call.
        let mut meta_tool_streak_count: u32 = 0;
        // Same-call dedup cache. Small models re-issue identical (tool_name,
        // payload) calls inside a single chat session — logs 2026-05-08T07:02
        // show `describe-tool {name: gmail_send}` ran 3x and `agent-manual
        // {section: mcp}` ran 2x back-to-back. Replay first result with a
        // `_dedup: true` flag + hint so the model unblocks instead of looping.
        // Pre-populate from the per-session dedup map. Each user message
        // spawns a fresh `chat_infer_*` call with an empty history of tool
        // results (chat history sent to the LLM is text-only — tool results
        // from prior turns are NOT replayed). Without this, a small model
        // re-issues `agent-manual`/`describe-tool` for tools it already used
        // moments earlier — see logs 2026-05-08T08:04. Persisted across calls
        // for the same `session_id`; cap 128 entries / session.
        let mut executed_tool_calls: std::collections::HashMap<
            (String, String),
            (std::time::Instant, serde_json::Value),
        > = if let Some(sid) = session_id {
            self.chat_session_dedup
                .read()
                .await
                .get(sid)
                .map(|(_, m)| m.clone())
                .unwrap_or_default()
        } else {
            std::collections::HashMap::new()
        };
        let mut consecutive_dedup_count: u32 = 0;
        const SESSION_DEDUP_CACHE_CAP: usize = 128;

        let final_answer = loop {
            iterations += 1;
            let image_parts_in_context = ctx
                .active_entries()
                .iter()
                .flat_map(|e| &e.parts)
                .filter(|p| matches!(p, agentos_types::ContentPart::Image { .. }))
                .count();
            let mut result = llm
                .infer_with_tools(&ctx, &llm_tool_manifests)
                .await
                .map_err(|e| format!("Inference failed: {}", e))?;

            // Strip leaked fenced ```json tool-intent blocks from `result.text`,
            // promote them into `result.tool_calls` when the adapter returned
            // none, and compute the user-visible form. `result.text` keeps
            // the model's raw reasoning (minus tool blocks) for the context
            // window; `visible_text` is what we show the user and persist to
            // chat history.
            let visible_text =
                self.sanitize_chat_inference_result(&mut result, agent_name, iterations);

            tracing::info!(
                target: "agentos::chat",
                agent = %agent_name,
                iteration = iterations,
                text_len = result.text.len(),
                visible_text_len = visible_text.len(),
                native_tool_calls = result.tool_calls.len(),
                image_parts_in_context,
                tokens_used = result.tokens_used.total_tokens,
                model = %result.model,
                duration_ms = result.duration_ms,
                "Chat LLM response received"
            );
            total_tokens_used = total_tokens_used.saturating_add(result.tokens_used.total_tokens);
            if let Some(cost) = &result.cost {
                if cost.total_cost_usd.is_finite() && cost.total_cost_usd > 0.0 {
                    total_cost_usd += cost.total_cost_usd;
                }
            }
            tracing::debug!(
                target: "agentos::chat",
                agent = %agent_name,
                iteration = iterations,
                text = %result.text,
                "Chat LLM raw response text"
            );

            if iterations >= chat_max_tool_iterations {
                let trimmed = visible_text.trim();
                if trimmed.is_empty() {
                    break format!(
                        "{}\n\n[Note: Maximum tool call limit reached.]",
                        EMPTY_LLM_ANSWER_PLACEHOLDER
                    );
                }
                break format!(
                    "{}\n\n[Note: Maximum tool call limit reached.]",
                    visible_text
                );
            }

            // Reset the meta-tool streak when the model produces real
            // visible text — a thinking turn or final-answer paragraph
            // counts as breaking the discovery loop (review fix W3).
            // Without this reset, a model that emits 3 meta calls,
            // then a thinking-only iteration, then more meta calls
            // would keep climbing the streak across the gap.
            if !visible_text.trim().is_empty() {
                meta_tool_streak_count = 0;
            }

            // Prefer native tool calls from the adapter. Use tool_calls presence
            // as the primary signal; StopReason is supplementary.
            let has_native_tool_calls = !result.tool_calls.is_empty();
            if has_native_tool_calls && result.stop_reason != agentos_llm::StopReason::ToolUse {
                tracing::warn!(
                    target: "agentos::chat",
                    stop_reason = ?result.stop_reason,
                    tool_call_count = result.tool_calls.len(),
                    "LLM returned tool_calls without ToolUse stop_reason; using native tool_calls anyway"
                );
            }
            if result.stop_reason == agentos_llm::StopReason::ToolUse
                && result.tool_calls.is_empty()
            {
                tracing::warn!(
                    target: "agentos::chat",
                    "LLM signaled ToolUse but returned no tool_calls"
                );
            }

            if has_native_tool_calls {
                // Empty-text + same-tool-call streak detector. See docstring
                // on the streaming variant for context.
                let mut sig_names: Vec<String> = result
                    .tool_calls
                    .iter()
                    .map(|tc| tc.tool_name.clone())
                    .collect();
                sig_names.sort();
                sig_names.dedup();
                let signature = sig_names.join("+");
                if visible_text.trim().is_empty() {
                    if empty_text_streak_signature.as_deref() == Some(signature.as_str()) {
                        empty_text_streak_count += 1;
                    } else {
                        empty_text_streak_signature = Some(signature.clone());
                        empty_text_streak_count = 1;
                    }
                    if empty_text_streak_count >= EMPTY_TEXT_TOOLCALL_STREAK_LIMIT {
                        tracing::warn!(
                            target: "agentos::chat",
                            agent = %agent_name,
                            iteration = iterations,
                            tools = %signature,
                            streak = empty_text_streak_count,
                            "Aborting chat loop: model stuck calling same tool(s) with no text"
                        );
                        break format!(
                            "{}\n\n[Note: aborted — model called {} {}x with no text. Likely stuck. Try rephrasing or use a stronger model.]",
                            EMPTY_LLM_ANSWER_PLACEHOLDER,
                            signature,
                            empty_text_streak_count,
                        );
                    }
                } else {
                    empty_text_streak_signature = None;
                    empty_text_streak_count = 0;
                }

                // Meta-tool streak guard: increment if the entire batch
                // is meta-tool calls; reset the moment a real tool is
                // invoked. Fires regardless of text content because the
                // signature of a discovery loop is "calls 4+ rounds of
                // search/describe/manual, never invokes a real tool".
                let tool_names_only: Vec<String> = result
                    .tool_calls
                    .iter()
                    .map(|tc| tc.tool_name.clone())
                    .collect();
                if iteration_is_all_meta(&tool_names_only) {
                    meta_tool_streak_count += 1;
                    if meta_tool_streak_count >= META_TOOL_STREAK_LIMIT {
                        tracing::warn!(
                            target: "agentos::chat",
                            agent = %agent_name,
                            iteration = iterations,
                            streak = meta_tool_streak_count,
                            tools = %tool_names_only.join(","),
                            "Aborting chat loop: meta-tool discovery streak exceeded"
                        );
                        break format!(
                            "{}\n\n[Note: aborted — model spent {} iterations on tool-discovery (search/describe/manual) without invoking a real tool. Pick a tool from `list-tools` and call it directly, or rephrase the request.]",
                            EMPTY_LLM_ANSWER_PLACEHOLDER,
                            meta_tool_streak_count,
                        );
                    }
                } else {
                    meta_tool_streak_count = 0;
                }

                // Push the LLM's tool-call response into context, preserving
                // the tool_calls array so adapters can reconstruct the
                // provider-native assistant message format on the next turn.
                let tool_calls_json = match serde_json::to_value(&result.tool_calls) {
                    Ok(v) => Some(v),
                    Err(e) => {
                        tracing::error!(
                            error = %e,
                            "Failed to serialize tool_calls into context metadata — \
                             multi-turn tool protocol will break on next inference"
                        );
                        None
                    }
                };
                ctx.push(agentos_types::ContextEntry {
                    role: agentos_types::ContextRole::Assistant,
                    parts: vec![agentos_types::ContentPart::Text {
                        text: result.text.clone(),
                    }],
                    timestamp: chrono::Utc::now(),
                    metadata: Some(agentos_types::ContextMetadata {
                        tool_name: None,
                        tool_id: None,
                        intent_id: None,
                        tokens_estimated: None,
                        tool_call_id: None,
                        assistant_tool_calls: tool_calls_json,
                    }),
                    importance: 0.5,
                    pinned: false,
                    reference_count: 0,
                    partition: agentos_types::ContextPartition::Active,
                    category: agentos_types::ContextCategory::Task,
                    is_summary: false,
                });

                let calls_to_execute: Vec<(String, serde_json::Value, String, Option<String>)> =
                    result
                        .tool_calls
                        .iter()
                        .map(|tc| {
                            (
                                tc.tool_name.clone(),
                                tc.payload.clone(),
                                tc.intent_type.clone(),
                                tc.id.clone(),
                            )
                        })
                        .collect();

                let agent_snapshot_for_chat: Arc<dyn AgentRegistryQuery> = {
                    let registry = self.agent_registry.read().await;
                    let agents: Vec<AgentSummary> = registry
                        .list_all()
                        .into_iter()
                        .map(|p| AgentSummary {
                            id: p.id,
                            name: p.name.clone(),
                            status: format!("{:?}", p.status).to_lowercase(),
                            registered_at: p.created_at,
                        })
                        .collect();
                    Arc::new(AgentRegistrySnapshot::new(agents))
                };

                let mut repeat_error_abort: Option<String> = None;
                for (tool_name, payload, intent_type_str, tool_call_id) in &calls_to_execute {
                    let chat_trace_id = TraceID::new();
                    let ws_chat = self.workspace_paths_for_agent(&agent_id);
                    let exec_ctx = ToolExecutionContext {
                        data_dir: self.data_dir.clone(),
                        task_id: TaskID::new(),
                        agent_id,
                        trace_id: chat_trace_id,
                        permissions: agent_permissions.clone(),
                        vault: None,
                        hal: Some(self.hal.clone()),
                        file_lock_registry: None,
                        agent_registry: Some(Arc::clone(&agent_snapshot_for_chat)),
                        task_registry: None,
                        escalation_query: None,
                        workspace_paths: ws_chat.read,
                        workspace_paths_writable: ws_chat.writable,
                        workspace_paths_executable: ws_chat.executable,
                        capability_registry: {
                            let reg = self.capability_registry.read().await;
                            Some(
                                Arc::new(CapabilityRegistrySnapshot::new(reg.list_capabilities()))
                                    as Arc<dyn CapabilityRegistryQuery>,
                            )
                        },
                        capability_dispatcher: Some(Arc::clone(&self.capability_dispatcher)
                            as Arc<dyn CapabilityDispatcher>),
                        storage_zone_query: Some(
                            Arc::new(self.zone_table.clone()) as Arc<dyn StorageZoneQuery>
                        ),
                        cancellation_token: self.cancellation_token.child_token(),
                        tool_categories: None,
                    };

                    let dedup_key = (
                        tool_name.clone(),
                        serde_json::to_string(payload).unwrap_or_default(),
                    );
                    let cached = executed_tool_calls.get(&dedup_key).map(|(_, v)| v.clone());
                    // Refresh LRU timestamp on hit so hot keys aren't evicted
                    // before colder keys inserted later in the same call.
                    if cached.is_some() {
                        if let Some(entry) = executed_tool_calls.get_mut(&dedup_key) {
                            entry.0 = std::time::Instant::now();
                        }
                    }

                    let start = std::time::Instant::now();
                    let mut tool_result = if let Some(prev) = cached.clone() {
                        consecutive_dedup_count += 1;
                        let mut wrapped = prev;
                        if let Some(obj) = wrapped.as_object_mut() {
                            obj.insert("_dedup".to_string(), serde_json::Value::Bool(true));
                            obj.insert(
                                "_dedup_hint".to_string(),
                                serde_json::Value::String(format!(
                                    "Identical call to '{}' was already executed in this session. \
                                     Result replayed verbatim. Use the existing result; do not call '{}' again with the same arguments. \
                                     If you need different information, change arguments or call a different tool.",
                                    tool_name, tool_name
                                )),
                            );
                        } else {
                            wrapped = serde_json::json!({
                                "_dedup": true,
                                "_dedup_hint": format!(
                                    "Identical call to '{}' was already executed; result replayed verbatim.",
                                    tool_name
                                ),
                                "result": wrapped,
                            });
                        }
                        tracing::warn!(
                            tool = %tool_name,
                            consecutive = consecutive_dedup_count,
                            "Chat tool dedup hit — replaying cached result"
                        );
                        if consecutive_dedup_count >= DEDUP_STREAK_LIMIT {
                            repeat_error_abort = Some(format!(
                                "[Note: aborted — same tool/payload repeated {}x with no progress (dedup cache hit). Last tool: '{}']",
                                consecutive_dedup_count, tool_name
                            ));
                        }
                        wrapped
                    } else {
                        consecutive_dedup_count = 0;
                        match self
                            .tool_runner
                            .execute(tool_name, payload.clone(), exec_ctx)
                            .await
                        {
                            Ok(value) => value,
                            Err(e) => {
                                tracing::warn!(
                                    tool = %tool_name,
                                    error = %e,
                                    "Chat tool execution failed"
                                );
                                serde_json::json!({"error": e.to_string()})
                            }
                        }
                    };
                    if cached.is_none() {
                        if let Some(action) =
                            crate::kernel_action::KernelAction::from_tool_result(&tool_result)
                        {
                            if let Some(reject) = chat_incompatible_action_error(&action) {
                                tool_result = serde_json::json!({ "error": reject });
                            } else {
                                let synthetic_task = {
                                    let mut t = agentos_types::AgentTask {
                                        agent_id,
                                        ..Default::default()
                                    };
                                    t.capability_token.agent_id = agent_id;
                                    t.capability_token.task_id = t.id;
                                    t.capability_token.permissions = agent_permissions.clone();
                                    t
                                };
                                let outcome = self
                                    .dispatch_kernel_action(&synthetic_task, action, chat_trace_id)
                                    .await;
                                tool_result = outcome.result;
                            }
                        }
                        // Don't cache meta/discovery tool results. Their output is
                        // stateless documentation; caching would freeze stale
                        // search/manual results across turns and trip dedup on
                        // legitimate re-exploration in the next chat turn.
                        if !agentos_tools::META_TOOL_NAMES.contains(&tool_name.as_str()) {
                            executed_tool_calls.insert(
                                dedup_key,
                                (std::time::Instant::now(), tool_result.clone()),
                            );
                        }
                    }
                    let duration_ms = start.elapsed().as_millis() as u64;

                    let success = !tool_result
                        .as_object()
                        .is_some_and(|o| o.contains_key("error"));

                    // Record successful real (non-dedup) tool calls into the
                    // cross-session usage rank and the in-memory LRU. Mirrors
                    // `task_executor.rs` so chat-driven tool use feeds back
                    // into `build_chat_tool_manifests`'s top-N selection on
                    // future turns. Failures and dedup-replays are not
                    // recorded — they don't represent productive use.
                    //
                    // Intentional asymmetry vs `task_executor.rs:1772-1785`:
                    // the executor records on every successful exec because
                    // it has no per-(tool, payload) dedup. Chat does, so the
                    // `cached.is_none()` guard collapses three identical
                    // `gmail_send {to: alice}` calls in one session into a
                    // single rank-record event — counting "agent uses gmail
                    // routinely", not "agent spammed it". Do not remove
                    // `cached.is_none()` thinking it's a bug.
                    if cached.is_none()
                        && success
                        && !agentos_tools::META_TOOL_NAMES.contains(&tool_name.as_str())
                    {
                        self.tool_usage
                            .record(&agent_id.to_string(), tool_name.as_str())
                            .await;
                        let tool_name_owned = tool_name.clone();
                        let mut lru = self.agent_tool_lru.write().await;
                        let entry = lru.entry(agent_id).or_default();
                        entry.retain(|n| n != &tool_name_owned);
                        entry.push_front(tool_name_owned);
                        if entry.len() > 10 {
                            entry.truncate(10);
                        }
                    }

                    if !success {
                        let err_text = tool_result
                            .get("error")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let mut err_sig: String = err_text.chars().take(80).collect();
                        if err_sig.is_empty() {
                            err_sig = "<no-message>".into();
                        }
                        let key = (tool_name.clone(), err_sig.clone());
                        let count = repeated_tool_errors.entry(key).or_insert(0);
                        *count += 1;
                        if *count >= REPEAT_TOOL_ERROR_LIMIT {
                            repeat_error_abort = Some(format!(
                                "[Note: aborted — tool '{}' kept failing with the same error ({}x): {}]",
                                tool_name, count, err_sig
                            ));
                        }
                    }

                    tool_calls.push(ChatToolCallRecord {
                        tool_name: tool_name.clone(),
                        intent_type: intent_type_str.clone(),
                        id: tool_call_id.clone(),
                        payload: payload.clone(),
                        result: tool_result.clone(),
                        duration_ms,
                    });

                    // Truncate large tool results to 4 KB (char-boundary safe).
                    let result_str = {
                        let full = serde_json::to_string_pretty(&tool_result).unwrap_or_default();
                        if full.len() > 4096 {
                            let mut boundary = 4096;
                            while boundary > 0 && !full.is_char_boundary(boundary) {
                                boundary -= 1;
                            }
                            format!("{}...[truncated]", &full[..boundary])
                        } else {
                            full
                        }
                    };

                    // Inject tool result with native metadata when available.
                    ctx.push(agentos_types::ContextEntry {
                        role: agentos_types::ContextRole::ToolResult,
                        parts: vec![agentos_types::ContentPart::Text { text: result_str }],
                        timestamp: chrono::Utc::now(),
                        metadata: Some(agentos_types::ContextMetadata {
                            tool_name: Some(tool_name.clone()),
                            tool_id: None,
                            intent_id: None,
                            tokens_estimated: None,
                            tool_call_id: tool_call_id.clone(),
                            assistant_tool_calls: None,
                        }),
                        importance: 0.7,
                        pinned: false,
                        reference_count: 0,
                        partition: agentos_types::ContextPartition::Active,
                        category: agentos_types::ContextCategory::Task,
                        is_summary: false,
                    });

                    if repeat_error_abort.is_some() {
                        break;
                    }
                }
                if let Some(note) = repeat_error_abort {
                    tracing::warn!(
                        target: "agentos::chat",
                        agent = %agent_name,
                        iteration = iterations,
                        "Aborting chat loop: repeat tool-error circuit breaker tripped"
                    );
                    break format!("{}\n\n{}", EMPTY_LLM_ANSWER_PLACEHOLDER, note);
                }
            } else {
                // No tool call — this is the final answer.
                let answer = if visible_text.trim().is_empty() {
                    tracing::warn!(
                        target: "agentos::chat",
                        agent = %agent_name,
                        iteration = iterations,
                        model = %result.model,
                        stop_reason = ?result.stop_reason,
                        raw_text_len = result.text.len(),
                        completion_tokens = result.tokens_used.completion_tokens,
                        prompt_tokens = result.tokens_used.prompt_tokens,
                        tool_calls_count = result.tool_calls.len(),
                        raw_text_preview = %result.text.chars().take(200).collect::<String>(),
                        "Chat LLM returned empty final answer; substituting placeholder"
                    );
                    EMPTY_LLM_ANSWER_PLACEHOLDER.to_string()
                } else {
                    visible_text
                };
                tracing::info!(
                    target: "agentos::chat",
                    agent = %agent_name,
                    iteration = iterations,
                    answer_len = answer.len(),
                    "Chat inference complete"
                );
                break answer;
            }
        };

        // Persist runs only on the success path. Earlier `return Err(...)`
        // arms (registry-lookup, LLM-adapter init) bypass this deliberately:
        // no tool calls executed, so nothing changed in the dedup cache and
        // there is nothing useful to write back.
        if let Some(sid) = session_id {
            persist_session_dedup_cache(
                &self.chat_session_dedup,
                sid,
                executed_tool_calls,
                SESSION_DEDUP_CACHE_CAP,
            )
            .await;
        }

        Ok(ChatInferenceResult {
            answer: final_answer,
            tool_calls,
            iterations,
            tokens_used: total_tokens_used,
            cost_usd: total_cost_usd,
        })
    }

    /// Chat inference with streaming events.
    ///
    /// Same logic as `chat_infer_with_tools()` but sends `ChatStreamEvent` values
    /// through an `mpsc::Sender` so the web layer can stream progress to the browser.
    /// Uses `infer_stream_with_tools()` internally so individual tokens are forwarded
    /// as `TextChunk` events for real incremental rendering.
    /// Also returns the final `ChatInferenceResult` so the caller can persist it.
    pub async fn chat_infer_streaming(
        &self,
        agent_name: &str,
        history: &[(String, String)],
        new_message: &str,
        user_parts: Option<Vec<agentos_types::ContentPart>>,
        tx: tokio::sync::mpsc::Sender<ChatStreamEvent>,
        session_id: Option<&str>,
    ) -> Result<ChatInferenceResult, String> {
        let (agent_id, agent_permissions, agent_description, agent_roles, agent_system_prompt) = {
            let registry = self.agent_registry.read().await;
            match registry.get_by_name(agent_name) {
                Some(a) if a.status != AgentStatus::Offline => (
                    a.id,
                    a.permissions.clone(),
                    a.description.clone(),
                    a.roles.clone(),
                    a.system_prompt.clone(),
                ),
                Some(_) => {
                    let msg = format!("Agent '{}' is offline", agent_name);
                    let _ = tx
                        .send(ChatStreamEvent::Error {
                            message: msg.clone(),
                        })
                        .await;
                    return Err(msg);
                }
                None => {
                    let msg = format!("Agent '{}' not found", agent_name);
                    let _ = tx
                        .send(ChatStreamEvent::Error {
                            message: msg.clone(),
                        })
                        .await;
                    return Err(msg);
                }
            }
        };

        let llm = {
            let active = self.active_llms.read().await;
            active.get(&agent_id).cloned()
        };
        let llm = match llm {
            Some(a) => a,
            None => {
                let msg = format!("No LLM adapter connected for agent '{}'", agent_name);
                let _ = tx
                    .send(ChatStreamEvent::Error {
                        message: msg.clone(),
                    })
                    .await;
                return Err(msg);
            }
        };

        let llm_tool_manifests: Vec<ToolManifest> =
            self.build_chat_tool_manifests(&agent_id, session_id).await;
        let connected_channels: Vec<crate::system_prompt::ChannelHint> =
            match self.channel_registry.list_active().await {
                Ok(list) => list
                    .into_iter()
                    .filter(|c| c.active)
                    .map(|c| crate::system_prompt::ChannelHint {
                        name: c.display_name,
                        kind: c.kind.to_string(),
                    })
                    .collect(),
                Err(_) => Vec::new(),
            };
        let system_prompt =
            crate::system_prompt::build_system_prompt(&crate::system_prompt::SystemPromptContext {
                agent_name: agent_name.to_string(),
                agent_description,
                agent_roles,
                custom_instructions: agent_system_prompt,
                sub_agent: None,
                enforce_final_tag: self.config.chat.enforce_final_tag,
                timezone: crate::system_prompt::local_timezone_str(),
                connected_channels,
                native_tool_calling: llm.supports_native_tool_calling(),
            });

        let mut ctx = agentos_types::ContextWindow::new(256);
        ctx.push(agentos_types::ContextEntry {
            role: agentos_types::ContextRole::System,
            parts: vec![agentos_types::ContentPart::Text {
                text: system_prompt,
            }],
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 1.0,
            pinned: true,
            reference_count: 0,
            partition: agentos_types::ContextPartition::Active,
            category: agentos_types::ContextCategory::Task,
            is_summary: false,
        });
        for (role, content) in history {
            let ctx_role = if role == "assistant" {
                agentos_types::ContextRole::Assistant
            } else {
                agentos_types::ContextRole::User
            };
            ctx.push(agentos_types::ContextEntry {
                role: ctx_role,
                parts: vec![agentos_types::ContentPart::Text {
                    text: content.clone(),
                }],
                timestamp: chrono::Utc::now(),
                metadata: None,
                importance: 0.5,
                pinned: false,
                reference_count: 0,
                partition: agentos_types::ContextPartition::Active,
                category: agentos_types::ContextCategory::History,
                is_summary: false,
            });
        }
        ctx.push(agentos_types::ContextEntry {
            role: agentos_types::ContextRole::User,
            parts: Self::merge_chat_user_parts(new_message, user_parts),
            timestamp: chrono::Utc::now(),
            metadata: None,
            importance: 0.5,
            pinned: false,
            reference_count: 0,
            partition: agentos_types::ContextPartition::Active,
            category: agentos_types::ContextCategory::Task,
            is_summary: false,
        });

        let mut tool_calls: Vec<ChatToolCallRecord> = Vec::new();
        let mut iterations = 0u32;
        let mut total_tokens_used = 0u64;
        let mut total_cost_usd = 0.0f64;
        let chat_max_tool_iterations = if self.config.chat.max_tool_iterations == 0 {
            CHAT_MAX_TOOL_ITERATIONS_FALLBACK
        } else {
            self.config.chat.max_tool_iterations
        };

        // Circuit breakers for stuck small-model loops. Reset whenever the
        // model makes progress (different tool / non-empty text / different
        // error). See logs around 2026-04-30T07:46 — gemma4:31b-cloud spammed
        // the same failing `agent-manual` call 8x with empty assistant text.
        // Streak guards disabled: thresholds set arbitrarily high so the
        // chat loop relies on `chat_max_tool_iterations` as the sole backstop.
        const REPEAT_TOOL_ERROR_LIMIT: u32 = 1_000_000;
        const EMPTY_TEXT_TOOLCALL_STREAK_LIMIT: u32 = 1_000_000;
        const DEDUP_STREAK_LIMIT: u32 = 1_000_000;
        let mut repeated_tool_errors: std::collections::HashMap<(String, String), u32> =
            std::collections::HashMap::new();
        let mut empty_text_streak_signature: Option<String> = None;
        let mut empty_text_streak_count: u32 = 0;
        let mut meta_tool_streak_count: u32 = 0;
        // Pre-populate from the per-session dedup map. Each user message
        // spawns a fresh `chat_infer_*` call with an empty history of tool
        // results (chat history sent to the LLM is text-only — tool results
        // from prior turns are NOT replayed). Without this, a small model
        // re-issues `agent-manual`/`describe-tool` for tools it already used
        // moments earlier — see logs 2026-05-08T08:04. Persisted across calls
        // for the same `session_id`; cap 128 entries / session.
        let mut executed_tool_calls: std::collections::HashMap<
            (String, String),
            (std::time::Instant, serde_json::Value),
        > = if let Some(sid) = session_id {
            self.chat_session_dedup
                .read()
                .await
                .get(sid)
                .map(|(_, m)| m.clone())
                .unwrap_or_default()
        } else {
            std::collections::HashMap::new()
        };
        let mut consecutive_dedup_count: u32 = 0;
        const SESSION_DEDUP_CACHE_CAP: usize = 128;

        let final_answer = loop {
            iterations += 1;

            let image_parts_in_context = ctx
                .active_entries()
                .iter()
                .flat_map(|e| &e.parts)
                .filter(|p| matches!(p, agentos_types::ContentPart::Image { .. }))
                .count();

            let _ = tx
                .send(ChatStreamEvent::Thinking {
                    iteration: iterations,
                })
                .await;

            // Use infer_stream_with_tools to get real token-level streaming.
            // Spawn it in a separate task so we can read tokens concurrently.
            let (inner_tx, mut inner_rx) =
                tokio::sync::mpsc::channel::<agentos_llm::InferenceEvent>(64);
            let llm_clone = llm.clone();
            let ctx_clone = ctx.clone();
            let manifests_clone = llm_tool_manifests.clone();
            tokio::spawn(async move {
                let inner_tx = inner_tx;
                if let Err(e) = llm_clone
                    .infer_stream_with_tools(&ctx_clone, &manifests_clone, inner_tx.clone())
                    .await
                {
                    // Some adapters surface the error via the channel before
                    // returning; some (e.g. reqwest connect/read failures in
                    // the OpenAI-compat path) only return Err and never wire
                    // up the SSE bridge — the receiver would otherwise observe
                    // a clean channel close and report the unhelpful
                    // "Stream ended without a Done event". Forward unconditionally;
                    // a duplicate Error event is harmless because the consumer
                    // breaks on the first one.
                    tracing::error!("infer_stream_with_tools failed: {e}");
                    let _ = inner_tx
                        .send(agentos_llm::InferenceEvent::Error(e.to_string()))
                        .await;
                }
            });

            // Consume streamed events, forwarding text chunks to the browser.
            // Per-iteration filter hides leaked fenced ```json tool-intent
            // blocks from the live SSE stream and (when `enforce_final_tag`
            // is enabled) drops any text outside `<final>...</final>` blocks
            // plus any text inside `<think>...</think>` blocks. The post-stream
            // extractor below promotes any matched fenced blocks into
            // `result.tool_calls` so the leaked intents actually execute.
            let mut sanitizer =
                crate::output_sanitizer::ChatOutputFilter::new(self.config.chat.enforce_final_tag);
            let mut inference_result: Option<agentos_llm::InferenceResult> = None;
            let mut stream_error: Option<String> = None;
            let mut stream_suppressed_count: usize = 0;
            let mut streamed_token_events: usize = 0;
            let mut pending_tail = String::new();

            while let Some(event) = inner_rx.recv().await {
                match event {
                    agentos_llm::InferenceEvent::Token(chunk) => {
                        let cleaned = sanitizer.push(&chunk);
                        if !cleaned.is_empty() {
                            streamed_token_events += 1;
                            let _ = tx.send(ChatStreamEvent::TextChunk { text: cleaned }).await;
                        }
                    }
                    agentos_llm::InferenceEvent::Done(result) => {
                        pending_tail = sanitizer.flush();
                        stream_suppressed_count = sanitizer.suppressed_block_count();
                        inference_result = Some(result);
                        break;
                    }
                    agentos_llm::InferenceEvent::Error(msg) => {
                        stream_error = Some(msg);
                        break;
                    }
                    // ToolCallStart, ToolCallDelta, ToolCallComplete, Usage — collected
                    // implicitly via the Done event's InferenceResult which carries all
                    // assembled tool_calls.
                    _ => {}
                }
            }

            if let Some(err_msg) = stream_error {
                let _ = tx
                    .send(ChatStreamEvent::Error {
                        message: format!("Inference failed: {}", err_msg),
                    })
                    .await;
                return Err(format!("Inference failed: {}", err_msg));
            }

            let mut result = match inference_result {
                Some(r) => r,
                None => {
                    let msg = "Stream ended without a Done event".to_string();
                    let _ = tx
                        .send(ChatStreamEvent::Error {
                            message: msg.clone(),
                        })
                        .await;
                    return Err(msg);
                }
            };

            // Defense in depth: scan the complete response text for fenced
            // ```json tool-intent blocks the adapter may have missed. When
            // found and the adapter returned no native tool calls, promote
            // them so the leaked intents actually execute. Always strip the
            // matched blocks from `result.text` so the context fed into the
            // next iteration does not re-tempt the model to repeat the leak.
            //
            // The streaming filter above already hid matched blocks from the
            // SSE stream; this post-stream pass operates on `result.text`,
            // which is the LLM adapter's complete unfiltered output.
            // `visible_text` is the user-visible form (also filtered by
            // `<final>` enforcement when enabled); `result.text` keeps the
            // model's raw reasoning for the next context window entry so
            // multi-turn tool-calling rounds do not lose chain-of-thought.
            let visible_text =
                self.sanitize_chat_inference_result(&mut result, agent_name, iterations);
            if streamed_token_events == 0 {
                // Some providers/adapters only emit a final Done payload. Simulate
                // incremental streaming so the UI remains responsive and visibly
                // progressive even when native token streaming is unavailable.
                let fallback_text = if !visible_text.is_empty() {
                    visible_text.clone()
                } else {
                    pending_tail.clone()
                };
                if !fallback_text.is_empty() {
                    const FALLBACK_CHUNK_CHARS: usize = 80;
                    const FALLBACK_CHUNK_DELAY_MS: u64 = 30;
                    let chars: Vec<char> = fallback_text.chars().collect();
                    let mut idx = 0usize;
                    while idx < chars.len() {
                        let end = (idx + FALLBACK_CHUNK_CHARS).min(chars.len());
                        let chunk: String = chars[idx..end].iter().collect();
                        let _ = tx.send(ChatStreamEvent::TextChunk { text: chunk }).await;
                        idx = end;
                        if idx < chars.len() {
                            tokio::time::sleep(std::time::Duration::from_millis(
                                FALLBACK_CHUNK_DELAY_MS,
                            ))
                            .await;
                        }
                    }
                }
            } else if !pending_tail.is_empty() {
                let _ = tx
                    .send(ChatStreamEvent::TextChunk { text: pending_tail })
                    .await;
            }
            if stream_suppressed_count > 0 {
                tracing::info!(
                    target: "agentos::chat",
                    agent = %agent_name,
                    iteration = iterations,
                    suppressed = stream_suppressed_count,
                    "Output sanitizer hid fenced tool-intent blocks from the live stream"
                );
            }

            tracing::info!(
                target: "agentos::chat",
                agent = %agent_name,
                iteration = iterations,
                text_len = result.text.len(),
                visible_text_len = visible_text.len(),
                native_tool_calls = result.tool_calls.len(),
                image_parts_in_context,
                tokens_used = result.tokens_used.total_tokens,
                model = %result.model,
                duration_ms = result.duration_ms,
                "Chat streaming LLM response received"
            );
            total_tokens_used = total_tokens_used.saturating_add(result.tokens_used.total_tokens);
            if let Some(cost) = &result.cost {
                if cost.total_cost_usd.is_finite() && cost.total_cost_usd > 0.0 {
                    total_cost_usd += cost.total_cost_usd;
                }
            }
            tracing::debug!(
                target: "agentos::chat",
                agent = %agent_name,
                iteration = iterations,
                text = %result.text,
                "Chat streaming LLM raw response text"
            );

            if iterations >= chat_max_tool_iterations {
                let visible_trimmed = visible_text.trim();
                let answer = if visible_trimmed.is_empty() {
                    format!(
                        "{}\n\n[Note: Maximum tool call limit reached.]",
                        EMPTY_LLM_ANSWER_PLACEHOLDER
                    )
                } else {
                    format!(
                        "{}\n\n[Note: Maximum tool call limit reached.]",
                        visible_text
                    )
                };
                let _ = tx
                    .send(ChatStreamEvent::Done {
                        answer: answer.clone(),
                        tool_calls: tool_calls.clone(),
                        iterations,
                        tokens_used: total_tokens_used,
                        cost_usd: total_cost_usd,
                    })
                    .await;
                break answer;
            }

            // Reset the meta-tool streak when the model produces real
            // visible text — a thinking turn or final-answer paragraph
            // counts as breaking the discovery loop (review fix W3).
            // Without this reset, a model that emits 3 meta calls,
            // then a thinking-only iteration, then more meta calls
            // would keep climbing the streak across the gap.
            if !visible_text.trim().is_empty() {
                meta_tool_streak_count = 0;
            }

            // Prefer native tool calls from the adapter. Use tool_calls presence
            // as the primary signal; StopReason is supplementary.
            let has_native_tool_calls = !result.tool_calls.is_empty();
            if has_native_tool_calls && result.stop_reason != agentos_llm::StopReason::ToolUse {
                tracing::warn!(
                    target: "agentos::chat",
                    stop_reason = ?result.stop_reason,
                    tool_call_count = result.tool_calls.len(),
                    "LLM returned tool_calls without ToolUse stop_reason; using native tool_calls anyway"
                );
            }
            if result.stop_reason == agentos_llm::StopReason::ToolUse
                && result.tool_calls.is_empty()
            {
                tracing::warn!(
                    target: "agentos::chat",
                    "LLM signaled ToolUse but returned no tool_calls"
                );
            }

            if has_native_tool_calls {
                // Circuit breaker: same tool-call set with empty assistant
                // text N iterations in a row. Triggers on the small-model
                // failure mode where the model emits no prose, only repeats
                // a tool call it cannot recover from.
                let mut sig_names: Vec<String> = result
                    .tool_calls
                    .iter()
                    .map(|tc| tc.tool_name.clone())
                    .collect();
                sig_names.sort();
                sig_names.dedup();
                let signature = sig_names.join("+");
                if visible_text.trim().is_empty() {
                    if empty_text_streak_signature.as_deref() == Some(signature.as_str()) {
                        empty_text_streak_count += 1;
                    } else {
                        empty_text_streak_signature = Some(signature.clone());
                        empty_text_streak_count = 1;
                    }
                    if empty_text_streak_count >= EMPTY_TEXT_TOOLCALL_STREAK_LIMIT {
                        tracing::warn!(
                            target: "agentos::chat",
                            agent = %agent_name,
                            iteration = iterations,
                            tools = %signature,
                            streak = empty_text_streak_count,
                            "Aborting chat loop: model stuck calling same tool(s) with no text"
                        );
                        let answer = format!(
                            "{}\n\n[Note: aborted — model called {} {}x with no text. Likely stuck. Try rephrasing or use a stronger model.]",
                            EMPTY_LLM_ANSWER_PLACEHOLDER,
                            signature,
                            empty_text_streak_count,
                        );
                        let _ = tx
                            .send(ChatStreamEvent::Done {
                                answer: answer.clone(),
                                tool_calls: tool_calls.clone(),
                                iterations,
                                tokens_used: total_tokens_used,
                                cost_usd: total_cost_usd,
                            })
                            .await;
                        break answer;
                    }
                } else {
                    empty_text_streak_signature = None;
                    empty_text_streak_count = 0;
                }

                // Meta-tool discovery loop guard — see sync variant for rationale.
                let tool_names_only: Vec<String> = result
                    .tool_calls
                    .iter()
                    .map(|tc| tc.tool_name.clone())
                    .collect();
                if iteration_is_all_meta(&tool_names_only) {
                    meta_tool_streak_count += 1;
                    if meta_tool_streak_count >= META_TOOL_STREAK_LIMIT {
                        tracing::warn!(
                            target: "agentos::chat",
                            agent = %agent_name,
                            iteration = iterations,
                            streak = meta_tool_streak_count,
                            tools = %tool_names_only.join(","),
                            "Aborting chat loop: meta-tool discovery streak exceeded"
                        );
                        let answer = format!(
                            "{}\n\n[Note: aborted — model spent {} iterations on tool-discovery (search/describe/manual) without invoking a real tool. Pick a tool from `list-tools` and call it directly, or rephrase the request.]",
                            EMPTY_LLM_ANSWER_PLACEHOLDER,
                            meta_tool_streak_count,
                        );
                        let _ = tx
                            .send(ChatStreamEvent::Done {
                                answer: answer.clone(),
                                tool_calls: tool_calls.clone(),
                                iterations,
                                tokens_used: total_tokens_used,
                                cost_usd: total_cost_usd,
                            })
                            .await;
                        break answer;
                    }
                } else {
                    meta_tool_streak_count = 0;
                }

                let tool_calls_json = match serde_json::to_value(&result.tool_calls) {
                    Ok(v) => Some(v),
                    Err(e) => {
                        tracing::error!(
                            error = %e,
                            "Failed to serialize tool_calls into context metadata — \
                             multi-turn tool protocol will break on next inference"
                        );
                        None
                    }
                };
                ctx.push(agentos_types::ContextEntry {
                    role: agentos_types::ContextRole::Assistant,
                    parts: vec![agentos_types::ContentPart::Text {
                        text: result.text.clone(),
                    }],
                    timestamp: chrono::Utc::now(),
                    metadata: Some(agentos_types::ContextMetadata {
                        tool_name: None,
                        tool_id: None,
                        intent_id: None,
                        tokens_estimated: None,
                        tool_call_id: None,
                        assistant_tool_calls: tool_calls_json,
                    }),
                    importance: 0.5,
                    pinned: false,
                    reference_count: 0,
                    partition: agentos_types::ContextPartition::Active,
                    category: agentos_types::ContextCategory::Task,
                    is_summary: false,
                });

                let calls_to_execute: Vec<(String, serde_json::Value, String, Option<String>)> =
                    result
                        .tool_calls
                        .iter()
                        .map(|tc| {
                            (
                                tc.tool_name.clone(),
                                tc.payload.clone(),
                                tc.intent_type.clone(),
                                tc.id.clone(),
                            )
                        })
                        .collect();

                let agent_snapshot_for_chat: Arc<dyn AgentRegistryQuery> = {
                    let registry = self.agent_registry.read().await;
                    let agents: Vec<AgentSummary> = registry
                        .list_all()
                        .into_iter()
                        .map(|p| AgentSummary {
                            id: p.id,
                            name: p.name.clone(),
                            status: format!("{:?}", p.status).to_lowercase(),
                            registered_at: p.created_at,
                        })
                        .collect();
                    Arc::new(AgentRegistrySnapshot::new(agents))
                };

                let mut repeat_error_abort: Option<String> = None;
                for (tool_name, payload, intent_type_str, tool_call_id) in &calls_to_execute {
                    let _ = tx
                        .send(ChatStreamEvent::ToolStart {
                            tool_name: tool_name.clone(),
                            iteration: iterations,
                        })
                        .await;

                    let chat_trace_id = TraceID::new();
                    let ws_chat = self.workspace_paths_for_agent(&agent_id);
                    let exec_ctx = ToolExecutionContext {
                        data_dir: self.data_dir.clone(),
                        task_id: TaskID::new(),
                        agent_id,
                        trace_id: chat_trace_id,
                        permissions: agent_permissions.clone(),
                        vault: None,
                        hal: Some(self.hal.clone()),
                        file_lock_registry: None,
                        agent_registry: Some(Arc::clone(&agent_snapshot_for_chat)),
                        task_registry: None,
                        escalation_query: None,
                        workspace_paths: ws_chat.read,
                        workspace_paths_writable: ws_chat.writable,
                        workspace_paths_executable: ws_chat.executable,
                        capability_registry: {
                            let reg = self.capability_registry.read().await;
                            Some(
                                Arc::new(CapabilityRegistrySnapshot::new(reg.list_capabilities()))
                                    as Arc<dyn CapabilityRegistryQuery>,
                            )
                        },
                        capability_dispatcher: Some(Arc::clone(&self.capability_dispatcher)
                            as Arc<dyn CapabilityDispatcher>),
                        storage_zone_query: Some(
                            Arc::new(self.zone_table.clone()) as Arc<dyn StorageZoneQuery>
                        ),
                        cancellation_token: self.cancellation_token.child_token(),
                        tool_categories: None,
                    };

                    let dedup_key = (
                        tool_name.clone(),
                        serde_json::to_string(payload).unwrap_or_default(),
                    );
                    let cached = executed_tool_calls.get(&dedup_key).map(|(_, v)| v.clone());
                    // Refresh LRU timestamp on hit so hot keys aren't evicted
                    // before colder keys inserted later in the same call.
                    if cached.is_some() {
                        if let Some(entry) = executed_tool_calls.get_mut(&dedup_key) {
                            entry.0 = std::time::Instant::now();
                        }
                    }

                    let start = std::time::Instant::now();
                    let mut tool_result = if let Some(prev) = cached.clone() {
                        consecutive_dedup_count += 1;
                        let mut wrapped = prev;
                        if let Some(obj) = wrapped.as_object_mut() {
                            obj.insert("_dedup".to_string(), serde_json::Value::Bool(true));
                            obj.insert(
                                "_dedup_hint".to_string(),
                                serde_json::Value::String(format!(
                                    "Identical call to '{}' was already executed in this session. \
                                     Result replayed verbatim. Use the existing result; do not call '{}' again with the same arguments. \
                                     If you need different information, change arguments or call a different tool.",
                                    tool_name, tool_name
                                )),
                            );
                        } else {
                            wrapped = serde_json::json!({
                                "_dedup": true,
                                "_dedup_hint": format!(
                                    "Identical call to '{}' was already executed; result replayed verbatim.",
                                    tool_name
                                ),
                                "result": wrapped,
                            });
                        }
                        tracing::warn!(
                            tool = %tool_name,
                            consecutive = consecutive_dedup_count,
                            "Chat streaming tool dedup hit — replaying cached result"
                        );
                        if consecutive_dedup_count >= DEDUP_STREAK_LIMIT {
                            repeat_error_abort = Some(format!(
                                "[Note: aborted — same tool/payload repeated {}x with no progress (dedup cache hit). Last tool: '{}']",
                                consecutive_dedup_count, tool_name
                            ));
                        }
                        wrapped
                    } else {
                        consecutive_dedup_count = 0;
                        match self
                            .tool_runner
                            .execute(tool_name, payload.clone(), exec_ctx)
                            .await
                        {
                            Ok(value) => value,
                            Err(e) => {
                                tracing::warn!(
                                    tool = %tool_name,
                                    error = %e,
                                    "Chat streaming tool execution failed"
                                );
                                serde_json::json!({"error": e.to_string()})
                            }
                        }
                    };
                    if cached.is_none() {
                        if let Some(action) =
                            crate::kernel_action::KernelAction::from_tool_result(&tool_result)
                        {
                            if let Some(reject) = chat_incompatible_action_error(&action) {
                                tool_result = serde_json::json!({ "error": reject });
                            } else {
                                let synthetic_task = {
                                    let mut t = agentos_types::AgentTask {
                                        agent_id,
                                        ..Default::default()
                                    };
                                    t.capability_token.agent_id = agent_id;
                                    t.capability_token.task_id = t.id;
                                    t.capability_token.permissions = agent_permissions.clone();
                                    t
                                };
                                let outcome = self
                                    .dispatch_kernel_action(&synthetic_task, action, chat_trace_id)
                                    .await;
                                tool_result = outcome.result;
                            }
                        }
                        // Don't cache meta/discovery tool results. Their output is
                        // stateless documentation; caching would freeze stale
                        // search/manual results across turns and trip dedup on
                        // legitimate re-exploration in the next chat turn.
                        if !agentos_tools::META_TOOL_NAMES.contains(&tool_name.as_str()) {
                            executed_tool_calls.insert(
                                dedup_key,
                                (std::time::Instant::now(), tool_result.clone()),
                            );
                        }
                    }
                    let duration_ms = start.elapsed().as_millis() as u64;

                    let result_str = {
                        let full = serde_json::to_string_pretty(&tool_result).unwrap_or_default();
                        if full.len() > 4096 {
                            let mut boundary = 4096;
                            while boundary > 0 && !full.is_char_boundary(boundary) {
                                boundary -= 1;
                            }
                            format!("{}...[truncated]", &full[..boundary])
                        } else {
                            full
                        }
                    };

                    let result_preview = {
                        let s = serde_json::to_string(&tool_result).unwrap_or_default();
                        if s.len() > 200 {
                            let mut boundary = 200;
                            while boundary > 0 && !s.is_char_boundary(boundary) {
                                boundary -= 1;
                            }
                            format!("{}...", &s[..boundary])
                        } else {
                            s
                        }
                    };
                    let success = !tool_result
                        .as_object()
                        .is_some_and(|o| o.contains_key("error"));

                    // Record successful real (non-dedup) tool calls into the
                    // cross-session usage rank and the in-memory LRU. See
                    // `chat_infer_with_tools` for rationale; same pattern.
                    if cached.is_none()
                        && success
                        && !agentos_tools::META_TOOL_NAMES.contains(&tool_name.as_str())
                    {
                        self.tool_usage
                            .record(&agent_id.to_string(), tool_name.as_str())
                            .await;
                        let tool_name_owned = tool_name.clone();
                        let mut lru = self.agent_tool_lru.write().await;
                        let entry = lru.entry(agent_id).or_default();
                        entry.retain(|n| n != &tool_name_owned);
                        entry.push_front(tool_name_owned);
                        if entry.len() > 10 {
                            entry.truncate(10);
                        }
                    }

                    if !success {
                        let err_text = tool_result
                            .get("error")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let mut err_sig: String = err_text.chars().take(80).collect();
                        if err_sig.is_empty() {
                            err_sig = "<no-message>".into();
                        }
                        let key = (tool_name.clone(), err_sig.clone());
                        let count = repeated_tool_errors.entry(key).or_insert(0);
                        *count += 1;
                        if *count >= REPEAT_TOOL_ERROR_LIMIT {
                            repeat_error_abort = Some(format!(
                                "[Note: aborted — tool '{}' kept failing with the same error ({}x): {}]",
                                tool_name, count, err_sig
                            ));
                        }
                    }

                    let _ = tx
                        .send(ChatStreamEvent::ToolResult {
                            tool_name: tool_name.clone(),
                            result_preview,
                            duration_ms,
                            success,
                        })
                        .await;

                    tool_calls.push(ChatToolCallRecord {
                        tool_name: tool_name.clone(),
                        intent_type: intent_type_str.clone(),
                        id: tool_call_id.clone(),
                        payload: payload.clone(),
                        result: tool_result.clone(),
                        duration_ms,
                    });

                    // Inject tool result with native metadata when available.
                    ctx.push(agentos_types::ContextEntry {
                        role: agentos_types::ContextRole::ToolResult,
                        parts: vec![agentos_types::ContentPart::Text { text: result_str }],
                        timestamp: chrono::Utc::now(),
                        metadata: Some(agentos_types::ContextMetadata {
                            tool_name: Some(tool_name.clone()),
                            tool_id: None,
                            intent_id: None,
                            tokens_estimated: None,
                            tool_call_id: tool_call_id.clone(),
                            assistant_tool_calls: None,
                        }),
                        importance: 0.7,
                        pinned: false,
                        reference_count: 0,
                        partition: agentos_types::ContextPartition::Active,
                        category: agentos_types::ContextCategory::Task,
                        is_summary: false,
                    });

                    if repeat_error_abort.is_some() {
                        break;
                    }
                }
                if let Some(note) = repeat_error_abort {
                    tracing::warn!(
                        target: "agentos::chat",
                        agent = %agent_name,
                        iteration = iterations,
                        "Aborting chat loop: repeat tool-error circuit breaker tripped"
                    );
                    let answer = format!("{}\n\n{}", EMPTY_LLM_ANSWER_PLACEHOLDER, note);
                    let _ = tx
                        .send(ChatStreamEvent::Done {
                            answer: answer.clone(),
                            tool_calls: tool_calls.clone(),
                            iterations,
                            tokens_used: total_tokens_used,
                            cost_usd: total_cost_usd,
                        })
                        .await;
                    break answer;
                }
            } else {
                let answer = if visible_text.trim().is_empty() {
                    tracing::warn!(
                        target: "agentos::chat",
                        agent = %agent_name,
                        iteration = iterations,
                        model = %result.model,
                        enforce_final_tag = self.config.chat.enforce_final_tag,
                        stop_reason = ?result.stop_reason,
                        raw_text_len = result.text.len(),
                        completion_tokens = result.tokens_used.completion_tokens,
                        prompt_tokens = result.tokens_used.prompt_tokens,
                        tool_calls_count = result.tool_calls.len(),
                        raw_text_preview = %result.text.chars().take(200).collect::<String>(),
                        "Chat streaming LLM returned empty final answer; substituting placeholder"
                    );
                    EMPTY_LLM_ANSWER_PLACEHOLDER.to_string()
                } else {
                    visible_text
                };
                tracing::info!(
                    target: "agentos::chat",
                    agent = %agent_name,
                    iteration = iterations,
                    answer_len = answer.len(),
                    "Chat streaming inference complete"
                );
                let _ = tx
                    .send(ChatStreamEvent::Done {
                        answer: answer.clone(),
                        tool_calls: tool_calls.clone(),
                        iterations,
                        tokens_used: total_tokens_used,
                        cost_usd: total_cost_usd,
                    })
                    .await;
                break answer;
            }
        };

        // Persist runs only on the success path. Earlier `return Err(...)`
        // arms (registry-lookup, LLM-adapter init) bypass this deliberately:
        // no tool calls executed, so nothing changed in the dedup cache and
        // there is nothing useful to write back.
        if let Some(sid) = session_id {
            persist_session_dedup_cache(
                &self.chat_session_dedup,
                sid,
                executed_tool_calls,
                SESSION_DEDUP_CACHE_CAP,
            )
            .await;
        }

        Ok(ChatInferenceResult {
            answer: final_answer,
            tool_calls,
            iterations,
            tokens_used: total_tokens_used,
            cost_usd: total_cost_usd,
        })
    }

    /// Strip leaked fenced ```json tool-intent blocks from a chat
    /// `InferenceResult`, promote any matched intents into `result.tool_calls`,
    /// and (when `enforce_final_tag` is enabled in the kernel chat config)
    /// compute the `<final>`-filtered user-visible text. Shared by both the
    /// streaming and non-streaming chat paths so leakage protection is
    /// uniform.
    ///
    /// Returns the cleaned user-visible text and leaves `result.text` with
    /// only the fenced tool-intent blocks removed (the model's raw reasoning
    /// prose is preserved there). The split matters: the user-facing SSE
    /// stream, chat history store, and `ChatInferenceResult::answer` should
    /// use the cleaned form, while the context window entry for the
    /// assistant turn stores the less-filtered `result.text` so the model
    /// retains its scratch reasoning across tool-calling iterations.
    ///
    /// Promotes extracted intents to `result.tool_calls` only when the
    /// adapter returned none, so the kernel cannot double-execute the same
    /// call. Always removes the matched fenced blocks from `result.text` so
    /// persisted history never contains them.
    fn sanitize_chat_inference_result(
        &self,
        result: &mut agentos_llm::InferenceResult,
        agent_name: &str,
        iteration: u32,
    ) -> String {
        use crate::output_sanitizer::{sanitize_visible_text, SanitizeProfile};

        let raw_text_len = result.text.len();
        let raw_text_empty = result.text.trim().is_empty();

        // History profile: strip fenced tool blocks + XML tags so the model's
        // next-turn context doesn't re-tempt the leaked format, but preserve
        // reasoning prose and raw errors.
        let history = sanitize_visible_text(&result.text, SanitizeProfile::History, false);

        // Promote extracted tool intents into result.tool_calls when the
        // adapter returned none (avoid double-execution otherwise).
        if !history.extracted_intents.is_empty() {
            tracing::warn!(
                target: "agentos::chat",
                agent = %agent_name,
                iteration = iteration,
                extracted = history.extracted_intents.len(),
                adapter_native_count = result.tool_calls.len(),
                "Promoted leaked fenced tool-intent blocks to structured tool calls"
            );
            self.audit_log(agentos_audit::AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: TraceID::new(),
                event_type: agentos_audit::AuditEventType::ToolIntentLeakedFromText,
                agent_id: None,
                task_id: None,
                tool_id: None,
                details: serde_json::json!({
                    "agent_name": agent_name,
                    "iteration": iteration,
                    "extracted_count": history.extracted_intents.len(),
                    "adapter_native_count": result.tool_calls.len(),
                    "promoted": result.tool_calls.is_empty(),
                    "tools": history
                        .extracted_intents
                        .iter()
                        .map(|i| i.tool.as_str())
                        .collect::<Vec<_>>(),
                }),
                severity: agentos_audit::AuditSeverity::Warn,
                reversible: false,
                rollback_ref: None,
            });
            if result.tool_calls.is_empty() {
                for intent in history.extracted_intents {
                    result.tool_calls.push(agentos_llm::InferenceToolCall {
                        id: None,
                        tool_name: intent.tool,
                        intent_type: intent.intent_type,
                        payload: intent.payload,
                    });
                }
            }
        }
        // Set result.text to the History-filtered form for the context window.
        result.text = history.text;

        // Delivery profile: full filtering for the user-facing answer.
        // Note: `delivery.extracted_intents` will always be empty here
        // because the History pass above already removed all fenced tool
        // blocks from `result.text`. Intent promotion uses
        // `history.extracted_intents` above — this is by design.
        let delivery = sanitize_visible_text(
            &result.text,
            SanitizeProfile::Delivery,
            self.config.chat.enforce_final_tag,
        );

        // Diagnostic logging: track where text disappears in the pipeline.
        if delivery.text.trim().is_empty() && !raw_text_empty {
            tracing::warn!(
                target: "agentos::chat",
                agent = %agent_name,
                iteration = iteration,
                raw_text_len = raw_text_len,
                history_text_len = result.text.len(),
                delivery_text_len = delivery.text.len(),
                enforce_final_tag = self.config.chat.enforce_final_tag,
                stop_reason = ?result.stop_reason,
                completion_tokens = result.tokens_used.completion_tokens,
                raw_preview = %result.text.chars().take(300).collect::<String>(),
                "Sanitizer reduced non-empty raw text to empty delivery — content stripped by filters"
            );
        } else if delivery.text.trim().is_empty() && raw_text_empty {
            tracing::warn!(
                target: "agentos::chat",
                agent = %agent_name,
                iteration = iteration,
                stop_reason = ?result.stop_reason,
                completion_tokens = result.tokens_used.completion_tokens,
                prompt_tokens = result.tokens_used.prompt_tokens,
                tool_calls = result.tool_calls.len(),
                "LLM returned empty text — model produced no content (not a sanitizer issue)"
            );
        }

        delivery.text
    }

    /// Log an audit entry, emitting a tracing error if the write fails.
    /// Replaces bare `.ok()` calls that silently swallow audit write failures.
    pub(crate) fn audit_log(&self, entry: agentos_audit::AuditEntry) {
        if let Err(e) = self.audit.append(entry) {
            tracing::error!(error = %e, "Failed to write audit log entry");
        }
    }

    /// Resolve the host directories `agent_id` may touch through file tools,
    /// bucketed by required permission mode. Returns three lists:
    ///
    /// - `read`     — every directory the agent has *at least* `READ` on (used
    ///   by `file-reader`, `file-diff`, `file-grep`, `file-glob`).
    /// - `writable` — directories the grant covers `WRITE` on (used by
    ///   `file-writer`, `file-editor`, `file-append`, `file-delete`,
    ///   `file-move`).
    /// - `executable` — directories the grant covers `EXEC` on (used by
    ///   `shell-exec` to extend its sandbox bind list).
    ///
    /// The legacy config-loaded `workspace_paths` are imported into the grant
    /// store at boot with `READ|WRITE` mode, so they appear in both `read`
    /// and `writable` but not `executable`. EXEC must be granted explicitly
    /// via `agentos workspace grant <path> --mode rwx`.
    pub fn workspace_paths_for_agent(
        &self,
        agent_id: &agentos_types::AgentID,
    ) -> AgentWorkspacePaths {
        use agentos_types::WorkspaceGrantMode;
        let mut read = self.workspace_paths.clone();
        let mut writable = self.workspace_paths.clone();
        let mut executable: Vec<PathBuf> = Vec::new();
        for grant in self.workspace_grants.list_for_agent(agent_id) {
            if grant.mode.covers(WorkspaceGrantMode::READ) && !read.contains(&grant.path) {
                read.push(grant.path.clone());
            }
            if grant.mode.covers(WorkspaceGrantMode::READ_WRITE) && !writable.contains(&grant.path)
            {
                writable.push(grant.path.clone());
            }
            if grant.mode.covers(WorkspaceGrantMode::READ_WRITE_EXEC)
                && !executable.contains(&grant.path)
            {
                executable.push(grant.path);
            }
        }
        AgentWorkspacePaths {
            read,
            writable,
            executable,
        }
    }

    /// Boot the kernel: load config, open subsystems, start bus, begin accepting.
    pub async fn boot(
        config_path: &Path,
        vault_passphrase: &ZeroizingString,
    ) -> Result<Self, anyhow::Error> {
        // 1. Load config
        let config = load_config(config_path)?;
        tracing::info!(
            config_path = %config_path.display(),
            ollama_host = %config.ollama.host,
            custom_llm_url = ?config.llm.custom_base_url,
            openai_base_url = ?config.llm.openai_base_url,
            "Kernel configuration loaded"
        );

        // 1.2 Load provider catalog (optional — missing file is not an error)
        let catalog_path = config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("providers.toml");
        let (provider_catalog, resolved_catalog_path) =
            match agentos_llm::ProviderCatalog::from_file(&catalog_path) {
                Ok(catalog) => {
                    if !catalog.is_empty() {
                        tracing::info!(
                            path = %catalog_path.display(),
                            providers = catalog.len(),
                            "Loaded provider catalog"
                        );
                    }
                    let path = Some(catalog_path);
                    (Arc::new(std::sync::RwLock::new(catalog)), path)
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to load provider catalog, continuing without it");
                    (
                        Arc::new(std::sync::RwLock::new(agentos_llm::ProviderCatalog::empty())),
                        None,
                    )
                }
            };

        // 1.5 Run pre-flight system health checks before any subsystem init
        preflight_checks(&config)?;

        // Ensure directories exist. The vault directory is created with 0o700 on Unix
        // so other users on the same host cannot list or access the vault parent directory.
        if let Some(parent) = Path::new(&config.audit.log_path).parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Some(parent) = Path::new(&config.secrets.vault_path).parent() {
            std::fs::create_dir_all(parent)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                    .unwrap_or_else(|e| {
                        tracing::warn!(error = %e, path = ?parent, "Failed to set vault directory permissions to 0o700");
                    });
            }
        }
        std::fs::create_dir_all(Path::new(&config.tools.core_tools_dir))?;
        std::fs::create_dir_all(Path::new(&config.tools.user_tools_dir))?;
        if let Some(parent) = Path::new(&config.bus.socket_path).parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Install bundled core tool manifests if not already present
        Self::install_core_manifests(Path::new(&config.tools.core_tools_dir))?;

        // 2. Open audit log
        let audit = Arc::new(AuditLog::open(Path::new(&config.audit.log_path))?);

        // 2.5 Verify audit hash chain integrity at startup (diagnostic — never blocks boot).
        {
            let from_seq = match audit.seq_for_last_n_entries(config.audit.verify_last_n_entries) {
                Ok(seq) => seq,
                Err(e) => {
                    tracing::error!(error = %e, "Failed to compute audit chain start position; skipping verification");
                    None
                }
            };

            match audit.verify_chain(from_seq) {
                Ok(ref result) if result.valid => {
                    tracing::info!(
                        entries_checked = result.entries_checked,
                        from_seq = ?from_seq,
                        "Audit chain integrity verified"
                    );
                }
                Ok(ref result) => {
                    tracing::error!(
                        entries_checked = result.entries_checked,
                        first_invalid_seq = ?result.first_invalid_seq,
                        error = ?result.error,
                        "SECURITY: Audit chain integrity FAILED — possible log tampering detected"
                    );
                    // Best-effort: append a tamper-detection event to the (possibly compromised) log.
                    if let Err(e) = audit.append(agentos_audit::AuditEntry {
                        timestamp: chrono::Utc::now(),
                        trace_id: TraceID::new(),
                        event_type: agentos_audit::AuditEventType::AuditChainTampered,
                        agent_id: None,
                        task_id: None,
                        tool_id: None,
                        details: serde_json::json!({
                            "entries_checked": result.entries_checked,
                            "first_invalid_seq": result.first_invalid_seq,
                            "error": result.error,
                        }),
                        severity: agentos_audit::AuditSeverity::Security,
                        reversible: false,
                        rollback_ref: None,
                    }) {
                        tracing::warn!(error = %e, "Failed to persist AuditChainTampered event to audit log");
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "Audit chain verification encountered an error");
                }
            }
        }

        // 3. Open or initialize secrets vault
        let vault_path = Path::new(&config.secrets.vault_path);
        let vault = if SecretsVault::is_initialized(vault_path) {
            Arc::new(SecretsVault::open(
                vault_path,
                vault_passphrase,
                audit.clone(),
            )?)
        } else {
            Arc::new(SecretsVault::initialize(
                vault_path,
                vault_passphrase,
                audit.clone(),
            )?)
        };

        // 4. Initialize capability engine (loads or generates HMAC signing key from vault)
        let capability_engine = Arc::new(CapabilityEngine::boot(&vault).await);

        // 4.5 Initialize HardwareAbstractionLayer
        let mut hal = HardwareAbstractionLayer::new();
        hal.register(Box::new(SystemDriver::new()));
        hal.register(Box::new(ProcessDriver::new()));
        hal.register(Box::new(NetworkDriver::new()));
        hal.register(Box::new(SensorDriver::new()));
        hal.register(Box::new(GpuDriver::new()));
        hal.register(Box::new(StorageDriver::new()));
        #[cfg(feature = "bluetooth")]
        hal.register(Box::new(BluetoothDriver::new()));
        #[cfg(feature = "audio")]
        hal.register(Box::new(AudioDriver::new()));
        #[cfg(feature = "display")]
        hal.register(Box::new(DisplayDriver::new()));
        #[cfg(feature = "printer")]
        hal.register(Box::new(PrinterDriver::new()));
        #[cfg(feature = "raw-usb")]
        hal.register(Box::new(RawUsbDriver::new()));
        #[cfg(feature = "usb-storage")]
        hal.register(Box::new(UsbStorageDriver::new()));
        #[cfg(feature = "webcam")]
        hal.register(Box::new(WebcamDriver::new()));

        // Register log reader with app logs only - audit log is not exposed to agents
        let app_logs = HashMap::new();
        let mut system_logs = HashMap::new();
        system_logs.insert(
            "syslog".to_string(),
            Path::new("/var/log/syslog").to_path_buf(),
        );
        hal.register(Box::new(LogReaderDriver::new(app_logs, system_logs)));

        let hardware_registry = Arc::new(HardwareRegistry::new());
        for device in discover_available_devices() {
            let status = KernelDeviceAccessGate::default_status_for_discovered_device(
                &device.id,
                &device.device_type,
            );
            let is_new =
                hardware_registry.register_device(&device.id, &device.device_type, status.clone());
            if is_new {
                tracing::info!(
                    device_id = %device.id,
                    device_type = %device.device_type,
                    status = ?status,
                    "Registered available hardware device during kernel boot"
                );
            }
        }
        // Wire the registry into the HAL immediately for compatibility with tests
        // and non-kernel callers; the richer approval gate is attached later once
        // the escalation manager exists.
        #[allow(unused_mut)]
        let mut hal = hal.with_registry(Arc::clone(&hardware_registry));

        // 5. Load tools (with optional CRL enforcement)
        // NOTE: Tools are loaded before the event channel exists, so boot-time
        // registrations do not emit ToolInstalled events. This is intentional --
        // the initial tool inventory can be queried via `cmd_list_tools`.
        let crl = if let Some(ref crl_path) = config.tools.crl_path {
            let crl_file = Path::new(crl_path);
            if crl_file.exists() {
                match agentos_tools::signing::RevocationList::load_from_file(crl_file) {
                    Ok(loaded) => {
                        tracing::info!(
                            path = %crl_path,
                            revoked = loaded.revoked_pubkeys.len(),
                            "Loaded certificate revocation list"
                        );
                        loaded
                    }
                    Err(e) => {
                        tracing::warn!(path = %crl_path, error = %e, "Failed to load CRL, proceeding without it");
                        agentos_tools::signing::RevocationList::new()
                    }
                }
            } else {
                tracing::warn!(path = %crl_path, "CRL path configured but file not found");
                agentos_tools::signing::RevocationList::new()
            }
        } else {
            agentos_tools::signing::RevocationList::new()
        };
        let tool_registry = Arc::new(RwLock::new(ToolRegistry::load_from_dirs_with_crl(
            Path::new(&config.tools.core_tools_dir),
            Path::new(&config.tools.user_tools_dir),
            crl,
        )?));

        // 5.5 Build schema registry from tool manifests. Examples are validated
        // against the schema at load — drift is a loud boot failure.
        let mut schema_registry = crate::schema_registry::SchemaRegistry::new();
        {
            let registry = tool_registry.read().await;
            for loaded in &registry.loaded {
                if let Some(ref schema) = loaded.manifest.payload_schema {
                    schema_registry.register_with_tier(
                        &loaded.manifest.manifest.name,
                        schema.clone(),
                        loaded.manifest.manifest.trust_tier,
                        &loaded.manifest.examples,
                    )?;
                    tracing::debug!(
                        tool = %loaded.manifest.manifest.name,
                        examples = loaded.manifest.examples.len(),
                        "Registered input schema for tool"
                    );
                }
            }
        }
        let schema_registry = Arc::new(schema_registry);

        // 6. Initialize other subsystems
        let data_dir = PathBuf::from(&config.tools.data_dir);
        std::fs::create_dir_all(&data_dir)?;

        // Canonicalize workspace paths at startup so runtime checks are fast.
        // Paths that don't exist yet are skipped with a warning.
        let workspace_paths: Vec<PathBuf> = config
            .tools
            .workspace
            .allowed_paths
            .iter()
            .filter_map(|p| {
                let path = PathBuf::from(p);
                match path.canonicalize() {
                    Ok(canonical) => Some(canonical),
                    Err(e) => {
                        tracing::debug!(
                            path = %p,
                            error = %e,
                            "Workspace path could not be canonicalized at startup; skipping"
                        );
                        None
                    }
                }
            })
            .collect();
        let state_db_path = resolve_state_db_path(&config.kernel.state_db_path, &data_dir);
        let state_store = Arc::new(
            crate::state_store::KernelStateStore::open(state_db_path.clone())
                .await
                .map_err(|e| anyhow::anyhow!("Failed to initialize kernel state DB: {}", e))?,
        );
        tracing::info!(
            state_db_path = %state_store.path().display(),
            "Kernel state persistence initialized"
        );
        let model_cache_dir = {
            let configured = PathBuf::from(&config.memory.model_cache_dir);
            if configured.is_absolute() {
                configured
            } else {
                data_dir.join(configured)
            }
        };
        std::fs::create_dir_all(&model_cache_dir)?;
        // Honor `memory.disable_embedder`: install a zero-vector stub instead
        // of touching onnxruntime. Vector retrieval becomes a no-op, FTS5
        // lexical search keeps working. The flag exists for hosts where
        // onnxruntime crashes during graph optimization (see config docs).
        let shared_embedder = if config.memory.disable_embedder {
            tracing::warn!(
                "memory.disable_embedder=true — using zero-vector embedder; \
                 semantic retrieval is disabled, lexical (FTS5) search still works"
            );
            Arc::new(Embedder::noop())
        } else {
            Arc::new(
                Embedder::with_cache_dir(&model_cache_dir)
                    .map_err(|e| anyhow::anyhow!("Failed to initialize shared embedder: {}", e))?,
            )
        };
        // Pre-compute manual section embeddings so `suggest_manual_sections`
        // can rank semantically (cosine over MiniLM) instead of falling
        // back to keyword overlap. Idempotent — first call wins.
        // Skip when the embedder is a no-op — zero vectors collapse cosine
        // ranking to ties, and the keyword-overlap fallback handles it.
        if !shared_embedder.is_noop() {
            agentos_tools::agent_manual::install_section_embeddings(Arc::clone(&shared_embedder));
        }
        // Clone a handle for semantic `search-tools` before `shared_embedder` is
        // moved into the procedural store below. A no-op embedder is carried
        // through unchanged — search-tools then falls back to substring scoring.
        let tool_search_embedder = Arc::clone(&shared_embedder);
        let episodic_memory = Arc::new(agentos_memory::EpisodicStore::open(&data_dir)?);
        let semantic_memory = Arc::new(agentos_memory::SemanticStore::open_with_embedder(
            &data_dir,
            shared_embedder.clone(),
        )?);
        let procedural_memory = Arc::new(agentos_memory::ProceduralStore::open_with_embedder(
            &data_dir,
            shared_embedder,
        )?);
        let scratchpad_store = Arc::new(
            agentos_scratch::ScratchpadStore::new(&data_dir.join("scratchpad.db"))
                .map_err(|e| anyhow::anyhow!("Scratchpad store init failed: {}", e))?,
        );
        let file_store = Arc::new(
            crate::file_store::FileStore::open(&data_dir)
                .map_err(|e| anyhow::anyhow!("File store init failed: {}", e))?,
        );
        let chat_store = Arc::new(
            crate::chat_store::ChatStore::open(&data_dir.join("chat.db"))
                .map_err(|e| anyhow::anyhow!("Chat store init failed: {}", e))?,
        );
        let convo_store = Arc::new(
            crate::convo_store::ConvoStore::open(&data_dir.join("agent_convos.db"))
                .map_err(|e| anyhow::anyhow!("Convo store init failed: {}", e))?,
        );
        let user_profile_db_path = config
            .user_profile
            .db_path
            .as_ref()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| data_dir.join("user_profile.db"));
        let user_profile_store = Arc::new(
            crate::user_profile_store::UserProfileStore::open_with_limits(
                user_profile_db_path,
                config.user_profile.min_confidence,
                config.user_profile.max_pinned,
            )
            .await
            .map_err(|e| anyhow::anyhow!("User profile store init failed: {}", e))?,
        );
        let mut tool_runner = ToolRunner::new_with_shared_memory(
            semantic_memory.clone(),
            episodic_memory.clone(),
            procedural_memory.clone(),
        );

        // Register scratchpad tools
        tool_runner.register_scratchpad_tools(scratchpad_store.clone());

        // host-package-install: replace the placeholder registered by
        // ToolRunner::new with one configured from `[tools.host_package]`.
        // When `enabled = false` we install with an empty allowlist + no
        // escalator so every call returns a clear error. When `enabled = true`
        // we resolve the operator-chosen privilege escalator and feed in the
        // allowlist + manager priority list. The returned `policy` handle
        // is retained on the kernel so the `ConfigWatcher` reload path can
        // hot-update the allowlist + manager list without restarting.
        let host_package_policy = {
            use agentos_tools::host_package::{
                resolve_escalator, EscalatorPolicy, HostPackageInstallTool, HostPackagePolicy,
            };
            let hp = &config.tools.host_package;
            let (allowlist, managers, escalator) = if hp.enabled {
                let escalator_policy = match hp.privilege_escalator.as_str() {
                    "auto" => EscalatorPolicy::Auto,
                    "pkexec" => EscalatorPolicy::Pkexec,
                    "helper" => EscalatorPolicy::Helper(std::path::PathBuf::from(&hp.helper_path)),
                    "none" => EscalatorPolicy::None,
                    other => {
                        tracing::error!(
                            value = %other,
                            "[tools.host_package].privilege_escalator must be one of \
                             auto|pkexec|helper|none — disabling host-package-install"
                        );
                        EscalatorPolicy::None
                    }
                };
                (
                    hp.allowlist.clone(),
                    hp.managers.clone(),
                    resolve_escalator(&escalator_policy),
                )
            } else {
                (Vec::new(), Vec::new(), None)
            };
            let policy = HostPackagePolicy::new(allowlist, managers);
            tool_runner.register(Box::new(HostPackageInstallTool::with_policy(
                policy.clone(),
                escalator,
            )));
            policy
        };

        // Register WASM tools from manifests that specify executor = wasm
        let wasm_executor = WasmToolExecutor::new(&data_dir);
        match wasm_executor {
            Ok(executor) => {
                let registry_read = tool_registry.read().await;
                for loaded in &registry_read.loaded {
                    if loaded.manifest.executor.executor_type == agentos_types::ExecutorType::Wasm {
                        if let Some(ref rel_path) = loaded.manifest.executor.wasm_path {
                            let abs_path = loaded.manifest_dir.join(rel_path);
                            match executor.load(&loaded.manifest, &abs_path) {
                                Ok(wasm_tool) => {
                                    tracing::info!(
                                        tool = %loaded.manifest.manifest.name,
                                        "Registered WASM tool"
                                    );
                                    tool_runner.register(Box::new(wasm_tool));
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        tool = %loaded.manifest.manifest.name,
                                        error = %e,
                                        "Failed to load WASM tool"
                                    );
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "WASM executor initialization failed; WASM tools disabled");
            }
        }

        // Register agent-manual and agent-self tools with a snapshot of all
        // registered tools. Both tools are registered after the tool registry is
        // fully loaded so they have an accurate view of all available tools.
        let tool_summaries_shared = {
            let registry_read = tool_registry.read().await;
            let all_tools: Vec<&agentos_types::RegisteredTool> = registry_read.list_all();
            let summaries_vec =
                agentos_tools::agent_manual::AgentManualTool::summaries_from_registry(&all_tools);
            std::sync::Arc::new(tokio::sync::RwLock::new(summaries_vec))
        };
        // Live snapshot of connected channels — populated now from the
        // registry, refreshed on register/deregister so the manual filter
        // and any other consumer always see the current state.
        // Populated at boot via `refresh_connected_channels_snapshot` (after `channel_registry` exists).
        let connected_channels_shared: agentos_tools::agent_manual::SharedConnectedChannels =
            std::sync::Arc::new(tokio::sync::RwLock::new(Vec::new()));
        // Live snapshot of installed skills — populated below after the skill
        // registry is built; refreshed on install/remove via
        // `refresh_installed_skills_snapshot`. Empty for now so the agent-manual
        // tool can be registered with the right shape.
        let installed_skills_shared: agentos_tools::agent_manual::SharedInstalledSkills =
            std::sync::Arc::new(tokio::sync::RwLock::new(Vec::new()));

        // Build skill registry, loading from configured skill directories.
        // Done here (before tool runner registration) so the `skill-create`
        // tool can be wired up with a live installer reference.
        let skill_registry = {
            let mut sr = agentos_skills::SkillRegistry::new();
            let core_skills_dir = Path::new(&config.skills.core_skills_dir);
            let user_skills_dir = Path::new(&config.skills.user_skills_dir);
            match sr.load_from_dir(core_skills_dir) {
                Ok(n) if n > 0 => {
                    tracing::info!(count = n, dir = %core_skills_dir.display(), "Loaded core skills")
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, dir = %core_skills_dir.display(), "Failed to scan core skills directory")
                }
            }
            match sr.load_from_dir(user_skills_dir) {
                Ok(n) if n > 0 => {
                    tracing::info!(count = n, dir = %user_skills_dir.display(), "Loaded user skills")
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, dir = %user_skills_dir.display(), "Failed to scan user skills directory")
                }
            }
            Arc::new(RwLock::new(sr))
        };

        // Hydrate the live skills snapshot now that the registry is loaded.
        // Subsequent install/remove paths refresh via
        // `refresh_installed_skills_snapshot`.
        {
            let sr = skill_registry.read().await;
            let snapshot = Self::build_skill_snapshot(&sr);
            let mut guard = installed_skills_shared.write().await;
            *guard = snapshot;
        }

        {
            // Collect tool names before registering agent-self so the list
            // includes every other tool but not agent-self itself (which is
            // registered in the next line). This avoids a chicken-and-egg
            // ordering problem and keeps the list accurate.
            let tool_count = tool_runner.list_tools().len();
            tool_runner.register_agent_manual_full(
                std::sync::Arc::clone(&tool_summaries_shared),
                std::sync::Arc::clone(&connected_channels_shared),
                std::sync::Arc::clone(&installed_skills_shared),
            );
            tool_runner.register_list_tools(std::sync::Arc::clone(&tool_summaries_shared));
            tool_runner.register_describe_tool(std::sync::Arc::clone(&tool_summaries_shared));
            tool_runner.register_search_tools(
                std::sync::Arc::clone(&tool_summaries_shared),
                tool_search_embedder,
            );
            tool_runner.register_skill_prompt(std::sync::Arc::clone(&installed_skills_shared));
            // `skill-create` writes the manifest + prompt under
            // `config.skills.user_skills_dir` and asks the kernel installer
            // to load it. The approval hook enforces `risk_class =
            // control_plane` from `tools/core/skill-create.toml`.
            let skill_installer: std::sync::Arc<dyn agentos_tools::SkillInstaller> =
                std::sync::Arc::new(crate::skill_installer::KernelSkillInstaller::new(
                    std::sync::Arc::clone(&skill_registry),
                    std::sync::Arc::clone(&installed_skills_shared),
                ));
            tool_runner.register_skill_create(
                std::path::PathBuf::from(&config.skills.user_skills_dir),
                skill_installer,
                std::sync::Arc::clone(&installed_skills_shared),
            );
            tool_runner.register_agent_self(tool_count);
        }

        let tool_usage = Arc::new(
            crate::tool_usage_store::ToolUsageStore::open(&data_dir.join("agent_tool_usage.db"))
                .map_err(|e| anyhow::anyhow!("ToolUsageStore init failed: {}", e))?,
        );

        // Proactive personalization — background interest aggregator (Phase 3).
        // When `personalization.enabled` is false we use an in-memory store so
        // no `user_interests.db` file is created on disk (Phase 6 invariant:
        // no personalization DB files when the operator has opted out).
        let user_interests_store = Arc::new(if config.personalization.enabled {
            crate::user_interests_store::UserInterestsStore::open(
                data_dir.join("user_interests.db"),
            )
            .await
            .map_err(|e| anyhow::anyhow!("UserInterestsStore init failed: {}", e))?
        } else {
            crate::user_interests_store::UserInterestsStore::open_in_memory()
                .await
                .map_err(|e| anyhow::anyhow!("UserInterestsStore in-memory init failed: {}", e))?
        });
        // Clone the interests store before moving it into the InterestModel so
        // the FeedbackProcessor (Phase 5) can also hold a handle.
        let user_interests_store_for_feedback = Arc::clone(&user_interests_store);
        let interest_model = Arc::new(crate::interest_model::InterestModel::new(
            user_interests_store,
            episodic_memory.clone(),
            tool_usage.clone(),
            &config.personalization,
        ));

        // Create hook registry early so it can be shared with the plugin registry.
        let hook_registry_arc = crate::hooks::HookRegistry::new();

        // 6.5 Initialize MCP supervisor, security gate, and attachment store.
        let kernel_cancellation_token = CancellationToken::new();
        let (mcp_event_tx, mut mcp_event_rx) = tokio::sync::mpsc::channel(100);
        let mcp_cancellation = kernel_cancellation_token.child_token();
        let mcp_supervisor = Arc::new(agentos_mcp::McpSupervisor::new(
            mcp_event_tx,
            mcp_cancellation.clone(),
        ));
        let mcp_security_gate = Arc::new(agentos_mcp::McpSecurityGate::new(
            audit.clone(),
            1024 * 1024, // 1MB default
        ));
        let mcp_attachment_store = Arc::new(
            crate::mcp_attachment_store::McpAttachmentStore::open(
                data_dir.join("mcp_attachments.db"),
            )
            .await
            .map_err(|e| anyhow::anyhow!("McpAttachmentStore init failed: {e}"))?,
        );
        let user_pref_proposal_store = Arc::new(
            crate::user_pref_proposals::UserPrefProposalStore::open(
                data_dir.join("user_pref_proposals.db"),
            )
            .await
            .map_err(|e| anyhow::anyhow!("UserPrefProposalStore init failed: {e}"))?,
        );

        // 6.6 Spawn all configured MCP servers in parallel.
        let mut mcp_add_tasks = Vec::new();
        for mcp_cfg in &config.mcp.servers {
            if let Err(e) = mcp_cfg.validate() {
                tracing::error!(
                    mcp_server = %mcp_cfg.name,
                    error = %e,
                    "Invalid MCP server config — skipping"
                );
                continue;
            }
            let supervisor = Arc::clone(&mcp_supervisor);
            let security_gate = Arc::clone(&mcp_security_gate);
            let cfg = mcp_cfg.clone();

            let task = tokio::spawn(async move {
                let transport_factory: Option<Arc<dyn agentos_mcp::McpTransportFactory>>;
                let transport: Arc<dyn agentos_mcp::McpTransport> = match (&cfg.command, &cfg.url) {
                    (Some(cmd), None) => {
                        // Create a factory so the supervisor can respawn on reconnect.
                        let factory =
                            Arc::new(agentos_mcp::transport::stdio::StdioTransportFactory::new(
                                format!("stdio:{}", cfg.name),
                                cmd.clone(),
                                cfg.args.clone(),
                                cfg.env.clone(),
                                cfg.working_dir.clone(),
                                cfg.timeout_secs,
                            ));
                        transport_factory = Some(factory);

                        match agentos_mcp::transport::stdio::StdioTransport::spawn(
                            format!("stdio:{}", cfg.name),
                            cmd.clone(),
                            cfg.args.clone(),
                            cfg.env.clone(),
                            cfg.working_dir.clone(),
                            cfg.timeout_secs,
                        )
                        .await
                        {
                            Ok(t) => Arc::new(t),
                            Err(e) => {
                                tracing::warn!(
                                    mcp_server = %cfg.name,
                                    error = %e,
                                    "Failed to spawn MCP transport"
                                );
                                return (cfg.name.clone(), Vec::new());
                            }
                        }
                    }
                    (None, Some(url)) => {
                        // HTTP is stateless — no factory needed.
                        transport_factory = None;
                        match agentos_mcp::transport::http::StreamableHttpTransport::new(
                            format!("http:{}", cfg.name),
                            url.clone(),
                            cfg.auth_token.clone(),
                            cfg.timeout_secs,
                        ) {
                            Ok(t) => Arc::new(t),
                            Err(e) => {
                                tracing::warn!(
                                    mcp_server = %cfg.name,
                                    error = %e,
                                    "Failed to create HTTP transport"
                                );
                                return (cfg.name.clone(), Vec::new());
                            }
                        }
                    }
                    _ => {
                        tracing::warn!(
                            mcp_server = %cfg.name,
                            "MCP server config must have either 'command' or 'url'"
                        );
                        return (cfg.name.clone(), Vec::new());
                    }
                };

                let resolved_config = agentos_mcp::McpServerResolvedConfig {
                    name: cfg.name.clone(),
                    timeout_secs: cfg.timeout_secs.unwrap_or(30),
                    auto_reconnect: cfg.auto_reconnect,
                    health_check_interval_secs: cfg.health_check_interval_secs,
                };

                let policy = agentos_mcp::McpServerPolicy {
                    name: cfg.name.clone(),
                    max_response_bytes: cfg.max_response_bytes.unwrap_or(1024 * 1024),
                    allowed_tools: cfg.allowed_tools.clone(),
                    denied_tools: cfg.denied_tools.clone(),
                    rate_limit_rpm: cfg.rate_limit_rpm.unwrap_or(60),
                };

                // Register security policy unconditionally — even if the server
                // fails to connect now, the health loop may reconnect it later.
                security_gate.register_server_policy(policy).await;

                match supervisor
                    .add_server_with_factory(resolved_config, transport, transport_factory)
                    .await
                {
                    Ok(tools) => {
                        tracing::info!(
                            mcp_server = %cfg.name,
                            tools = tools.len(),
                            "MCP server connected"
                        );
                        (cfg.name.clone(), tools)
                    }
                    Err(e) => {
                        tracing::warn!(
                            mcp_server = %cfg.name,
                            error = %e,
                            "MCP server connection failed"
                        );
                        (cfg.name.clone(), Vec::new())
                    }
                }
            });
            mcp_add_tasks.push(task);
        }

        // Wait for all servers to complete and register their tools.
        let mut seen: std::collections::HashSet<String> =
            tool_runner.list_tools().into_iter().collect();
        for task in mcp_add_tasks {
            if let Ok((server_name, tools)) = task.await {
                for tool_def in tools {
                    if seen.contains(&tool_def.name) {
                        tracing::warn!(
                            mcp_server = %server_name,
                            tool = %tool_def.name,
                            "Skipping MCP tool — name conflicts with existing tool"
                        );
                        continue;
                    }
                    seen.insert(tool_def.name.clone());
                    let adapter = agentos_mcp::McpToolAdapter::new(
                        Arc::clone(&mcp_supervisor),
                        Arc::clone(&mcp_security_gate),
                        server_name.clone(),
                        tool_def,
                    );
                    tool_runner.register(Box::new(adapter));
                }
            }
        }

        // 6.7 Load persisted runtime MCP attachments (from previous `mcp attach` calls).
        //
        // These are spawned sequentially after config-based servers so they use the
        // same `seen` set and skip any name collisions with already-registered tools.
        {
            match mcp_attachment_store.list_all().await {
                Ok(records) => {
                    for record in records {
                        tracing::info!(
                            mcp_server = %record.name,
                            "Restoring persisted MCP attachment"
                        );

                        // Resolve vault secrets in env vars. Abort this server
                        // if any required secret is missing — a partial env would
                        // cause confusing auth failures downstream.
                        let mut resolved_env: std::collections::HashMap<String, String> =
                            std::collections::HashMap::new();
                        let mut env_ok = true;
                        for (k, v) in &record.env {
                            if let Some(secret_name) = v.strip_prefix("vault:") {
                                match vault.get(secret_name).await {
                                    Ok(s) => {
                                        resolved_env.insert(k.clone(), s.as_str().to_string());
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            mcp_server = %record.name,
                                            env_key = %k,
                                            secret = %secret_name,
                                            error = %e,
                                            "vault secret missing for persisted MCP env var — skipping server"
                                        );
                                        env_ok = false;
                                        break;
                                    }
                                }
                            } else {
                                resolved_env.insert(k.clone(), v.clone());
                            }
                        }
                        if !env_ok {
                            continue;
                        }

                        let transport_factory: Option<Arc<dyn agentos_mcp::McpTransportFactory>>;
                        let transport: Arc<dyn agentos_mcp::McpTransport> = match (
                            &record.command,
                            &record.url,
                        ) {
                            (Some(cmd), None) => {
                                let factory = Arc::new(
                                    agentos_mcp::transport::stdio::StdioTransportFactory::new(
                                        format!("stdio:{}", record.name),
                                        cmd.clone(),
                                        record.args.clone(),
                                        resolved_env.clone(),
                                        None,
                                        record.timeout_secs,
                                    ),
                                );
                                transport_factory = Some(factory);
                                match agentos_mcp::transport::stdio::StdioTransport::spawn(
                                    format!("stdio:{}", record.name),
                                    cmd.clone(),
                                    record.args.clone(),
                                    resolved_env,
                                    None,
                                    record.timeout_secs,
                                )
                                .await
                                {
                                    Ok(t) => Arc::new(t),
                                    Err(e) => {
                                        tracing::warn!(mcp_server = %record.name, error = %e, "Failed to restore persisted MCP server");
                                        continue;
                                    }
                                }
                            }
                            (None, Some(url)) => {
                                transport_factory = None;
                                // OAuth2 mode takes precedence over static token.
                                if let Some(ref connector_id) = record.oauth_connector_id {
                                    let provider =
                                        match crate::mcp_oauth_provider::VaultOAuthProvider::new(
                                            connector_id.clone(),
                                            &vault,
                                        ) {
                                            Ok(p) => Arc::new(p),
                                            Err(e) => {
                                                tracing::warn!(mcp_server = %record.name, error = %e, "Failed to build OAuth provider on restore — skipping");
                                                continue;
                                            }
                                        };
                                    match agentos_mcp::transport::http::StreamableHttpTransport::new_with_oauth(
                                        format!("http:{}", record.name),
                                        url.clone(),
                                        provider,
                                        record.timeout_secs,
                                    ) {
                                        Ok(t) => Arc::new(t),
                                        Err(e) => {
                                            tracing::warn!(mcp_server = %record.name, error = %e, "Failed to restore persisted MCP OAuth HTTP server");
                                            continue;
                                        }
                                    }
                                } else {
                                    // Resolve vault:KEY reference — static tokens are auto-vaulted
                                    // at attach time, so the persisted value is "vault:mcp.<name>.auth_token".
                                    let resolved_token = match &record.auth_token {
                                        Some(v) if v.starts_with("vault:") => {
                                            let key = &v["vault:".len()..];
                                            match vault.get(key).await {
                                                Ok(s) => Some(s.as_str().to_string()),
                                                Err(e) => {
                                                    tracing::warn!(
                                                        mcp_server = %record.name,
                                                        vault_key = %key,
                                                        error = %e,
                                                        "Failed to resolve auth_token from vault on restore — skipping server"
                                                    );
                                                    continue;
                                                }
                                            }
                                        }
                                        other => other.clone(),
                                    };
                                    match agentos_mcp::transport::http::StreamableHttpTransport::new(
                                        format!("http:{}", record.name),
                                        url.clone(),
                                        resolved_token,
                                        record.timeout_secs,
                                    ) {
                                        Ok(t) => Arc::new(t),
                                        Err(e) => {
                                            tracing::warn!(mcp_server = %record.name, error = %e, "Failed to restore persisted MCP HTTP server");
                                            continue;
                                        }
                                    }
                                }
                            }
                            _ => {
                                tracing::warn!(mcp_server = %record.name, "Persisted MCP attachment has neither command nor url — skipping");
                                continue;
                            }
                        };

                        let resolved_config = agentos_mcp::McpServerResolvedConfig {
                            name: record.name.clone(),
                            timeout_secs: record.timeout_secs.unwrap_or(30),
                            auto_reconnect: true,
                            health_check_interval_secs: 30,
                        };
                        let policy = agentos_mcp::McpServerPolicy {
                            name: record.name.clone(),
                            max_response_bytes: 1024 * 1024,
                            allowed_tools: vec![],
                            denied_tools: vec![],
                            rate_limit_rpm: 60,
                        };
                        mcp_security_gate.register_server_policy(policy).await;

                        match mcp_supervisor
                            .add_server_with_factory(resolved_config, transport, transport_factory)
                            .await
                        {
                            Ok(tools) => {
                                for tool_def in tools {
                                    if seen.contains(&tool_def.name) {
                                        tracing::warn!(mcp_server = %record.name, tool = %tool_def.name, "Skipping restored MCP tool — name conflict");
                                        continue;
                                    }
                                    seen.insert(tool_def.name.clone());

                                    // Register into ToolRegistry (LLM visibility).
                                    let manifest = agentos_types::ToolManifest {
                                        manifest: agentos_types::tool::ToolInfo {
                                            name: tool_def.name.clone(),
                                            version: "0.1.0".to_string(),
                                            description: tool_def.description.clone(),
                                            author: format!("mcp:{}", record.name),
                                            checksum: None,
                                            author_pubkey: None,
                                            signature: None,
                                            trust_tier: agentos_types::TrustTier::Core,
                                            tags: Some(vec![
                                                "mcp".to_string(),
                                                record.name.clone(),
                                            ]),
                                            capability_tags: vec![],
                                            group: String::new(),
                                        },
                                        capabilities_required:
                                            agentos_types::tool::ToolCapabilities {
                                                permissions: vec![format!(
                                                    "mcp.{}",
                                                    tool_def.name.replace('-', "_").to_lowercase()
                                                )],
                                            },
                                        capabilities_provided: agentos_types::tool::ToolOutputs {
                                            outputs: vec!["content.text".to_string()],
                                        },
                                        intent_schema: agentos_types::tool::ToolSchema {
                                            input: "McpToolInput".to_string(),
                                            output: "McpToolOutput".to_string(),
                                        },
                                        payload_schema: Some(tool_def.input_schema.clone()),
                                        examples: vec![],
                                        sandbox: agentos_types::ToolSandbox {
                                            network: true,
                                            fs_write: false,
                                            gpu: false,
                                            max_memory_mb: 256,
                                            max_cpu_ms: 30_000,
                                            syscalls: vec![],
                                            weight: Some("network".to_string()),
                                        },
                                        executor: agentos_types::ToolExecutor::default(),
                                        fallbacks: vec![],
                                        // MCP tools may perform arbitrary operations — default
                                        // to ExecCapable so approval is required unless the
                                        // operator has an explicit auto-approve rule.
                                        risk_class: agentos_types::RiskClass::ExecCapable,
                                        usage_hints: None,
                                        tags: vec![],
                                    };
                                    {
                                        let mut reg = tool_registry.write().await;
                                        let _ = reg.register(manifest);
                                    }

                                    // Register into ToolRunner via dynamic path so
                                    // `mcp detach` can remove it via unregister_dynamic.
                                    let adapter = agentos_mcp::McpToolAdapter::new(
                                        Arc::clone(&mcp_supervisor),
                                        Arc::clone(&mcp_security_gate),
                                        record.name.clone(),
                                        tool_def,
                                    );
                                    tool_runner.register_dynamic(Box::new(adapter));
                                }
                            }
                            Err(e) => {
                                tracing::warn!(mcp_server = %record.name, error = %e, "Failed to reconnect persisted MCP server");
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to load persisted MCP attachments — continuing without them");
                }
            }

            // After MCP restore: rebuild tool_summaries so boot-restored MCP tools
            // are visible to agent-manual / list-tools / describe-tool / search-tools.
            //
            // Boot ordering: tool_summaries_shared is initialised earlier (line ~2819)
            // from a registry snapshot taken BEFORE this MCP restore loop, and the
            // lifecycle_sender is wired up AFTER it. So lifecycle events fired during
            // restore are silently dropped and the initial snapshot misses every
            // MCP tool. Without this explicit refresh agents cannot discover any
            // boot-restored MCP tools at all (only runtime `mcp attach` would work,
            // because by then the listener is alive and refreshes summaries on each
            // ToolInstalled event).
            {
                let registry_read = tool_registry.read().await;
                let all_tools = registry_read.list_all();
                let fresh = agentos_tools::agent_manual::AgentManualTool::summaries_from_registry(
                    &all_tools,
                );
                let count = fresh.len();
                *tool_summaries_shared.write().await = fresh;
                tracing::info!(
                    tool_count = count,
                    "tool_summaries refreshed after MCP restore"
                );
            }
        }

        // 6.8 Spawn health check loop.
        let _health_loop_handle = mcp_supervisor.spawn_health_loop();

        // 6.9 Forward MCP lifecycle events to audit log.
        {
            tokio::spawn(async move {
                while let Some(event) = mcp_event_rx.recv().await {
                    match &event {
                        agentos_mcp::McpLifecycleEvent::ServerConnected { name, tool_count } => {
                            tracing::info!(server = %name, tools = tool_count, "MCP lifecycle: connected");
                        }
                        agentos_mcp::McpLifecycleEvent::ServerDisconnected { name, error } => {
                            tracing::warn!(server = %name, error = %error, "MCP lifecycle: disconnected");
                        }
                        agentos_mcp::McpLifecycleEvent::ServerReconnecting { name, attempt } => {
                            tracing::info!(server = %name, attempt = attempt, "MCP lifecycle: reconnecting");
                        }
                        agentos_mcp::McpLifecycleEvent::ServerStopped { name } => {
                            tracing::info!(server = %name, "MCP lifecycle: stopped");
                        }
                        agentos_mcp::McpLifecycleEvent::ToolCallCompleted { .. } => {}
                    }
                }
            });
        }

        let tool_runner = Arc::new(tool_runner);
        let sandbox = Arc::new(SandboxExecutor::new(
            data_dir.clone(),
            config.kernel.max_concurrent_sandbox_children,
        ));
        tracing::info!(
            sandbox_policy = ?config.kernel.sandbox_policy,
            max_concurrent_sandbox_children = config.kernel.max_concurrent_sandbox_children,
            "Sandbox execution policy configured"
        );
        let scheduler = Arc::new(TaskScheduler::with_state_store(
            config.kernel.max_concurrent_tasks,
            Some(state_store.clone()),
        ));
        let active_llms: Arc<RwLock<HashMap<AgentID, Arc<dyn LLMCore>>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let mut context_budget = config.context_budget.clone();
        if let Err(e) = context_budget.validate() {
            tracing::warn!("Invalid context budget config: {} — using defaults", e);
            context_budget = TokenBudget::default();
        }
        let context_compiler = Arc::new(crate::context_compiler::ContextCompiler::new(
            context_budget,
        ));
        let agent_registry = Arc::new(RwLock::new(AgentRegistry::with_persistence(
            data_dir.clone(),
        )));
        let router = Arc::new(crate::router::TaskRouter::new(
            config.routing.strategy.clone(),
            config.routing.rules.clone(),
        ));
        let message_bus = Arc::new(crate::agent_message_bus::AgentMessageBus::new());
        let profile_manager = Arc::new(ProfileManager::new());
        let retrieval_gate = Arc::new(crate::retrieval_gate::RetrievalGate::new(5));
        let retrieval_executor = Arc::new(crate::retrieval_gate::RetrievalExecutor::new(
            semantic_memory.clone(),
            episodic_memory.clone(),
            procedural_memory.clone(),
            tool_registry.clone(),
        ));
        let mut extraction_registry = crate::memory_extraction::ExtractionRegistry::new();
        extraction_registry.register_defaults();
        let memory_extraction = Arc::new(crate::memory_extraction::MemoryExtractionEngine::new(
            extraction_registry,
            semantic_memory.clone(),
            config.memory.extraction.clone(),
        ));
        let consolidation_engine = Arc::new(crate::consolidation::ConsolidationEngine::new(
            episodic_memory.clone(),
            procedural_memory.clone(),
            config.memory.consolidation.clone(),
        ));
        let memory_blocks = Arc::new(crate::memory_blocks::MemoryBlockStore::open(&data_dir)?);
        let context_memory_store = Arc::new(crate::context_memory_store::ContextMemoryStore::open(
            &data_dir.join(&config.memory.context.db_path),
            config.memory.context.max_tokens,
            config.memory.context.max_versions,
            config.context_budget.chars_per_token,
        )?);
        let schedule_persistence = Arc::new(
            crate::schedule_persistence::SchedulePersistence::new(&data_dir)
                .map_err(|e| anyhow::anyhow!("Schedule persistence init failed: {}", e))?,
        );
        let schedule_store = Arc::new(
            crate::schedule_store::ScheduleStore::open(data_dir.join("schedules.db"))
                .await
                .map_err(|e| anyhow::anyhow!("Schedule store init failed: {}", e))?,
        );
        // Sweep orphaned `Running` runs left over from a kernel crash.
        // Threshold 1h — anything still Running that long without a completion
        // event must be stale; mark as Failed so visibility tools and the
        // delivery sweeper don't treat them as in-flight forever.
        match schedule_store
            .mark_orphaned_runs_failed(chrono::Duration::hours(1))
            .await
        {
            Ok(n) if n > 0 => {
                tracing::warn!(
                    orphaned = n,
                    "Marked orphaned scheduled runs as Failed on boot"
                );
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(error = %e, "Orphaned-run sweep failed on boot");
            }
        }
        let schedule_manager = Arc::new(
            ScheduleManager::with_persistence_and_store(
                schedule_persistence.clone(),
                Some(schedule_store.clone()),
            )
            .await
            .map_err(|e| anyhow::anyhow!("Schedule manager rehydration failed: {}", e))?,
        );
        tracing::info!(
            schedule_count = schedule_manager.list_jobs().await.len(),
            once_count = schedule_manager.list_once_jobs().await.len(),
            timer_count = schedule_manager.list_timers().await.len(),
            "Schedule manager rehydrated from disk"
        );
        let background_pool = Arc::new(BackgroundPool::new());

        // 6.5 Initialize pipeline engine
        let pipeline_store = Arc::new(
            PipelineStore::open(&data_dir.join("pipelines.db"))
                .map_err(|e| anyhow::anyhow!("Pipeline store init failed: {}", e))?,
        );
        let pipeline_engine = Arc::new(PipelineEngine::new(pipeline_store));

        // Pre-populate the message bus pubkey map from the persisted agent registry.
        // This ensures agents that were registered in a prior kernel session can
        // authenticate their messages immediately on reconnect, before the
        // `cmd_connect_agent` flow has a chance to run `register_pubkey_internal`.
        {
            let registry = agent_registry.read().await;
            for agent in registry.list_all() {
                if let Some(ref pk) = agent.public_key_hex {
                    if let Err(e) = message_bus
                        .register_pubkey_internal(agent.id, pk.clone())
                        .await
                    {
                        // Should not happen at boot — each agent ID is unique in the registry.
                        tracing::warn!(
                            agent_id = %agent.id,
                            error = %e,
                            "Skipped pubkey pre-population at boot"
                        );
                    }
                }
            }
        }

        // 7. Start bus server
        let bus = Arc::new(BusServer::bind(Path::new(&config.bus.socket_path)).await?);

        let identity_manager = Arc::new(crate::identity::IdentityManager::new(vault.clone()));

        let checkpoint_store = Arc::new(
            crate::checkpoint_store::CheckpointStore::open(data_dir.join("checkpoints.db"))
                .await
                .map_err(|e| anyhow::anyhow!("CheckpointStore init failed: {e}"))?,
        );

        // Atomic task checkout store. Single-owner dispatch claim; in-memory
        // fallback on disk-open failure (claims then don't survive restart, but
        // dispatch still works) rather than aborting boot.
        let task_checkout_store = Arc::new(
            match crate::task_checkout_store::TaskCheckoutStore::open(
                &data_dir.join("task_checkout.db"),
            ) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = %e, "task_checkout.db open failed; using in-memory checkout store");
                    crate::task_checkout_store::TaskCheckoutStore::in_memory()
                        .map_err(|e| anyhow::anyhow!("in-memory task checkout store init: {e}"))?
                }
            },
        );

        // Opt-in claude-code session-resume cache. Only built when enabled, so the
        // default path never opens the DB. A failure to open degrades gracefully
        // to an in-memory cache (resume still works within the process) rather than
        // aborting boot — the session is a cache, never a source of truth.
        let claude_session_lookup = if config.llm.claude_code_resume {
            let store = match crate::claude_session_store::ClaudeSessionStore::open(
                &data_dir.join("claude_session.db"),
            ) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "claude_session.db open failed; using in-memory resume cache"
                    );
                    crate::claude_session_store::ClaudeSessionStore::in_memory()
                        .map_err(|e| anyhow::anyhow!("in-memory claude session store init: {e}"))?
                }
            };
            Some(Arc::new(
                crate::claude_session_store::KernelClaudeSessionLookup::new(Arc::new(store)),
            ))
        } else {
            None
        };

        // User filesystem grants: durable, runtime-mutable list of host directories
        // each agent (or all agents) may read/write/exec inside. Populated from
        // CLI/web/bus; legacy `tools.workspace.allowed_paths` are imported once below.
        let workspace_grant_store = Arc::new(
            crate::workspace_grant_store::WorkspaceGrantStore::open(
                data_dir.join("workspace_grants.db"),
            )
            .await
            .map_err(|e| anyhow::anyhow!("WorkspaceGrantStore init failed: {e}"))?,
        );
        // Import config.tools.workspace.allowed_paths as global grants on first boot.
        // Subsequent boots are no-ops because the unique index rejects duplicates;
        // the duplicate error is matched structurally on its sentinel `resource`.
        for legacy in &config.tools.workspace.allowed_paths {
            let p = std::path::Path::new(legacy);
            match workspace_grant_store.grant(
                p,
                None,
                agentos_types::WorkspaceGrantMode::READ_WRITE,
                "config",
                "kernel-boot",
            ) {
                Ok(_) => tracing::info!(path = %legacy, "Imported legacy workspace path as grant"),
                Err(agentos_types::AgentOSError::PermissionDenied { resource, .. })
                    if resource == crate::workspace_grant_store::GRANT_DUPLICATE_RESOURCE =>
                {
                    tracing::debug!(path = %legacy, "Legacy workspace path already imported");
                }
                Err(e) => {
                    tracing::warn!(path = %legacy, error = %e, "Failed to import legacy workspace path");
                }
            }
        }
        let workspace_grants = Arc::new(
            crate::workspace_grant_store::WorkspaceGrantRegistry::load(workspace_grant_store)
                .map_err(|e| anyhow::anyhow!("WorkspaceGrantRegistry load failed: {e}"))?,
        );

        let snapshot_manager = Arc::new(crate::snapshot::SnapshotManager::new(
            data_dir.join("snapshots"),
            data_dir.clone(), // allowed_root: only paths within data_dir may be snapshotted
            72,               // hours
        ));

        let trace_collector = Arc::new(
            crate::trace_collector::TraceCollector::new(&data_dir.join("traces.db"))
                .map_err(|e| anyhow::anyhow!("TraceCollector init failed: {e}"))?,
        );
        let otel = Arc::new(crate::otel_exporter::OtelExporter::from_config(
            &config.otel,
        )?);

        let event_bus = Arc::new(crate::event_bus::EventBus::new());
        let escalation_manager = Arc::new(crate::escalation::EscalationManager::with_state_store(
            Some(state_store.clone()),
        ));
        let cost_tracker = Arc::new(crate::cost_tracker::CostTracker::with_state_store(Some(
            state_store.clone(),
        )));

        let context_manager = Arc::new(ContextManager::with_full_config(
            config.kernel.context_window_max_entries,
            config.kernel.context_window_token_budget,
            active_llms.clone(),
            cost_tracker.clone(),
            config.context.clone(),
        ));

        let restored_tasks = scheduler.restore_from_store().await?;
        let restored_escalations = escalation_manager.restore_from_store().await?;
        let restored_cost_snapshots = cost_tracker.restore_from_store().await?;
        tracing::info!(
            restored_tasks,
            restored_escalations,
            restored_cost_snapshots,
            "Restored persisted kernel runtime state"
        );

        // Discover tasks with checkpoints available for resume (informational only).
        match checkpoint_store.list_checkpoints().await {
            Ok(summaries) if !summaries.is_empty() => {
                tracing::info!(
                    count = summaries.len(),
                    "Boot: found {} tasks with checkpoints — use 'agentos task resume <id>' to restore",
                    summaries.len()
                );
            }
            Ok(_) => {
                tracing::debug!("Boot: no checkpointed tasks found");
            }
            Err(e) => {
                tracing::warn!(error = %e, "Boot: failed to query checkpoint store");
            }
        }

        // Event channel capacity is configurable so operators can tune it under heavy
        // load without recompiling.  Subsidiary notification channels (tool lifecycle,
        // comm, schedule, arbiter) are internal-only and kept at a fixed 1 024 slots.
        let event_channel_capacity = config.kernel.events.channel_capacity;
        const NOTIF_CHANNEL_CAPACITY: usize = 1024;

        let (event_sender, event_receiver) = tokio::sync::mpsc::channel(event_channel_capacity);

        // Register IoT protocol drivers (feature-gated, config-conditional)
        #[cfg(feature = "mqtt")]
        {
            if let (Ok(host), Ok(port_str)) = (
                std::env::var("AGENTOS_MQTT_HOST"),
                std::env::var("AGENTOS_MQTT_PORT"),
            ) {
                if let Ok(port) = port_str.parse::<u16>() {
                    let client_id = std::env::var("AGENTOS_MQTT_CLIENT_ID")
                        .unwrap_or_else(|_| "agentos".to_string());
                    let creds = std::env::var("AGENTOS_MQTT_USER").ok().map(|user| {
                        let pass = std::env::var("AGENTOS_MQTT_PASS").unwrap_or_default();
                        (user, pass)
                    });
                    let creds_ref = creds.as_ref().map(|(u, p)| (u.as_str(), p.as_str()));
                    match MqttDriver::new(
                        &host,
                        port,
                        &client_id,
                        creds_ref,
                        kernel_cancellation_token.child_token(),
                    )
                    .await
                    {
                        Ok(driver) => {
                            hal.register(Box::new(driver));
                            tracing::info!(host = %host, port, "MQTT HAL driver registered");
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "Failed to initialize MQTT driver");
                        }
                    }
                }
            }
        }

        #[cfg(feature = "homeassistant")]
        {
            if let Ok(base_url) = std::env::var("AGENTOS_HA_URL") {
                let token = std::env::var("AGENTOS_HA_TOKEN").unwrap_or_default();
                if !token.is_empty() {
                    hal.register(Box::new(HomeAssistantDriver::new(&base_url, &token)));
                    tracing::info!(url = %base_url, "Home Assistant HAL driver registered");
                }
            }
        }

        let hal = Arc::new(
            hal.with_device_access_gate(Arc::new(KernelDeviceAccessGate::new(
                hardware_registry.clone(),
                escalation_manager.clone(),
                audit.clone(),
            )))
            .with_event_sink(Arc::new(KernelHalEventSink::new(
                capability_engine.clone(),
                audit.clone(),
                event_sender.clone(),
            ))),
        );

        // Create tool lifecycle notification channel and inject sender into registry.
        // The kernel receives these lightweight notifications and converts them into
        // properly HMAC-signed EventMessages with audit trail entries.
        let (tool_lifecycle_sender, tool_lifecycle_receiver) =
            tokio::sync::mpsc::channel(NOTIF_CHANNEL_CAPACITY);
        tool_registry
            .write()
            .await
            .set_lifecycle_sender(tool_lifecycle_sender);

        // Create notification channels for communication and schedule subsystems.
        // These subsystems send lightweight notifications; the kernel converts them
        // into properly HMAC-signed EventMessages with audit trail entries.
        let (comm_notif_sender, comm_notif_receiver) =
            tokio::sync::mpsc::channel(NOTIF_CHANNEL_CAPACITY);
        message_bus.set_notification_sender(comm_notif_sender).await;

        let (schedule_notif_sender, schedule_notif_receiver) =
            tokio::sync::mpsc::channel(NOTIF_CHANNEL_CAPACITY);
        schedule_manager
            .set_notification_sender(schedule_notif_sender)
            .await;

        // Create notification channel for resource arbiter (preemption/deadlock events).
        let (arbiter_notif_sender, arbiter_notif_receiver) =
            tokio::sync::mpsc::channel(NOTIF_CHANNEL_CAPACITY);

        let per_agent_rate_limit = config.kernel.per_agent_rate_limit;

        // Broadcast channel for task status updates (Phase 1 infra; Phase 2 attaches SSE).
        // Capacity 256 — old messages are silently evicted when no receivers are active.
        let (status_update_sender, _status_update_receiver_placeholder) =
            tokio::sync::broadcast::channel::<agentos_bus::StatusUpdate>(256);

        // Lossy broadcast of coarse realtime events for the control panel's
        // WebSocket/SSE layer. Capacity 512 — old events evicted when receivers lag.
        let (realtime_event_sender, _realtime_event_receiver_placeholder) =
            tokio::sync::broadcast::channel::<agentos_types::RealtimeEvent>(512);

        // Initialise the Unified Notification and Interaction System (UNIS).
        let agent_inbox = Arc::new(
            crate::agent_inbox::AgentInbox::new(
                &data_dir.join("agent_inbox.db"),
                config.notifications.max_inbox_size,
            )
            .map_err(|e| anyhow::anyhow!("AgentInbox init failed: {e}"))?,
        );
        let agent_message_inbox = Arc::new(
            crate::agent_message_inbox::AgentMessageInbox::new(
                &data_dir.join("agent_messages.db"),
                config.notifications.max_inbox_size,
            )
            .map_err(|e| anyhow::anyhow!("AgentMessageInbox init failed: {e}"))?,
        );
        let agent_inbox_writer = Arc::new(crate::agent_inbox_writer::AgentInboxWriter::new(
            Arc::clone(&agent_inbox),
            Arc::clone(&agent_message_inbox),
            30,
        ));

        let notification_router = {
            let inbox_path = data_dir.join("user_inbox.db");
            let inbox = Arc::new(
                crate::user_inbox::UserInbox::new(&inbox_path, config.notifications.max_inbox_size)
                    .map_err(|e| anyhow::anyhow!("UserInbox init failed: {e}"))?,
            );
            let router = Arc::new(crate::notification_router::NotificationRouter::new(
                inbox,
                audit.clone(),
            ));

            // Register pluggable delivery adapters from config.
            let adapter_cfg = &config.notifications.adapters;

            if adapter_cfg.desktop.enabled {
                let min_prio = crate::notification_router::parse_min_priority(
                    &adapter_cfg.desktop.min_priority,
                );
                router
                    .register_adapter(Arc::new(
                        crate::notification_router::DesktopDeliveryAdapter::new(
                            min_prio,
                            adapter_cfg.desktop.notify_on_task_complete,
                        ),
                    ))
                    .await;
            }

            if adapter_cfg.webhook.enabled {
                match crate::notification_router::WebhookDeliveryAdapter::from_config(
                    &adapter_cfg.webhook,
                ) {
                    Ok(adapter) => router.register_adapter(Arc::new(adapter)).await,
                    Err(e) => {
                        tracing::warn!(error = %e, "Webhook notification adapter disabled: invalid config")
                    }
                }
            }

            if adapter_cfg.slack.enabled {
                match crate::notification_router::SlackDeliveryAdapter::from_config(
                    &adapter_cfg.slack,
                ) {
                    Ok(adapter) => router.register_adapter(Arc::new(adapter)).await,
                    Err(e) => {
                        tracing::warn!(error = %e, "Slack notification adapter disabled: invalid config")
                    }
                }
            }

            router
        };

        // Phase 6: Bidirectional channel protocol.
        let channel_registry = {
            let db_path = data_dir.join("user_channels.db");
            Arc::new(
                crate::user_channel_registry::UserChannelRegistry::new(&db_path)
                    .map_err(|e| anyhow::anyhow!("UserChannelRegistry init failed: {e}"))?,
            )
        };
        let channel_listener_registry =
            Arc::new(crate::user_channel_registry::ChannelListenerRegistry::new());
        let inbound_chat_bridge = Arc::new(crate::channel_chat_bridge::KernelChatBridge::new());
        let (inbound_tx, inbound_rx) =
            tokio::sync::mpsc::channel::<crate::notification_router::InboundMessage>(512);
        // InboundRouter is spawned in `wire_inbound_chat_bridge` (after Arc::new(kernel))
        // so the bridge is guaranteed to be wired before the first inbound message is processed.

        // Initialize ChannelManager for bidirectional adapter management.
        let (channel_manager_inbound_tx, channel_manager_inbound_rx) =
            tokio::sync::mpsc::channel::<agentos_channels::types::InboundMessage>(256);
        let channel_manager_arc = Arc::new(agentos_channels::manager::ChannelManager::new(
            channel_manager_inbound_tx,
            kernel_cancellation_token.clone(),
        ));

        // Pairing manager: tracks the (channel_instance_id, sender_id) DM
        // allowlist used by the channel adapters AND by the escalation
        // broadcast sink (so approval prompts only go to paired senders).
        let pairing_manager = agentos_channels::pairing::PairingManager::new();

        // Wire the channel broadcast sink into the escalation manager so
        // every new PendingEscalation fans out to paired DM channels in
        // addition to the legacy `notify_url` webhook. The sink also
        // takes a handle to AuditLog so it can record
        // `EscalationBroadcastSuppressed` events when dedupe or rate
        // limits withhold a prompt — operators must see what was missed.
        escalation_manager
            .add_sink(Arc::new(
                crate::escalation_channel_sink::ChannelBroadcastSink::with_audit(
                    Arc::clone(&channel_manager_arc),
                    Arc::clone(&pairing_manager),
                    Arc::clone(&audit),
                ),
            ))
            .await;

        let connector_registry = Arc::new(agentos_connectors::ConnectorRegistry::new(Arc::clone(
            &vault,
        )));

        // Container runtime — attempt Docker connection, fall back to None
        let quota_enforcer = Arc::new(agentos_runtime::QuotaEnforcer::new(
            agentos_runtime::ContainerQuota::default(),
        ));
        let compute_runtime: Option<Arc<dyn agentos_runtime::ComputeRuntime>> =
            match agentos_runtime::DockerRuntime::new(vec![
                "python:3.11-slim".into(),
                "python:3.12-slim".into(),
                "node:20-alpine".into(),
                "node:22-alpine".into(),
                "ubuntu:22.04".into(),
                "ubuntu:24.04".into(),
                "rust:1.78-slim".into(),
                "alpine:3.19".into(),
            ])
            .await
            {
                Ok(rt) => {
                    tracing::info!("Container runtime (Docker) initialized");
                    Some(Arc::new(rt))
                }
                Err(e) => {
                    tracing::info!(error = %e, "Docker not available — container runtime disabled");
                    None
                }
            };

        let webhook_db_path = data_dir.join("webhook_endpoints.db");
        let webhook_registry =
            Arc::new(crate::webhook_registry::WebhookRegistry::new(&webhook_db_path).await?);

        let webhook_throttle = Arc::new(crate::webhook_throttle::WebhookThrottle::new(60, 30));
        let (webhook_batch_tx, webhook_batch_rx) = tokio::sync::mpsc::channel(256);
        let webhook_batcher = Arc::new(crate::webhook_batcher::WebhookBatcher::new(
            webhook_batch_tx,
            50,
        ));

        // Task tool classifier for native-array category scoping (Phase 3).
        // Only the zero-cost heuristic is implemented today; "heuristic+semantic"
        // / "llm" degrade to heuristic with a log until those land.
        let tool_classifier: Arc<dyn crate::tool_scoping::TaskToolClassifier> = {
            let mode = config.tools.discovery.scoping_classifier.clone();
            if mode != "heuristic" {
                tracing::info!(
                    classifier = %mode,
                    "scoping_classifier not yet implemented; using heuristic"
                );
            }
            Arc::new(crate::tool_scoping::HeuristicClassifier)
        };

        // Proactive recommendation engine (Phase 4). When disabled, use an in-memory
        // store so no `recommendations.db` file is created (Phase 6 invariant).
        let recommendations_store = Arc::new(if config.personalization.enabled {
            match crate::recommendations_store::RecommendationsStore::open(
                data_dir.join("recommendations.db"),
            )
            .await
            {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to open recommendations.db — falling back to in-memory");
                    crate::recommendations_store::RecommendationsStore::open_in_memory()
                        .await
                        .map_err(|e2| {
                            anyhow::anyhow!("RecommendationsStore in-memory fallback failed: {e2}")
                        })?
                }
            }
        } else {
            crate::recommendations_store::RecommendationsStore::open_in_memory()
                .await
                .map_err(|e| anyhow::anyhow!("RecommendationsStore in-memory init failed: {e}"))?
        });
        let recommendation_engine =
            Arc::new(crate::recommendation_engine::RecommendationEngine::new(
                recommendations_store,
                interest_model.clone(),
                user_profile_store.clone(),
                notification_router.clone(),
                audit.clone(),
                &config.personalization,
            ));

        // Phase 5: feedback loop processor — applies accept/dismiss/restate
        // signals and runs the hourly profile decay/archival sweep.
        let feedback_processor = Arc::new(crate::personalization_feedback::FeedbackProcessor::new(
            Arc::clone(&user_profile_store),
            user_interests_store_for_feedback,
            Arc::clone(&audit),
            crate::personalization_feedback::PersonalizationFeedbackConfig {
                pin_rank_decay_half_life_days: config.personalization.pin_rank_decay_half_life_days,
                profile_archive_idle_days: config.personalization.profile_archive_idle_days,
                dismiss_cooldown_hours: config.personalization.dismiss_cooldown_hours,
                restate_confidence_boost: config.personalization.restate_confidence_boost,
            },
        ));

        let audit_for_dispatcher = Arc::clone(&audit);
        let kernel = Kernel {
            config,
            audit,
            vault,
            capability_engine,
            scheduler,
            context_manager,
            context_compiler,
            tool_registry: tool_registry.clone(),
            agent_registry,
            bus,
            tool_runner,
            tool_summaries: tool_summaries_shared,
            tool_usage,
            sandbox,
            router,
            active_llms,
            image_resolver: std::sync::RwLock::new(Arc::new(NoopImageResolver)),
            attachment_sink: Arc::new(std::sync::RwLock::new(Arc::new(
                crate::attachment_sink::NoopAttachmentSink,
            ))),
            message_bus,
            profile_manager,
            episodic_memory,
            semantic_memory,
            procedural_memory,
            retrieval_gate,
            retrieval_executor,
            memory_extraction,
            consolidation_engine,
            memory_blocks,
            context_memory_store,
            scratchpad_store: scratchpad_store.clone(),
            file_store: file_store.clone(),
            chat_store: chat_store.clone(),
            convo_store: convo_store.clone(),
            user_profile_store,
            user_profile_l0_cache: std::sync::Mutex::new(None),
            interest_model,
            recommendation_engine,
            feedback_processor,
            skill_registry,
            schedule_manager,
            background_pool,
            hal,
            hardware_registry,
            schema_registry,
            pipeline_engine,
            intent_validator: Arc::new(crate::intent_validator::IntentValidator::new()),
            escalation_manager,
            cost_tracker,
            risk_classifier: Arc::new(crate::risk_classifier::RiskClassifier::new()),
            tool_classifier,
            identity_manager,
            injection_scanner: Arc::new(crate::injection_scanner::InjectionScanner::new()),
            resource_arbiter: {
                let mut arbiter = crate::resource_arbiter::ResourceArbiter::new();
                arbiter.set_arbiter_sender(arbiter_notif_sender);
                Arc::new(arbiter)
            },
            checkpoint_store,
            workspace_grants,
            task_checkout_store,
            claude_session_lookup,
            approval_mode_resolver: None,
            approval_policy_matcher: None,
            mcp_attachment_store,
            user_pref_proposal_store,
            snapshot_manager,
            trace_collector,
            rpc_manager: Arc::new(crate::rpc_manager::RpcManager::new()),
            otel,
            event_bus,
            notification_router,
            agent_inbox,
            agent_message_inbox,
            agent_inbox_writer,
            channel_registry,
            channel_listener_registry,
            connected_channels_snapshot: connected_channels_shared,
            installed_skills_snapshot: installed_skills_shared,
            inbound_tx,
            inbound_chat_bridge,
            pending_inbound_rx: std::sync::Mutex::new(Some(inbound_rx)),
            webhook_secrets: Arc::new(RwLock::new(HashMap::new())),
            connector_registry,
            compute_runtime,
            quota_enforcer,
            webhook_registry,
            webhook_throttle,
            webhook_batcher,
            webhook_batch_rx: Arc::new(tokio::sync::Mutex::new(Some(webhook_batch_rx))),
            status_update_sender,
            realtime_event_sender,
            task_scoped_subscriptions: Arc::new(RwLock::new(HashMap::new())),
            event_sender,
            event_receiver: Arc::new(tokio::sync::Mutex::new(event_receiver)),
            tool_lifecycle_receiver: Arc::new(tokio::sync::Mutex::new(tool_lifecycle_receiver)),
            comm_notification_receiver: Arc::new(tokio::sync::Mutex::new(comm_notif_receiver)),
            schedule_notification_receiver: Arc::new(tokio::sync::Mutex::new(
                schedule_notif_receiver,
            )),
            arbiter_notification_receiver: Arc::new(tokio::sync::Mutex::new(
                arbiter_notif_receiver,
            )),
            per_agent_rate_limiter: Arc::new(tokio::sync::Mutex::new(
                crate::rate_limit::PerAgentRateLimiter::new(per_agent_rate_limit),
            )),
            mcp_supervisor,
            mcp_security_gate,
            provider_catalog,
            catalog_path: resolved_catalog_path,
            data_dir,
            config_path: config_path.to_path_buf(),
            workspace_paths,
            started_at: chrono::Utc::now(),
            cancellation_token: kernel_cancellation_token,
            shutdown_audited: std::sync::atomic::AtomicBool::new(false),
            channel_manager: channel_manager_arc,
            channel_manager_rx: Arc::new(tokio::sync::Mutex::new(channel_manager_inbound_rx)),
            pairing_manager,
            host_package_policy,
            hook_registry: Arc::clone(&hook_registry_arc),
            plugin_registry: crate::plugin_registry::PluginRegistry::new(
                Arc::clone(&hook_registry_arc),
                Arc::clone(&tool_registry),
            ),
            capability_registry: Arc::new(RwLock::new(
                crate::capability_registry::CapabilityRegistry::new(),
            )),
            zone_table: crate::managed_storage::ZoneTable::new(),
            process_table: crate::managed_process::ProcessTable::default(),
            capability_broker: Arc::new(crate::capability_broker::CapabilityBroker::with_defaults()),
            policy_engine: Arc::new(RwLock::new(
                crate::policy_engine::PolicyEngine::development_profile(),
            )),
            // Placeholder — wired with actual registry reference immediately below.
            capability_dispatcher: Arc::new(
                crate::capability_dispatch::KernelCapabilityDispatcher::new(
                    Arc::new(RwLock::new(
                        crate::capability_registry::CapabilityRegistry::new(),
                    )),
                    Arc::clone(&audit_for_dispatcher),
                ),
            ),
            agent_tool_lru: Arc::new(RwLock::new(HashMap::new())),
            chat_session_dedup: Arc::new(RwLock::new(HashMap::new())),
        };

        // Re-wire the dispatcher to use the actual registry (the one with providers registered).
        // This is safe because we haven't shared `kernel` yet.
        // SAFETY: We need mut to reassign — this is the only place that modifies it.
        // Re-create dispatcher with actual registry reference.
        let capability_dispatcher =
            Arc::new(crate::capability_dispatch::KernelCapabilityDispatcher::new(
                Arc::clone(&kernel.capability_registry),
                Arc::clone(&kernel.audit),
            ));
        let kernel = {
            let mut k = kernel;
            k.capability_dispatcher = capability_dispatcher;
            k
        };

        // Register built-in capability providers (KMC).
        let zone_table = kernel.zone_table.clone();
        {
            // Open the workspace persistence store. Failures fall back to an
            // in-memory provider so the kernel still boots — operators get a
            // warning, not a hard crash.
            let workspaces_db =
                std::path::PathBuf::from(&kernel.config.tools.data_dir).join("workspaces.db");
            let data_dir_for_drift = std::path::PathBuf::from(&kernel.config.tools.data_dir);
            let env_provider = match crate::workspace_store::WorkspaceStore::open(workspaces_db)
                .await
            {
                Ok(store) => {
                    let store = Arc::new(store);
                    match crate::managed_env::EnvProvider::from_config_with_store(
                        &kernel.config.env,
                        store,
                    )
                    .await
                    {
                        Ok(p) => {
                            // Best-effort warn-only reconciliation between
                            // workspaces.db and on-disk workspace directories.
                            p.warn_on_disk_drift(&data_dir_for_drift).await;
                            Arc::new(p)
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "failed to load workspaces from DB; starting with empty in-memory state");
                            Arc::new(crate::managed_env::EnvProvider::from_config(
                                &kernel.config.env,
                            ))
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to open workspaces.db; workspace state will be in-memory only");
                    Arc::new(crate::managed_env::EnvProvider::from_config(
                        &kernel.config.env,
                    ))
                }
            };
            let storage_provider =
                crate::managed_storage::StorageProvider::with_defaults(zone_table.clone());
            let mut reg = kernel.capability_registry.write().await;
            if let Err(e) = reg.register(env_provider.clone()) {
                tracing::warn!("Failed to register env capability provider: {e}");
            }
            if let Err(e) = reg.register(Arc::new(storage_provider)) {
                tracing::warn!("Failed to register storage capability provider: {e}");
            }
            // Use the kernel-owned ProcessTable so wire_process_crash_emission
            // (called after Arc::new(kernel)) can install a callback that
            // emits ProcessCrashed events from the same table the provider
            // uses to track child processes.
            let process_provider = crate::managed_process::ProcessProvider::with_resolver(
                kernel.process_table.clone(),
                env_provider.clone() as Arc<dyn crate::managed_env::WorkspaceResolver>,
            );
            if let Err(e) = reg.register(Arc::new(process_provider)) {
                tracing::warn!("Failed to register proc capability provider: {e}");
            }
            let network_provider = crate::managed_network::NetworkProvider::with_defaults();
            if let Err(e) = reg.register(Arc::new(network_provider)) {
                tracing::warn!("Failed to register net capability provider: {e}");
            }
            let build_provider = crate::managed_build::BuildProvider::with_resolver(
                crate::managed_build::BuildConfig::default(),
                env_provider.clone() as Arc<dyn crate::managed_env::WorkspaceResolver>,
            );
            if let Err(e) = reg.register(Arc::new(build_provider)) {
                tracing::warn!("Failed to register build capability provider: {e}");
            }
        }

        // Register the built-in audit hook as the first hook.
        // It fires on every event and writes to the append-only AuditLog.
        {
            let audit_hook = crate::hooks::AuditHook::new(Arc::clone(&kernel.audit));
            kernel.hook_registry.register(audit_hook).await;
        }

        // Register the approval hook — creates escalations for high-risk tool calls.
        // Audit hook runs first so all tool calls are logged before approval can abort.
        let mode_resolver = crate::hooks::ApprovalModeResolver::new(
            kernel.config.approval.clone(),
            Arc::clone(&kernel.agent_registry),
        );
        // Operator-curated "allow always" policy store. Failure to open is
        // not fatal — the kernel falls back to the legacy in-memory policy.
        let policy_matcher: Option<Arc<crate::approval_policy_store::ApprovalPolicyMatcher>> = {
            let path = kernel.data_dir.join("approval_policy.db");
            match crate::approval_policy_store::ApprovalPolicyStore::open(path).await {
                Ok(store) => {
                    let store = Arc::new(store);
                    match crate::approval_policy_store::ApprovalPolicyMatcher::load(store) {
                        Ok(m) => Some(Arc::new(m)),
                        Err(e) => {
                            tracing::warn!(error = %e, "approval policy matcher load failed; running without learned policy");
                            None
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "approval policy store open failed; running without learned policy");
                    None
                }
            }
        };
        {
            let approval_hook = crate::hooks::ApprovalHook::new(
                crate::hooks::AutoApprovePolicy::default_rules(),
                Arc::clone(&kernel.escalation_manager),
                Arc::clone(&kernel.tool_registry),
                Arc::clone(&mode_resolver),
                policy_matcher.clone(),
            );
            kernel.hook_registry.register(approval_hook).await;
        }
        // Park the resolver and policy matcher on the Kernel so the CLI and
        // ConfigWatcher can mutate them at runtime. Same
        // `let kernel = { let mut k = kernel; ...; k };` pattern used above
        // for `capability_dispatcher`.
        let kernel = {
            let mut k = kernel;
            k.approval_mode_resolver = Some(mode_resolver);
            k.approval_policy_matcher = policy_matcher;
            k
        };
        {
            let cfg = &kernel.config.user_adaptation;
            let hook = crate::hooks::UserAdaptationHook::new(
                cfg.enabled,
                Arc::clone(&kernel.scheduler),
                Arc::clone(&kernel.context_manager),
                Arc::clone(&kernel.user_pref_proposal_store),
                Arc::clone(&kernel.active_llms),
                Arc::clone(&kernel.audit),
                cfg.min_confidence,
                cfg.max_proposals_per_task,
                cfg.model.clone(),
            );
            kernel.hook_registry.register(hook).await;
        }

        // Discover plugin manifests from the plugins/ directories.
        // Resolve relative paths against the kernel's data_dir so discovery
        // works regardless of the process working directory.
        // Discovery is fast (TOML reads only, no code loaded).
        {
            let base = kernel.data_dir.parent().unwrap_or(&kernel.data_dir);
            let plugin_dirs = vec![base.join("plugins/core"), base.join("plugins/user")];
            let count = kernel.plugin_registry.discover(&plugin_dirs).await;
            if count > 0 {
                tracing::info!("Discovered {} plugins from manifests", count);
            }
        }

        // Restore bidirectional channels persisted from the previous run.
        kernel.restore_channels().await;
        kernel.refresh_connected_channels_snapshot().await;

        // Load connector manifests from the connectors/ directory.
        {
            let connectors_dir = kernel.data_dir.join("connectors");
            match agentos_connectors::load_connector_manifests(&connectors_dir) {
                Ok(manifests) => {
                    for manifest in manifests {
                        if let Err(e) = kernel.connector_registry.register(manifest).await {
                            tracing::warn!(error = %e, "Failed to register connector");
                        }
                    }
                }
                Err(e) => tracing::warn!(error = %e, "Failed to load connector manifests"),
            }
        }

        // Start the webhook batcher flush loop.
        {
            let batcher = Arc::clone(&kernel.webhook_batcher);
            let cancel = kernel.cancellation_token.clone();
            tokio::spawn(async move {
                batcher.run_flush_loop(cancel).await;
            });
        }

        // Start the container reaper (TTL enforcement) if Docker is available.
        if let Some(ref rt) = kernel.compute_runtime {
            let reaper = Arc::new(agentos_runtime::ContainerReaper::new(
                Arc::clone(rt),
                kernel.cancellation_token.clone(),
            ));
            reaper.start();
            tracing::info!("Container reaper started");
        }

        // Note: webhook wake-up loop is started after the kernel is wrapped in
        // Arc, via `start_webhook_wakeup()`. This is because the wake-up service
        // needs Arc<Kernel> to create tasks.

        // Auto-reactivate agents that were Online before this kernel session ended.
        // Runs after pubkey pre-registration (above) so signing is immediately available.
        let (reactivated, skipped) = kernel.auto_reactivate_agents().await;
        if reactivated > 0 || skipped > 0 {
            tracing::info!(reactivated, skipped, "Agent auto-reactivation complete");
        }

        // Emit KernelStarted audit event
        kernel.audit_log(agentos_audit::AuditEntry {
            timestamp: kernel.started_at,
            trace_id: TraceID::new(),
            event_type: agentos_audit::AuditEventType::KernelStarted,
            agent_id: None,
            task_id: None,
            tool_id: None,
            details: serde_json::json!({
                "bus_socket": kernel.config.bus.socket_path,
                "max_concurrent_tasks": kernel.config.kernel.max_concurrent_tasks
            }),
            severity: agentos_audit::AuditSeverity::Info,
            reversible: false,
            rollback_ref: None,
        });

        Ok(kernel)
    }

    /// Start the webhook wake-up loop. Must be called after the kernel is
    /// wrapped in `Arc`, since the wake-up service needs `Arc<Kernel>` to
    /// create tasks via the scheduler.
    /// Wire the kernel into the inbound chat bridge and spawn the InboundRouter.
    /// Must be called once after `Arc::new(kernel)`.
    pub fn wire_inbound_chat_bridge(self: &Arc<Self>) {
        self.inbound_chat_bridge.set_kernel(Arc::downgrade(self));
        let rx = self
            .pending_inbound_rx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(rx) = rx {
            tokio::spawn(
                crate::inbound_router::InboundRouter::new(
                    self.notification_router.clone(),
                    self.channel_registry.clone(),
                    self.scheduler.clone(),
                    self.inbound_chat_bridge.clone(),
                    self.audit.clone(),
                    self.escalation_manager.clone(),
                    self.pairing_manager.clone(),
                    self.vault.clone(),
                    self.attachment_sink.clone(),
                    self.config.transcription.clone(),
                    rx,
                )
                .run(),
            );
        }
    }

    /// Install a callback on the managed-process table so that abnormal
    /// process exits (`Failed` or `Killed`) emit a `ProcessCrashed` event on
    /// the kernel's event bus. Subscriptions to `events.system_health:observe`
    /// will then receive a triggered task per crash.
    ///
    /// Must be called once after `Arc::new(kernel)` so the callback can hold a
    /// weak reference to the kernel and upgrade it at fire time.
    pub async fn wire_process_crash_emission(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        self.process_table
            .set_crash_callback(Arc::new(move |info| {
                let Some(kernel) = weak.upgrade() else {
                    return;
                };
                // The callback fires from inside the process-table write lock
                // release path; do real work on a dedicated task so the caller
                // never awaits the dispatcher.
                tokio::spawn(async move {
                    let severity = match info.status {
                        crate::managed_process::ProcessStatus::Killed => {
                            agentos_types::EventSeverity::Critical
                        }
                        _ => agentos_types::EventSeverity::Warning,
                    };
                    let exited_at = info.exited_at.map(|t| t.to_rfc3339()).unwrap_or_default();
                    let payload = serde_json::json!({
                        "process_id": info.process_id,
                        "agent_id": info.agent_id.to_string(),
                        "task_id": info.task_id.to_string(),
                        "binary": info.binary,
                        "args": info.args,
                        "pid": info.pid,
                        "status": format!("{:?}", info.status),
                        "exit_code": info.exit_code,
                        "exited_at": exited_at,
                    });
                    kernel
                        .emit_event(
                            agentos_types::EventType::ProcessCrashed,
                            agentos_types::EventSource::TaskScheduler,
                            severity,
                            payload,
                            0,
                        )
                        .await;
                });
            }))
            .await;
        tracing::info!("Process crash emission wired");
    }

    pub async fn start_webhook_wakeup(self: &Arc<Self>) {
        let rx = self.webhook_batch_rx.lock().await.take();
        if let Some(rx) = rx {
            let wakeup = crate::webhook_wakeup::WebhookWakeUp::new(
                Arc::clone(self),
                rx,
                32768, // 32KB max context per batch
            );
            let cancel = self.cancellation_token.clone();
            tokio::spawn(async move {
                wakeup.run(cancel).await;
            });
            tracing::info!("Webhook wake-up loop started");
        }
    }

    /// Write a `KernelShutdown` audit entry exactly once per kernel lifecycle.
    ///
    /// Uses a `compare_exchange` on `shutdown_audited` so that if multiple exit
    /// paths converge (e.g., `KernelCommand::Shutdown` writes the entry and then
    /// the `cancelled()` arm in `run()` also fires), only the first caller writes.
    pub(crate) fn audit_shutdown(&self, reason: &str, severity: agentos_audit::AuditSeverity) {
        use std::sync::atomic::Ordering;
        if self
            .shutdown_audited
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.audit_log(agentos_audit::AuditEntry {
                timestamp: chrono::Utc::now(),
                trace_id: TraceID::new(),
                event_type: agentos_audit::AuditEventType::KernelShutdown,
                agent_id: None,
                task_id: None,
                tool_id: None,
                details: serde_json::json!({ "reason": reason }),
                severity,
                reversible: false,
                rollback_ref: None,
            });
        }
    }

    /// Broadcast a task status update to all active subscribers.
    ///
    /// Phase 1: the broadcast sender exists so Phase 2 (SSE) can subscribe without
    /// structural changes.  If there are no active receivers the message is silently dropped.
    pub(crate) fn push_status_update(&self, task_id: TaskID, state: TaskState, message: String) {
        let _ = self.status_update_sender.send(agentos_bus::StatusUpdate {
            task_id,
            state,
            message,
        });
    }

    /// Signal all kernel loops to stop gracefully.
    pub fn shutdown(&self) {
        self.audit_shutdown("api_shutdown", agentos_audit::AuditSeverity::Info);
        self.cancellation_token.cancel();
    }

    /// Number of agents currently tracked by the per-agent rate limiter.
    /// Exposed for integration testing; 0 means no rate-limit state is retained.
    pub async fn rate_limiter_tracked_count(&self) -> usize {
        self.per_agent_rate_limiter.lock().await.tracked_count()
    }

    /// Public API: Connect a new agent through the kernel command dispatch path.
    #[allow(clippy::too_many_arguments)]
    pub async fn api_connect_agent(
        &self,
        name: String,
        provider: LLMProvider,
        model: String,
        base_url: Option<String>,
        roles: Vec<String>,
        description: Option<String>,
        thinking_level: Option<ThinkingLevel>,
        system_prompt: Option<String>,
    ) -> Result<(), String> {
        self.api_connect_agent_with_options(
            name,
            provider,
            model,
            base_url,
            roles,
            description,
            thinking_level,
            system_prompt,
            false,
        )
        .await
    }

    /// Public API: Connect a new agent with explicit `skip_health_check`.
    ///
    /// Use `skip_health_check = true` for test harnesses or environments where
    /// the LLM endpoint is intentionally unreachable but a mock adapter will be
    /// substituted post-registration.
    #[allow(clippy::too_many_arguments)]
    pub async fn api_connect_agent_with_options(
        &self,
        name: String,
        provider: LLMProvider,
        model: String,
        base_url: Option<String>,
        roles: Vec<String>,
        description: Option<String>,
        thinking_level: Option<ThinkingLevel>,
        system_prompt: Option<String>,
        skip_health_check: bool,
    ) -> Result<(), String> {
        match self
            .cmd_connect_agent(
                name,
                provider,
                model,
                base_url,
                roles,
                description,
                thinking_level,
                system_prompt,
                false,
                vec![],
                false,
                skip_health_check,
            )
            .await
        {
            agentos_bus::KernelResponse::Success { .. } => Ok(()),
            agentos_bus::KernelResponse::Error { message } => Err(message),
            _ => Err("Unexpected kernel response".to_string()),
        }
    }

    /// Public API: Disconnect an agent by ID through the kernel command dispatch path.
    pub async fn api_disconnect_agent(&self, agent_id: AgentID) -> Result<(), String> {
        match self.cmd_disconnect_agent(agent_id).await {
            agentos_bus::KernelResponse::Success { .. } => Ok(()),
            agentos_bus::KernelResponse::Error { message } => Err(message),
            _ => Err("Unexpected kernel response".to_string()),
        }
    }

    /// Public API: Permanently remove an agent (profile + memory + inboxes + schedules).
    /// Returns the wipe-summary JSON on success.
    pub async fn api_remove_agent(
        &self,
        agent_id: AgentID,
    ) -> Result<Option<serde_json::Value>, String> {
        match self.cmd_remove_agent(agent_id).await {
            agentos_bus::KernelResponse::Success { data } => Ok(data),
            agentos_bus::KernelResponse::Error { message } => Err(message),
            _ => Err("Unexpected kernel response".to_string()),
        }
    }

    /// Public API: Install a tool from a manifest path through the kernel command dispatch path.
    pub async fn api_install_tool(&self, manifest_path: String) -> Result<(), String> {
        match self.cmd_install_tool(manifest_path).await {
            agentos_bus::KernelResponse::Success { .. } => Ok(()),
            agentos_bus::KernelResponse::Error { message } => Err(message),
            _ => Err("Unexpected kernel response".to_string()),
        }
    }

    /// Public API: Remove a tool by name through the kernel command dispatch path.
    pub async fn api_remove_tool(&self, tool_name: String) -> Result<(), String> {
        match self.cmd_remove_tool(tool_name).await {
            agentos_bus::KernelResponse::Success { .. } => Ok(()),
            agentos_bus::KernelResponse::Error { message } => Err(message),
            _ => Err("Unexpected kernel response".to_string()),
        }
    }

    /// Public API: Set a secret through the kernel command dispatch path.
    ///
    /// NOTE: `value` is a plain `String`. The caller should zero any `ZeroizingString`
    /// source before this frame is dropped. A future improvement is to accept
    /// `ZeroizingString` here and propagate it through `cmd_set_secret`.
    pub async fn api_set_secret(
        &self,
        name: String,
        value: String,
        scope: SecretScope,
    ) -> Result<(), String> {
        match self.cmd_set_secret(name, value, scope, None).await {
            agentos_bus::KernelResponse::Success { .. } => Ok(()),
            agentos_bus::KernelResponse::Error { message } => Err(message),
            _ => Err("Unexpected kernel response".to_string()),
        }
    }

    /// Public API: Revoke a secret through the kernel command dispatch path.
    pub async fn api_revoke_secret(&self, name: String) -> Result<(), String> {
        match self.cmd_revoke_secret(name).await {
            agentos_bus::KernelResponse::Success { .. } => Ok(()),
            agentos_bus::KernelResponse::Error { message } => Err(message),
            _ => Err("Unexpected kernel response".to_string()),
        }
    }

    /// Public API: Grant a permission to an agent through the kernel command dispatch path.
    /// Permission format: `resource:rwx` (e.g. `fs.user_data:rw`, `network.outbound:x`).
    pub async fn api_grant_permission(
        &self,
        agent_name: String,
        permission: String,
    ) -> Result<(), String> {
        match self.cmd_grant_permission(agent_name, permission).await {
            agentos_bus::KernelResponse::Success { .. } => Ok(()),
            agentos_bus::KernelResponse::Error { message } => Err(message),
            _ => Err("Unexpected kernel response".to_string()),
        }
    }

    /// Public API: Revoke a permission from an agent through the kernel command dispatch path.
    /// Permission format: `resource:rwx` (e.g. `fs.user_data:rw`, `network.outbound:x`).
    pub async fn api_revoke_permission(
        &self,
        agent_name: String,
        permission: String,
    ) -> Result<(), String> {
        match self.cmd_revoke_permission(agent_name, permission).await {
            agentos_bus::KernelResponse::Success { .. } => Ok(()),
            agentos_bus::KernelResponse::Error { message } => Err(message),
            _ => Err("Unexpected kernel response".to_string()),
        }
    }

    /// Public API: Update mutable agent profile settings.
    pub async fn api_update_agent_settings(
        &self,
        agent_name: String,
        description: String,
        default_thinking_level: ThinkingLevel,
        system_prompt: Option<String>,
    ) -> Result<(), String> {
        let mut registry = self.agent_registry.write().await;
        registry
            .update_profile_settings(
                &agent_name,
                description,
                default_thinking_level,
                system_prompt,
            )
            .map(|_| ())
    }

    /// Execute a pipeline with full security enforcement (agent resolution, permission
    /// enforcement, injection scanning, audit logging).
    ///
    /// Public entry point for non-kernel callers such as the web server. Internally
    /// delegates to `cmd_run_pipeline` so all security checks are applied identically
    /// to CLI-initiated runs.
    pub async fn run_pipeline(
        &self,
        name: String,
        input: String,
        detach: bool,
        agent_name: Option<String>,
    ) -> Result<serde_json::Value, String> {
        match self.cmd_run_pipeline(name, input, detach, agent_name).await {
            agentos_bus::KernelResponse::Success { data } => {
                Ok(data.unwrap_or(serde_json::Value::Null))
            }
            agentos_bus::KernelResponse::Error { message } => Err(message),
            _ => Err("Unexpected kernel response".to_string()),
        }
    }
}

fn resolve_state_db_path(configured: &str, data_dir: &Path) -> PathBuf {
    let configured_path = PathBuf::from(configured);
    if configured_path.is_absolute() {
        return configured_path;
    }
    // All relative paths are resolved against data_dir so the result is
    // deterministic regardless of the process working directory.
    data_dir.join(configured_path)
}

/// Run pre-flight system health checks before initializing any subsystem.
/// Returns `Err` with a descriptive message if any check fails so that `boot()`
/// can surface a clear diagnostic instead of crashing deep in subsystem init.
fn preflight_checks(config: &KernelConfig) -> Result<(), anyhow::Error> {
    let data_dir = std::path::Path::new(&config.tools.data_dir);

    // 1. Disk space check on the data directory partition
    if config.preflight.min_free_disk_mb > 0 {
        let free_mb = get_free_disk_mb(data_dir)?;
        if free_mb < config.preflight.min_free_disk_mb {
            return Err(anyhow::anyhow!(
                "Pre-flight check failed: insufficient disk space on {}. \
                 Free: {} MB, required: {} MB. \
                 Free up disk space or set preflight.min_free_disk_mb = 0 to disable this check.",
                data_dir.display(),
                free_mb,
                config.preflight.min_free_disk_mb,
            ));
        }
        tracing::info!(
            free_mb,
            min_required_mb = config.preflight.min_free_disk_mb,
            "Pre-flight: disk space OK"
        );
    }

    // 2. Writability checks for database parent directories
    if config.preflight.check_db_writable {
        let state_db_path = resolve_state_db_path(&config.kernel.state_db_path, data_dir);
        let mut writable_paths = vec![
            ("audit", PathBuf::from(&config.audit.log_path)),
            ("vault", PathBuf::from(&config.secrets.vault_path)),
            ("state", state_db_path),
            // Bus socket runtime dir (e.g. /run/agentos on systemd); the loop
            // probes the socket path's parent directory.
            ("bus", PathBuf::from(&config.bus.socket_path)),
        ];
        // Log directory (Phase 02 writes JSON logs here). Skip when file logging
        // is disabled (log_dir = ""), so we never probe the process CWD. The
        // sentinel child makes the loop's `.parent()` resolve to the log dir.
        // The dir is normally created by the binary's logging init before boot;
        // this probe is defense-in-depth and no-ops if it does not exist yet.
        if !config.logging.log_dir.is_empty() {
            writable_paths.push((
                "logs",
                PathBuf::from(&config.logging.log_dir).join(".agentos_logdir_probe"),
            ));
        }

        for (label, path) in writable_paths {
            if let Some(parent) = path.parent() {
                if parent.exists() {
                    // Use O_CREAT|O_EXCL (create_new) to avoid following symlinks.
                    // Include a nanosecond timestamp to prevent false EEXIST from a stale
                    // file left by a crashed predecessor with the same recycled PID.
                    let test_file = parent.join(format!(
                        ".agentos_preflight_{}_{}.tmp",
                        std::process::id(),
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_nanos()
                    ));
                    match std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&test_file)
                    {
                        Ok(f) => {
                            drop(f);
                            let _ = std::fs::remove_file(&test_file);
                            tracing::info!(
                                path = %parent.display(),
                                "Pre-flight: {} directory writable",
                                label
                            );
                        }
                        Err(e) => {
                            return Err(anyhow::anyhow!(
                                "Pre-flight check failed: {} directory {} is not writable: {}",
                                label,
                                parent.display(),
                                e,
                            ));
                        }
                    }
                }
                // Parent does not exist yet -- boot() will create it, skip the check.
            }
        }
    }

    Ok(())
}

/// Return free disk space in MB for the partition containing `path`.
/// Walks up to the first existing ancestor when `path` does not yet exist.
/// Uses `statvfs(2)` directly — no external binaries required (works in distroless containers).
/// On non-Unix platforms returns `u64::MAX` so the threshold check is always skipped.
fn get_free_disk_mb(path: &std::path::Path) -> Result<u64, anyhow::Error> {
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::mem::MaybeUninit;

        // Walk up to the first existing ancestor.
        let mut check = path.to_path_buf();
        loop {
            if check.exists() {
                break;
            }
            match check.parent().map(|p| p.to_path_buf()) {
                Some(parent) if parent != check => check = parent,
                _ => {
                    check = std::path::PathBuf::from("/");
                    break;
                }
            }
        }

        // Use OsStrExt::as_bytes() to preserve exact filesystem path bytes without
        // the lossy UTF-8 replacement that to_string_lossy() would introduce.
        #[cfg(unix)]
        use std::os::unix::ffi::OsStrExt;
        let c_path = CString::new(check.as_os_str().as_bytes())
            .map_err(|e| anyhow::anyhow!("Invalid path for statvfs: {}", e))?;
        let mut stat = MaybeUninit::<libc::statvfs>::uninit();
        let ret = unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) };
        if ret != 0 {
            return Err(anyhow::anyhow!(
                "statvfs({}) failed: {}",
                check.display(),
                std::io::Error::last_os_error()
            ));
        }
        let stat = unsafe { stat.assume_init() };
        // f_bavail: free blocks for unprivileged processes; f_frsize: fundamental block size.
        // Explicit u64 casts are defensive: on 32-bit platforms fsblkcnt_t/c_ulong are u32
        // and multiplying two u32 values before widening would overflow.
        #[allow(clippy::unnecessary_cast)]
        let free_bytes = (stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64);
        Ok(free_bytes / (1024 * 1024))
    }

    #[cfg(not(unix))]
    {
        tracing::warn!("Disk space pre-flight check not supported on this platform; skipping");
        Ok(u64::MAX)
    }
}

#[cfg(test)]
mod meta_tool_streak_tests {
    use super::iteration_is_all_meta;

    #[test]
    fn empty_batch_is_not_meta() {
        assert!(!iteration_is_all_meta(&[]));
    }

    #[test]
    fn pure_meta_batch_is_meta() {
        assert!(iteration_is_all_meta(&[
            "search-tools".into(),
            "describe-tool".into(),
        ]));
    }

    #[test]
    fn any_real_tool_breaks_meta() {
        // The exact case from the 2026-05-08 logs: alternating
        // search-tools/describe-tool with a single gmail_send
        // interleaved must reset the streak.
        assert!(!iteration_is_all_meta(&[
            "search-tools".into(),
            "gmail_send".into(),
        ]));
        assert!(!iteration_is_all_meta(&["file-reader".into()]));
    }

    #[test]
    fn agent_manual_alone_is_meta() {
        assert!(iteration_is_all_meta(&["agent-manual".into()]));
    }

    #[test]
    fn iteration_is_all_meta_recognises_canonical_list() {
        // Sanity: the streak guard should match every entry in the
        // canonical agentos-tools list, so the dedup cache and the
        // discovery-loop guard can never drift out of sync.
        for name in agentos_tools::META_TOOL_NAMES {
            assert!(
                iteration_is_all_meta(&[(*name).to_string()]),
                "missing canonical meta tool: {name}"
            );
        }
    }
}

#[cfg(test)]
mod preflight_tests {
    use super::*;
    use crate::config::*;
    use tempfile::tempdir;

    fn make_test_config(
        data_dir: &str,
        audit_log: &str,
        vault_path: &str,
        min_free_mb: u64,
        check_writable: bool,
    ) -> KernelConfig {
        KernelConfig {
            kernel: KernelSettings {
                max_concurrent_tasks: 1,
                default_task_timeout_secs: 30,
                context_window_max_entries: 10,
                context_window_token_budget: 0,
                state_db_path: "data/kernel_state.db".to_string(),
                task_limits: Default::default(),
                tool_calls: Default::default(),
                tool_execution: Default::default(),
                autonomous_mode: Default::default(),
                health_port: 9091,
                per_agent_rate_limit: 0,
                events: Default::default(),
                sandbox_policy: Default::default(),
                max_concurrent_sandbox_children: 4,
                context_compaction: Default::default(),
            },
            secrets: SecretsSettings {
                vault_path: vault_path.to_string(),
            },
            audit: AuditSettings {
                log_path: audit_log.to_string(),
                max_audit_entries: 0,
                verify_last_n_entries: 0,
            },
            tools: ToolsSettings {
                core_tools_dir: data_dir.to_string(),
                user_tools_dir: data_dir.to_string(),
                data_dir: data_dir.to_string(),
                crl_path: None,
                workspace: crate::config::WorkspaceConfig::default(),
                host_package: crate::config::HostPackageSettings::default(),
                discovery: Default::default(),
            },
            bus: BusSettings {
                socket_path: "/tmp/test.sock".to_string(),
                tls: None,
            },
            ollama: OllamaSettings {
                host: "http://localhost:11434".to_string(),
                default_model: "test".to_string(),
                request_timeout_secs: 300,
            },
            llm: LlmSettings::default(),
            memory: MemorySettings::default(),
            routing: RoutingConfig::default(),
            context_budget: agentos_types::TokenBudget::default(),
            context: ContextConfig::default(),
            health_monitor: HealthMonitorConfig::default(),
            preflight: PreflightConfig {
                min_free_disk_mb: min_free_mb,
                check_db_writable: check_writable,
            },
            logging: Default::default(),
            notifications: Default::default(),
            mcp: Default::default(),
            registry: Default::default(),
            scratchpad: Default::default(),
            skills: Default::default(),
            otel: OtelConfig::default(),
            approval: Default::default(),
            api: Default::default(),
            web: Default::default(),
            chat: Default::default(),
            user_adaptation: Default::default(),
            env: Default::default(),
            gateway: Default::default(),
            scheduler: Default::default(),
            transcription: Default::default(),
            user_profile: Default::default(),
            personalization: Default::default(),
        }
    }

    #[test]
    fn preflight_disk_check_disabled_passes() {
        // min_free_disk_mb = 0 should always succeed regardless of actual disk state.
        let config = make_test_config("/tmp", "/tmp/audit.db", "/tmp/vault.db", 0, false);
        assert!(preflight_checks(&config).is_ok());
    }

    #[test]
    fn preflight_extremely_high_threshold_fails() {
        // A threshold of u64::MAX should always fail (no disk has that much free space).
        let config = make_test_config("/tmp", "/tmp/audit.db", "/tmp/vault.db", u64::MAX, false);
        let result = preflight_checks(&config);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("insufficient disk space"), "Error: {}", msg);
    }

    #[test]
    #[cfg(unix)]
    fn preflight_get_free_disk_mb_on_root() {
        let free = get_free_disk_mb(std::path::Path::new("/")).unwrap();
        assert!(
            free > 0,
            "Root partition should have some free space; got {} MB",
            free
        );
    }

    #[test]
    #[cfg(unix)]
    fn preflight_get_free_disk_mb_nonexistent_path_falls_back() {
        let free = get_free_disk_mb(std::path::Path::new(
            "/nonexistent_agentos_preflight_path/deep/dir",
        ))
        .unwrap();
        assert!(
            free > 0,
            "Should fall back to / and return > 0 MB; got {}",
            free
        );
    }

    #[test]
    fn preflight_check_db_writable_nonexistent_parent_passes() {
        // Directories that don't exist yet are skipped — boot() will create them.
        let config = make_test_config(
            "/tmp",
            "/nonexistent_agentos_dir/audit.db",
            "/nonexistent_agentos_dir/vault.db",
            0,
            true,
        );
        assert!(preflight_checks(&config).is_ok());
    }

    #[test]
    #[cfg(unix)]
    fn preflight_check_db_writable_readonly_dir_fails() {
        use std::os::unix::fs::PermissionsExt;

        // Skip if running as root (root bypasses permission checks).
        let is_root = std::process::Command::new("id")
            .arg("-u")
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "0");
        if is_root {
            return;
        }

        let dir = tempdir().unwrap();
        let readonly_dir = dir.path().join("readonly");
        std::fs::create_dir(&readonly_dir).unwrap();
        std::fs::set_permissions(&readonly_dir, std::fs::Permissions::from_mode(0o444)).unwrap();

        let audit_path = readonly_dir.join("audit.db").to_string_lossy().into_owned();
        let vault_path = readonly_dir.join("vault.db").to_string_lossy().into_owned();
        let config = make_test_config(
            dir.path().to_str().unwrap(),
            &audit_path,
            &vault_path,
            0,
            true,
        );

        let result = preflight_checks(&config);
        // Restore permissions so tempdir cleanup succeeds.
        let _ = std::fs::set_permissions(&readonly_dir, std::fs::Permissions::from_mode(0o755));

        assert!(
            result.is_err(),
            "Expected writability check to fail for read-only directory"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("not writable"),
            "Error should mention 'not writable': {}",
            msg
        );
    }

    #[test]
    #[cfg(unix)]
    fn preflight_log_dir_not_writable_fails() {
        use std::os::unix::fs::PermissionsExt;

        // Skip if running as root (root bypasses permission checks).
        let is_root = std::process::Command::new("id")
            .arg("-u")
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "0");
        if is_root {
            return;
        }

        let dir = tempdir().unwrap();
        // audit + vault live in a writable dir so the failure is specifically
        // the new log-directory probe, not an earlier one.
        let audit_path = dir.path().join("audit.db").to_string_lossy().into_owned();
        let vault_path = dir.path().join("vault.db").to_string_lossy().into_owned();
        let readonly_logs = dir.path().join("logs_ro");
        std::fs::create_dir(&readonly_logs).unwrap();
        std::fs::set_permissions(&readonly_logs, std::fs::Permissions::from_mode(0o444)).unwrap();

        let mut config = make_test_config(
            dir.path().to_str().unwrap(),
            &audit_path,
            &vault_path,
            0,
            true,
        );
        config.logging.log_dir = readonly_logs.to_string_lossy().into_owned();

        let result = preflight_checks(&config);
        // Restore permissions so tempdir cleanup succeeds.
        let _ = std::fs::set_permissions(&readonly_logs, std::fs::Permissions::from_mode(0o755));

        let msg = match result {
            Ok(()) => panic!("Expected pre-flight to fail on a read-only log directory"),
            Err(e) => e.to_string(),
        };
        assert!(
            msg.contains("logs") && msg.contains("not writable"),
            "Error should mention the log dir is not writable: {msg}"
        );
    }

    #[test]
    fn preflight_all_dirs_writable_passes() {
        let dir = tempdir().unwrap();
        let audit_path = dir.path().join("audit.db").to_string_lossy().into_owned();
        let vault_path = dir.path().join("vault.db").to_string_lossy().into_owned();
        let mut config = make_test_config(
            dir.path().to_str().unwrap(),
            &audit_path,
            &vault_path,
            0,
            true,
        );
        // Point the new logs + bus probes at the writable tempdir.
        config.logging.log_dir = dir.path().to_string_lossy().into_owned();
        config.bus.socket_path = dir
            .path()
            .join("agentos.sock")
            .to_string_lossy()
            .into_owned();

        assert!(
            preflight_checks(&config).is_ok(),
            "Pre-flight should pass when every probed directory is writable"
        );
    }
}

#[cfg(test)]
mod vault_bootstrap_tests {
    use super::*;
    use crate::config::*;
    use agentos_audit::AuditLog;
    use tempfile::tempdir;

    fn make_test_config(root: &Path) -> KernelConfig {
        KernelConfig {
            kernel: KernelSettings {
                max_concurrent_tasks: 1,
                default_task_timeout_secs: 30,
                context_window_max_entries: 10,
                context_window_token_budget: 0,
                state_db_path: root
                    .join("data/kernel_state.db")
                    .to_string_lossy()
                    .into_owned(),
                task_limits: Default::default(),
                tool_calls: Default::default(),
                tool_execution: Default::default(),
                autonomous_mode: Default::default(),
                health_port: 0,
                per_agent_rate_limit: 0,
                events: Default::default(),
                sandbox_policy: Default::default(),
                max_concurrent_sandbox_children: 4,
                context_compaction: Default::default(),
            },
            secrets: SecretsSettings {
                vault_path: root.join("vault/vault.db").to_string_lossy().into_owned(),
            },
            audit: AuditSettings {
                log_path: root.join("data/audit.db").to_string_lossy().into_owned(),
                max_audit_entries: 0,
                verify_last_n_entries: 0,
            },
            tools: ToolsSettings {
                core_tools_dir: root.join("tools/core").to_string_lossy().into_owned(),
                user_tools_dir: root.join("tools/user").to_string_lossy().into_owned(),
                data_dir: root.join("data").to_string_lossy().into_owned(),
                crl_path: None,
                workspace: WorkspaceConfig::default(),
                host_package: crate::config::HostPackageSettings::default(),
                discovery: Default::default(),
            },
            bus: BusSettings {
                socket_path: root
                    .join("data/agentos.sock")
                    .to_string_lossy()
                    .into_owned(),
                tls: None,
            },
            ollama: OllamaSettings {
                host: "http://localhost:11434".to_string(),
                default_model: "test".to_string(),
                request_timeout_secs: 300,
            },
            llm: LlmSettings::default(),
            memory: MemorySettings::default(),
            routing: RoutingConfig::default(),
            context_budget: agentos_types::TokenBudget::default(),
            context: ContextConfig::default(),
            health_monitor: HealthMonitorConfig::default(),
            preflight: PreflightConfig::default(),
            logging: Default::default(),
            notifications: Default::default(),
            mcp: Default::default(),
            registry: Default::default(),
            scratchpad: Default::default(),
            skills: Default::default(),
            otel: OtelConfig::default(),
            approval: Default::default(),
            api: Default::default(),
            web: Default::default(),
            chat: Default::default(),
            user_adaptation: Default::default(),
            env: Default::default(),
            gateway: Default::default(),
            scheduler: Default::default(),
            transcription: Default::default(),
            user_profile: Default::default(),
            personalization: Default::default(),
        }
    }

    #[test]
    fn resolve_boot_vault_passphrase_generates_and_reuses_managed_file() {
        let dir = tempdir().unwrap();
        let config = make_test_config(dir.path());
        unsafe {
            std::env::set_var("AGENTOS_AUTO_INIT_VAULT", "true");
        }

        let first = resolve_boot_vault_passphrase(&config).unwrap().unwrap();
        let passphrase_path = vault_passphrase_path(Path::new(&config.secrets.vault_path));
        assert!(passphrase_path.exists());
        let persisted = std::fs::read_to_string(&passphrase_path).unwrap();
        assert_eq!(persisted, first.as_str());

        std::fs::create_dir_all(Path::new(&config.audit.log_path).parent().unwrap()).unwrap();
        std::fs::create_dir_all(Path::new(&config.secrets.vault_path).parent().unwrap()).unwrap();
        let audit = AuditLog::open(Path::new(&config.audit.log_path)).unwrap();
        SecretsVault::initialize(
            Path::new(&config.secrets.vault_path),
            &ZeroizingString::new(first.as_str().to_string()),
            std::sync::Arc::new(audit),
        )
        .unwrap();

        let second = resolve_boot_vault_passphrase(&config).unwrap().unwrap();
        assert_eq!(first.as_str(), second.as_str());
        unsafe {
            std::env::remove_var("AGENTOS_AUTO_INIT_VAULT");
        }
    }

    #[test]
    fn resolve_boot_vault_passphrase_returns_none_without_auto_init_or_env() {
        let dir = tempdir().unwrap();
        let config = make_test_config(dir.path());

        unsafe {
            std::env::remove_var("AGENTOS_AUTO_INIT_VAULT");
        }
        assert!(resolve_boot_vault_passphrase(&config).unwrap().is_none());
    }

    #[test]
    fn resolve_boot_vault_passphrase_errors_when_existing_vault_has_no_managed_passphrase() {
        let dir = tempdir().unwrap();
        let config = make_test_config(dir.path());

        std::fs::create_dir_all(Path::new(&config.audit.log_path).parent().unwrap()).unwrap();
        std::fs::create_dir_all(Path::new(&config.secrets.vault_path).parent().unwrap()).unwrap();
        let audit = AuditLog::open(Path::new(&config.audit.log_path)).unwrap();
        SecretsVault::initialize(
            Path::new(&config.secrets.vault_path),
            &ZeroizingString::new("manual-passphrase".to_string()),
            std::sync::Arc::new(audit),
        )
        .unwrap();

        let err = match resolve_boot_vault_passphrase(&config) {
            Ok(_) => panic!("expected managed-passphrase lookup to fail"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("Vault already exists"));
    }
}

#[cfg(test)]
mod hal_device_access_gate_tests {
    use super::*;
    use agentos_audit::AuditLog;
    use tempfile::tempdir;

    fn make_gate() -> (
        KernelDeviceAccessGate,
        Arc<HardwareRegistry>,
        Arc<crate::escalation::EscalationManager>,
    ) {
        let dir = tempdir().expect("temp dir");
        let audit_path = dir.path().join("audit.db");
        let audit = Arc::new(AuditLog::open(&audit_path).expect("audit log should open"));
        let registry = Arc::new(HardwareRegistry::new());
        let escalation_manager = Arc::new(crate::escalation::EscalationManager::new());
        std::mem::forget(dir);

        (
            KernelDeviceAccessGate::new(registry.clone(), escalation_manager.clone(), audit),
            registry,
            escalation_manager,
        )
    }

    #[tokio::test]
    async fn pending_device_access_creates_escalation() {
        let (gate, registry, escalation_manager) = make_gate();
        registry.register_pending_device("gpu:0", "gpu");
        let agent_id = AgentID::new();
        let task_id = TaskID::new();

        let err = gate
            .check(&agent_id, &task_id, "gpu:0", "gpu", HalOperation::Read)
            .await
            .expect_err("pending device should require approval");

        assert!(matches!(err, AgentOSError::DeviceAccessPending { .. }));
        assert_eq!(escalation_manager.list_pending().await.len(), 1);
    }

    #[tokio::test]
    async fn approved_device_access_succeeds_and_quarantined_fails() {
        let (gate, registry, _) = make_gate();
        let agent_id = AgentID::new();
        let task_id = TaskID::new();
        registry.register_pending_device("sensor:thermal_zone0", "thermal-sensor");
        registry
            .approve_for_agent("sensor:thermal_zone0", agent_id)
            .expect("approval should succeed");

        gate.check(
            &agent_id,
            &task_id,
            "sensor:thermal_zone0",
            "thermal-sensor",
            HalOperation::Read,
        )
        .await
        .expect("approved device should pass");

        registry
            .set_device_status("sensor:thermal_zone0", DeviceStatus::Quarantined)
            .expect("quarantine should succeed");
        let err = gate
            .check(
                &agent_id,
                &task_id,
                "sensor:thermal_zone0",
                "thermal-sensor",
                HalOperation::Read,
            )
            .await
            .expect_err("quarantined device should fail");

        assert!(matches!(err, AgentOSError::DeviceQuarantined(_)));
    }

    #[tokio::test]
    async fn agent_specific_deny_blocks_only_the_denied_agent() {
        let (gate, registry, _) = make_gate();
        let approved_agent = AgentID::new();
        let denied_agent = AgentID::new();
        let task_id = TaskID::new();
        registry.register_pending_device("gpu:0", "gpu");
        registry
            .approve_for_agent("gpu:0", approved_agent)
            .expect("approval should succeed");
        registry
            .deny_for_agent("gpu:0", denied_agent)
            .expect("agent-specific deny should succeed");

        gate.check(
            &approved_agent,
            &task_id,
            "gpu:0",
            "gpu",
            HalOperation::Read,
        )
        .await
        .expect("approved agent should still have access");

        let err = gate
            .check(&denied_agent, &task_id, "gpu:0", "gpu", HalOperation::Read)
            .await
            .expect_err("denied agent should be blocked");

        assert!(matches!(err, AgentOSError::PermissionDenied { .. }));
    }
}
