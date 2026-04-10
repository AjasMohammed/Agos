/// A2A (Agent-to-Agent) Protocol implementation for AgentOS.
///
/// Google's open-source A2A standard enables agent interoperability across
/// frameworks (LangGraph, PydanticAI, Google ADK, CrewAI). It complements MCP:
///   - MCP  = agent ↔ tool communication
///   - A2A  = agent ↔ agent communication (task delegation, discovery)
///
/// # Endpoints exposed
///
/// ```text
/// GET  /.well-known/agent.json   — Agent Card (capabilities, auth, endpoint)
/// POST /a2a/tasks                — Submit a task delegation
/// GET  /a2a/tasks/{id}           — Poll task status
/// POST /a2a/tasks/{id}/cancel    — Cancel a running task
/// ```
///
/// # Security
///
/// All incoming task delegations are validated against a CapabilityToken before
/// being dispatched to the kernel. All A2A interactions are logged to the audit
/// trail (`A2ATaskReceived`, `A2ATaskCompleted`, `A2ATaskRejected`).
pub mod agent_card;
pub mod client;
pub mod server;
pub mod task;

pub use agent_card::{AgentCapability, AgentCard, AuthRequirement};
pub use client::A2AClient;
pub use server::build_a2a_router;
pub use task::{A2ATask, A2ATaskStatus};
