//! KMC bridge tools — thin wrappers that route agent tool calls to
//! kernel-mediated capability providers.
//!
//! Each tool fills in the `domain` and `action` fields, extracts the tool-specific
//! payload, and delegates to the `CapabilityDispatcher` on the `ToolExecutionContext`.

use crate::traits::{AgentTool, ToolExecutionContext};
use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;

/// Macro to define a KMC bridge tool with minimal boilerplate.
macro_rules! kmc_tool {
    (
        name: $name:expr,
        domain: $domain:expr,
        action: $action:expr,
        permissions: [ $( ($res:expr, $op:expr) ),* $(,)? ],
        struct_name: $struct_name:ident
    ) => {
        pub struct $struct_name;

        impl $struct_name {
            pub fn new() -> Self { Self }
        }

        impl Default for $struct_name {
            fn default() -> Self { Self::new() }
        }

        #[async_trait]
        impl AgentTool for $struct_name {
            fn name(&self) -> &str { $name }

            fn required_permissions(&self) -> Vec<(String, PermissionOp)> {
                vec![ $( ($res.to_string(), $op) ),* ]
            }

            async fn execute(
                &self,
                payload: serde_json::Value,
                context: ToolExecutionContext,
            ) -> Result<serde_json::Value, AgentOSError> {
                let dispatcher = context.capability_dispatcher.as_ref().ok_or_else(|| {
                    AgentOSError::KernelError {
                        reason: "capability dispatcher not available (kernel context required)".into(),
                    }
                })?;

                dispatcher.dispatch(agentos_types::CapabilityDispatchRequest {
                    domain: $domain.to_string(),
                    action: $action.to_string(),
                    params: payload,
                    agent_id: context.agent_id,
                    task_id: context.task_id,
                    trace_id: context.trace_id,
                    data_dir: context.data_dir.clone(),
                    permissions: context.permissions.clone(),
                    workspace_paths: context.workspace_paths.clone(),
                }).await
            }
        }
    };
}

// ---------------------------------------------------------------------------
// Environment tools (env.*)
// ---------------------------------------------------------------------------

kmc_tool! {
    name: "env-create",
    domain: "env",
    action: "create",
    permissions: [("env.create", PermissionOp::Execute)],
    struct_name: EnvCreateTool
}

kmc_tool! {
    name: "env-install",
    domain: "env",
    action: "install",
    permissions: [("env.install", PermissionOp::Execute), ("net.outbound", PermissionOp::Execute)],
    struct_name: EnvInstallTool
}

kmc_tool! {
    name: "env-list",
    domain: "env",
    action: "list",
    permissions: [("env.list", PermissionOp::Read)],
    struct_name: EnvListTool
}

kmc_tool! {
    name: "env-destroy",
    domain: "env",
    action: "destroy",
    permissions: [("env.destroy", PermissionOp::Execute)],
    struct_name: EnvDestroyTool
}

// ---------------------------------------------------------------------------
// Storage zone tools (storage.*)
// ---------------------------------------------------------------------------

kmc_tool! {
    name: "storage-zone-create",
    domain: "storage",
    action: "zone.create",
    permissions: [("storage.zone.create", PermissionOp::Execute)],
    struct_name: StorageZoneCreateTool
}

kmc_tool! {
    name: "storage-zone-list",
    domain: "storage",
    action: "zone.list",
    permissions: [("storage.zone.list", PermissionOp::Read)],
    struct_name: StorageZoneListTool
}

kmc_tool! {
    name: "storage-zone-revoke",
    domain: "storage",
    action: "zone.revoke",
    permissions: [("storage.zone.revoke", PermissionOp::Execute)],
    struct_name: StorageZoneRevokeTool
}

// ---------------------------------------------------------------------------
// Process tools (proc.*)
// ---------------------------------------------------------------------------

kmc_tool! {
    name: "proc-spawn",
    domain: "proc",
    action: "spawn",
    permissions: [("proc.spawn", PermissionOp::Execute)],
    struct_name: ProcSpawnTool
}

kmc_tool! {
    name: "proc-signal",
    domain: "proc",
    action: "signal",
    permissions: [("proc.signal", PermissionOp::Execute)],
    struct_name: ProcSignalTool
}

kmc_tool! {
    name: "proc-output",
    domain: "proc",
    action: "output",
    permissions: [("proc.output", PermissionOp::Read)],
    struct_name: ProcOutputTool
}

kmc_tool! {
    name: "proc-list",
    domain: "proc",
    action: "list",
    permissions: [("proc.list", PermissionOp::Read)],
    struct_name: ProcListTool
}

kmc_tool! {
    name: "proc-wait",
    domain: "proc",
    action: "wait",
    permissions: [("proc.wait", PermissionOp::Read)],
    struct_name: ProcWaitTool
}

// ---------------------------------------------------------------------------
// Network tools (net.*)
// ---------------------------------------------------------------------------

kmc_tool! {
    name: "net-http",
    domain: "net",
    action: "http",
    permissions: [("net.http", PermissionOp::Execute)],
    struct_name: NetHttpTool
}

kmc_tool! {
    name: "net-dns",
    domain: "net",
    action: "dns",
    permissions: [("net.dns", PermissionOp::Read)],
    struct_name: NetDnsTool
}

// ---------------------------------------------------------------------------
// Build tools (build.*)
// ---------------------------------------------------------------------------

kmc_tool! {
    name: "build-run",
    domain: "build",
    action: "run",
    permissions: [("build.run", PermissionOp::Execute)],
    struct_name: BuildRunTool
}

kmc_tool! {
    name: "build-test",
    domain: "build",
    action: "test",
    permissions: [("build.test", PermissionOp::Execute)],
    struct_name: BuildTestTool
}

kmc_tool! {
    name: "build-lint",
    domain: "build",
    action: "lint",
    permissions: [("build.lint", PermissionOp::Execute)],
    struct_name: BuildLintTool
}

// ---------------------------------------------------------------------------
// All KMC tool names for factory registration
// ---------------------------------------------------------------------------

/// Names of all KMC bridge tools.
pub const KMC_TOOL_NAMES: &[&str] = &[
    "env-create",
    "env-install",
    "env-list",
    "env-destroy",
    "storage-zone-create",
    "storage-zone-list",
    "storage-zone-revoke",
    "proc-spawn",
    "proc-signal",
    "proc-output",
    "proc-list",
    "proc-wait",
    "net-http",
    "net-dns",
    "build-run",
    "build-test",
    "build-lint",
];

/// Build a KMC tool by name.
pub fn build_kmc_tool(name: &str) -> Option<Box<dyn AgentTool>> {
    let tool: Box<dyn AgentTool> = match name {
        "env-create" => Box::new(EnvCreateTool::new()),
        "env-install" => Box::new(EnvInstallTool::new()),
        "env-list" => Box::new(EnvListTool::new()),
        "env-destroy" => Box::new(EnvDestroyTool::new()),
        "storage-zone-create" => Box::new(StorageZoneCreateTool::new()),
        "storage-zone-list" => Box::new(StorageZoneListTool::new()),
        "storage-zone-revoke" => Box::new(StorageZoneRevokeTool::new()),
        "proc-spawn" => Box::new(ProcSpawnTool::new()),
        "proc-signal" => Box::new(ProcSignalTool::new()),
        "proc-output" => Box::new(ProcOutputTool::new()),
        "proc-list" => Box::new(ProcListTool::new()),
        "proc-wait" => Box::new(ProcWaitTool::new()),
        "net-http" => Box::new(NetHttpTool::new()),
        "net-dns" => Box::new(NetDnsTool::new()),
        "build-run" => Box::new(BuildRunTool::new()),
        "build-test" => Box::new(BuildTestTool::new()),
        "build-lint" => Box::new(BuildLintTool::new()),
        _ => return None,
    };
    Some(tool)
}
