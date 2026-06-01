pub mod agent;
pub mod agent_inbox;
pub mod agent_self;
pub mod capability;
pub mod channel;
pub mod chat;
pub mod context;
pub mod delivery;
pub mod error;
pub mod event;
pub mod fallback;
pub mod hook;
pub mod ids;
pub mod intent;
pub mod notification;
pub mod path;
pub mod plugin;
pub mod profile;
pub mod registry_query;
pub mod role;
pub mod schedule;
pub mod secret;
pub mod task;
pub mod tool;
pub use path::{reject_traversal, PathError};
pub use schedule::*;
pub mod agent_message;
pub mod skill;

// Re-export commonly used types at crate root
pub use agent::{AgentProfile, AgentStatus, LLMProvider};
pub use agent_inbox::{AgentInboxEntry, AgentInboxKind, AgentMessageEntry};
pub use agent_message::{AgentMessage, MessageContent, MessageTarget};
pub use agent_self::{AgentSelfView, BudgetSummary, SubscriptionSummary};
pub use capability::{
    CapabilityToken, IntentTypeFlag, PermissionEntry, PermissionOp, PermissionSet,
};
pub use channel::{ChannelKind, RegisteredChannel};
pub use context::{
    ContentPart, ContextCategory, ContextEntry, ContextMetadata, ContextPartition, ContextRole,
    ContextSlice, ContextWindow, HandoffMode, ImageSource, OverflowStrategy, SubAgentResult,
    TokenBudget,
};
pub use error::AgentOSError;
pub use event::{
    EventCategory, EventMessage, EventSeverity, EventSource, EventSubscription, EventType,
    EventTypeFilter, RawUsbDeviceOpened, RawUsbTransfer, RealtimeEvent, SubscriptionPriority,
    ThrottlePolicy,
};
pub use fallback::{apply_transforms, TransformOp};
pub use hook::{HookEvent, HookResult};
pub use ids::NotificationID;
pub use ids::*;
pub use intent::{
    ActionRiskLevel, HardwareResource, IntentCoherenceResult, IntentMessage, IntentResult,
    IntentResultStatus, IntentTarget, IntentType, SemanticPayload, SubscribePayload,
    SubscriptionDuration, UnsubscribePayload,
};
pub use notification::{
    AttachmentKind, DeliveryChannel, DeliveryStatus, InlineAttachment, InteractionRequest,
    MessageAttachment, NotificationPriority, NotificationSource, TaskOutcome, UserMessage,
    UserMessageKind, UserResponse,
};
pub use plugin::{ChannelDeclaration, PluginManifest};
pub use profile::{ProfileCategory, ProfileEntry, ProfileEntryStatus, ProfilePatch, ProfileSource};
pub use registry_query::{
    AgentRegistryQuery, AgentRegistrySnapshot, AgentSummary, CapabilityDescriptorSummary,
    CapabilityDispatchRequest, CapabilityDispatcher, CapabilityRegistryQuery,
    CapabilityRegistrySnapshot, EscalationQuery, EscalationSnapshot, EscalationSummary,
    StorageZoneQuery, TaskIntrospectionSummary, TaskQuery, TaskSnapshot, ZoneAccessLevel,
};
pub use role::Role;
pub use secret::{SecretEntry, SecretMetadata, SecretOwner, SecretScope};
pub use skill::SkillManifest;
pub use task::TriggerSource;
pub use task::{
    AgentBudget, AgentTask, BudgetAction, ComplexityLevel, CostSnapshot, ModelDowngradeTier,
    PreemptionLevel, TaskReasoningHints, TaskState, TaskSummary, ThinkingLevel, ToolCallRecord,
};
pub use tool::{
    ExecutorType, FallbackRule, RegisteredTool, RiskClass, ToolExecutor, ToolManifest, ToolSandbox,
    ToolStatus, TrustTier, UsageHints,
};
pub mod approval;
pub use approval::{ApprovalDecision, ApprovalMode};
pub mod workspace_grant;
pub use workspace_grant::{WorkspaceGrant, WorkspaceGrantMode};
pub mod task_trace;
pub use task_trace::{
    IterationTrace, PermissionCheckTrace, TaskTrace, TaskTraceSummary, ToolCallTrace,
};
pub mod team;
pub use team::{TeamConfig, TeamMember, TeamRole};
pub mod webhook;
pub use chat::ChatStreamFrame;
pub use webhook::{
    SignatureAlgorithm, WebhookEndpoint, WebhookEndpointMeta, WebhookEvent, WebhookProvider,
};
