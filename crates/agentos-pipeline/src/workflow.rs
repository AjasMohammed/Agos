use agentos_nodes::{NodeExecute, NodeRegistry};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

use crate::definition::{OnFailure, PipelineDefinition, PipelineStep, StepAction};

#[derive(Debug, Error)]
pub enum WorkflowCompileError {
    #[error("unknown node type: {0}")]
    UnknownNodeType(String),
    #[error("node '{0}': {1}")]
    InvalidNode(String, String),
    #[error("cycle detected in workflow graph")]
    CycleDetected,
    #[error("workflow has no end node with an incoming connection")]
    NoOutput,
}

/// Top-level on-wire/on-disk format for visual workflows.
/// Serialized as JSON in `<data_dir>/workflows/<id>.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkflowSpec {
    pub id: String,
    pub name: String,
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub settings: WorkflowSettings,
    pub nodes: Vec<WorkflowNode>,
    #[serde(default)]
    pub connections: Connections,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkflowSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_wall_time_minutes: Option<u64>,
    /// Node id whose output is the workflow result (overrides end-node inference).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowNode {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub node_type: String,
    #[serde(default = "default_type_version")]
    pub type_version: u32,
    pub position: [i32; 2],
    #[serde(default)]
    pub parameters: serde_json::Value,
    #[serde(default)]
    pub credentials: BTreeMap<String, String>,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub notes: Option<String>,
}

/// n8n-style connection map:
///   source_node_id → output_port → output_index → [Connection]
pub type Connections = BTreeMap<String, BTreeMap<String, Vec<Vec<Connection>>>>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Connection {
    pub node: String,
    #[serde(rename = "type", default = "default_port_kind")]
    pub port_type: String,
    #[serde(default)]
    pub index: usize,
}

fn default_version() -> String {
    "1.0.0".into()
}
fn default_type_version() -> u32 {
    1
}
fn default_port_kind() -> String {
    "main".into()
}

impl WorkflowSpec {
    /// Compile this spec into an executable `PipelineDefinition`.
    ///
    /// - Validates that every `node.type` exists in the registry.
    /// - Structural nodes (`start`, `end`) are skipped.
    /// - Computes `depends_on` from the connection map (inverted).
    pub async fn compile_to_pipeline(
        &self,
        registry: &NodeRegistry,
    ) -> Result<PipelineDefinition, WorkflowCompileError> {
        // 1. Validate all node types exist.
        for node in &self.nodes {
            if registry.get(&node.node_type).await.is_none() {
                return Err(WorkflowCompileError::UnknownNodeType(
                    node.node_type.clone(),
                ));
            }
        }

        // 2. Build incoming-edge map: target_id → [source_ids]
        let mut incoming: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (src, outs) in &self.connections {
            for buckets in outs.values() {
                for bucket in buckets {
                    for conn in bucket {
                        incoming
                            .entry(conn.node.clone())
                            .or_default()
                            .push(src.clone());
                    }
                }
            }
        }

        // 3. Identify structural (non-step) node ids.
        let structural: std::collections::HashSet<String> = self
            .nodes
            .iter()
            .filter_map(|n| {
                // We need the registry sync-style — use a blocking check via get cached result.
                // Structural nodes are those whose execute.kind == Builtin { name: "start" | "end" }.
                // We'll check the type name heuristically; runtime validity was checked above.
                if n.node_type == "start" || n.node_type == "end" {
                    Some(n.id.clone())
                } else {
                    None
                }
            })
            .collect();

        // 4. Translate each executable node to a PipelineStep.
        let mut steps = Vec::new();
        let mut output_step_id: Option<String> = None;

        for node in &self.nodes {
            if node.disabled {
                continue;
            }
            let manifest = registry.get(&node.node_type).await.unwrap();

            let depends_on: Vec<String> = incoming
                .get(&node.id)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|src| !structural.contains(src))
                .collect();

            match &manifest.node.execute {
                NodeExecute::Builtin { name } if name == "start" => {
                    // structural — no step
                }
                NodeExecute::Builtin { name } if name == "end" => {
                    // structural — record output source
                    if let Some(src) = incoming.get(&node.id).and_then(|v| v.first()) {
                        if !structural.contains(src) {
                            output_step_id = Some(src.clone());
                        }
                    }
                }
                NodeExecute::Builtin { name } => {
                    // Compile all other builtins as StepAction::Builtin
                    steps.push(PipelineStep {
                        id: node.id.clone(),
                        action: StepAction::Builtin {
                            name: name.clone(),
                            params: node.parameters.clone(),
                        },
                        output_var: Some(format!("{}_output", node.id)),
                        depends_on,
                        timeout_minutes: param_u64(&node.parameters, "timeout_minutes"),
                        retry_on_failure: None,
                        retry_backoff_ms: None,
                        retry_max_delay_ms: None,
                        on_failure: parse_on_failure(node.parameters.get("on_failure")),
                        default_value: node
                            .parameters
                            .get("default_value")
                            .and_then(|v| v.as_str())
                            .map(String::from),
                    });
                }
                NodeExecute::Tool {
                    tool_id,
                    parameter_mapping,
                } => {
                    let input = build_tool_input(node, parameter_mapping);
                    steps.push(PipelineStep {
                        id: node.id.clone(),
                        action: StepAction::Tool {
                            tool: tool_id.clone(),
                            input,
                        },
                        output_var: Some(format!("{}_output", node.id)),
                        depends_on,
                        timeout_minutes: param_u64(&node.parameters, "timeout_minutes"),
                        retry_on_failure: param_u64(&node.parameters, "retry_on_failure")
                            .map(|n| n as u32),
                        retry_backoff_ms: None,
                        retry_max_delay_ms: None,
                        on_failure: parse_on_failure(node.parameters.get("on_failure")),
                        default_value: node
                            .parameters
                            .get("default_value")
                            .and_then(|v| v.as_str())
                            .map(String::from),
                    });
                }
                NodeExecute::Agent {
                    agent_property,
                    task_template,
                } => {
                    let agent = node
                        .parameters
                        .get(agent_property)
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let task = interpolate_template(task_template, &node.parameters);
                    steps.push(PipelineStep {
                        id: node.id.clone(),
                        action: StepAction::Agent { agent, task },
                        output_var: Some(format!("{}_output", node.id)),
                        depends_on,
                        timeout_minutes: param_u64(&node.parameters, "timeout_minutes"),
                        retry_on_failure: None,
                        retry_backoff_ms: None,
                        retry_max_delay_ms: None,
                        on_failure: parse_on_failure(node.parameters.get("on_failure")),
                        default_value: None,
                    });
                }
                NodeExecute::Http {
                    url_template,
                    method,
                    headers,
                    body_template,
                } => {
                    let mut input = serde_json::json!({
                        "url": interpolate_template(url_template, &node.parameters),
                        "method": method,
                        "headers": headers,
                    });
                    if let Some(bt) = body_template {
                        input["body"] =
                            serde_json::Value::String(interpolate_template(bt, &node.parameters));
                    }
                    steps.push(PipelineStep {
                        id: node.id.clone(),
                        action: StepAction::Tool {
                            tool: "http_request".into(),
                            input,
                        },
                        output_var: Some(format!("{}_output", node.id)),
                        depends_on,
                        timeout_minutes: None,
                        retry_on_failure: None,
                        retry_backoff_ms: None,
                        retry_max_delay_ms: None,
                        on_failure: OnFailure::Fail,
                        default_value: None,
                    });
                }
                NodeExecute::CallWorkflow {
                    workflow_id_property,
                } => {
                    let workflow_id = if workflow_id_property.is_empty() {
                        node.parameters
                            .get("workflow_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string()
                    } else {
                        workflow_id_property.clone()
                    };
                    let input = serde_json::json!({
                        "workflow_id": workflow_id,
                        "input": node.parameters.get("input").cloned().unwrap_or(serde_json::Value::Null),
                    });
                    steps.push(PipelineStep {
                        id: node.id.clone(),
                        action: StepAction::Tool {
                            tool: "call_workflow".into(),
                            input,
                        },
                        output_var: Some(format!("{}_output", node.id)),
                        depends_on,
                        timeout_minutes: None,
                        retry_on_failure: None,
                        retry_backoff_ms: None,
                        retry_max_delay_ms: None,
                        on_failure: OnFailure::Fail,
                        default_value: None,
                    });
                }
                NodeExecute::KernelCommand {
                    command,
                    parameter_mapping,
                } => {
                    let input = build_tool_input(node, parameter_mapping);
                    steps.push(PipelineStep {
                        id: node.id.clone(),
                        action: StepAction::Tool {
                            tool: format!("kernel:{}", command),
                            input,
                        },
                        output_var: Some(format!("{}_output", node.id)),
                        depends_on,
                        timeout_minutes: None,
                        retry_on_failure: None,
                        retry_backoff_ms: None,
                        retry_max_delay_ms: None,
                        on_failure: OnFailure::Fail,
                        default_value: None,
                    });
                }
            }
        }

        let output = output_step_id
            .map(|id| format!("{}_output", id))
            .or_else(|| self.settings.output.clone());

        Ok(PipelineDefinition {
            name: self.name.clone(),
            version: self.version.clone(),
            description: Some(self.description.clone()),
            permissions: vec![],
            steps,
            output,
            max_cost_usd: self.settings.max_cost_usd,
            max_wall_time_minutes: self.settings.max_wall_time_minutes,
        })
    }

    /// Convert a legacy `PipelineDefinition` into a `WorkflowSpec` for display.
    /// Assigns synthetic node ids (n0 = start, n1..nN = steps, nEnd = end).
    pub fn from_pipeline_definition(def: &PipelineDefinition) -> WorkflowSpec {
        let mut nodes: Vec<WorkflowNode> = Vec::new();
        let mut connections: Connections = BTreeMap::new();

        // Start node
        nodes.push(WorkflowNode {
            id: "n0".into(),
            name: "Start".into(),
            node_type: "start".into(),
            type_version: 1,
            position: [100, 300],
            parameters: serde_json::Value::Object(Default::default()),
            credentials: BTreeMap::new(),
            disabled: false,
            notes: None,
        });

        let step_count = def.steps.len();
        let mut prev_id = "n0".to_string();

        for (i, step) in def.steps.iter().enumerate() {
            let node_id = format!("n{}", i + 1);
            let x = 400 + (i as i32) * 220;

            let (node_type, parameters) = match &step.action {
                StepAction::Agent { agent, task } => (
                    "agent-task".to_string(),
                    serde_json::json!({
                        "agent_name": agent,
                        "task": task,
                        "timeout_minutes": step.timeout_minutes,
                    }),
                ),
                StepAction::Tool { tool, input } => (format!("tool.{}", tool), input.clone()),
                StepAction::Builtin { name, params } => {
                    (format!("builtin.{}", name), params.clone())
                }
            };

            nodes.push(WorkflowNode {
                id: node_id.clone(),
                name: step.id.clone(),
                node_type,
                type_version: 1,
                position: [x, 300],
                parameters,
                credentials: BTreeMap::new(),
                disabled: false,
                notes: None,
            });

            // Connect prev → this node
            connections
                .entry(prev_id.clone())
                .or_default()
                .entry("main".into())
                .or_default()
                .push(vec![Connection {
                    node: node_id.clone(),
                    port_type: "main".into(),
                    index: 0,
                }]);

            prev_id = node_id;
        }

        let end_id = format!("n{}", step_count + 1);
        nodes.push(WorkflowNode {
            id: end_id.clone(),
            name: "End".into(),
            node_type: "end".into(),
            type_version: 1,
            position: [400 + (step_count as i32) * 220, 300],
            parameters: serde_json::Value::Object(Default::default()),
            credentials: BTreeMap::new(),
            disabled: false,
            notes: None,
        });

        connections
            .entry(prev_id)
            .or_default()
            .entry("main".into())
            .or_default()
            .push(vec![Connection {
                node: end_id,
                port_type: "main".into(),
                index: 0,
            }]);

        WorkflowSpec {
            id: def.name.clone(),
            name: def.name.clone(),
            version: def.version.clone(),
            description: def.description.clone().unwrap_or_default(),
            settings: WorkflowSettings {
                max_cost_usd: def.max_cost_usd,
                max_wall_time_minutes: def.max_wall_time_minutes,
                output: None,
            },
            nodes,
            connections,
        }
    }
}

/// Build a tool input JSON from node parameters, applying the manifest's `parameter_mapping`.
fn build_tool_input(
    node: &WorkflowNode,
    parameter_mapping: &BTreeMap<String, String>,
) -> serde_json::Value {
    let base = if let Some(obj) = node.parameters.as_object() {
        obj.clone()
    } else {
        return node.parameters.clone();
    };

    let mut out = serde_json::Map::new();
    for (k, v) in &base {
        let mapped_key = parameter_mapping.get(k).map(|s| s.as_str()).unwrap_or(k);
        out.insert(mapped_key.to_string(), v.clone());
    }
    // Also include any fixed values from mapping (values starting with __fixed__:)
    for (target, source) in parameter_mapping {
        if let Some(fixed) = source.strip_prefix("__fixed__:") {
            out.insert(target.clone(), serde_json::Value::String(fixed.to_string()));
        }
    }
    serde_json::Value::Object(out)
}

/// Substitute `{{param_name}}` placeholders from node parameters.
fn interpolate_template(template: &str, params: &serde_json::Value) -> String {
    let mut result = template.to_string();
    if let Some(obj) = params.as_object() {
        for (k, v) in obj {
            let placeholder = format!("{{{{{}}}}}", k);
            let value = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            result = result.replace(&placeholder, &value);
        }
    }
    result
}

fn parse_on_failure(v: Option<&serde_json::Value>) -> OnFailure {
    match v.and_then(|v| v.as_str()) {
        Some("skip") => OnFailure::Skip,
        Some("use_default") => OnFailure::UseDefault,
        _ => OnFailure::Fail,
    }
}

fn param_u64(params: &serde_json::Value, key: &str) -> Option<u64> {
    params.get(key).and_then(|v| v.as_u64())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_nodes::{NodeManifest, NodeManifestBody, NodeRegistry};

    fn start_manifest() -> NodeManifest {
        NodeManifest {
            node: NodeManifestBody {
                id: "start".into(),
                display_name: "Start".into(),
                execute: agentos_nodes::NodeExecute::Builtin {
                    name: "start".into(),
                },
                ..Default::default()
            },
        }
    }

    fn end_manifest() -> NodeManifest {
        NodeManifest {
            node: NodeManifestBody {
                id: "end".into(),
                display_name: "End".into(),
                execute: agentos_nodes::NodeExecute::Builtin { name: "end".into() },
                ..Default::default()
            },
        }
    }

    fn agent_task_manifest() -> NodeManifest {
        NodeManifest {
            node: NodeManifestBody {
                id: "agent-task".into(),
                display_name: "Agent Task".into(),
                execute: agentos_nodes::NodeExecute::Agent {
                    agent_property: "agent_name".into(),
                    task_template: "{{task}}".into(),
                },
                ..Default::default()
            },
        }
    }

    async fn make_registry() -> NodeRegistry {
        let r = NodeRegistry::new();
        for m in [start_manifest(), end_manifest(), agent_task_manifest()] {
            r.register_static(m).await;
        }
        r
    }

    fn make_workflow(node_type: &str, agent: &str, task: &str) -> WorkflowSpec {
        let mut connections: Connections = BTreeMap::new();
        connections
            .entry("n0".into())
            .or_default()
            .entry("main".into())
            .or_default()
            .push(vec![Connection {
                node: "n1".into(),
                port_type: "main".into(),
                index: 0,
            }]);
        connections
            .entry("n1".into())
            .or_default()
            .entry("main".into())
            .or_default()
            .push(vec![Connection {
                node: "n2".into(),
                port_type: "main".into(),
                index: 0,
            }]);

        WorkflowSpec {
            id: "test".into(),
            name: "Test Workflow".into(),
            version: "1.0.0".into(),
            description: "".into(),
            settings: WorkflowSettings::default(),
            nodes: vec![
                WorkflowNode {
                    id: "n0".into(),
                    name: "Start".into(),
                    node_type: "start".into(),
                    type_version: 1,
                    position: [0, 0],
                    parameters: serde_json::Value::Null,
                    credentials: BTreeMap::new(),
                    disabled: false,
                    notes: None,
                },
                WorkflowNode {
                    id: "n1".into(),
                    name: "MyAgent".into(),
                    node_type: node_type.to_string(),
                    type_version: 1,
                    position: [300, 0],
                    parameters: serde_json::json!({
                        "agent_name": agent,
                        "task": task,
                        "timeout_minutes": 5,
                    }),
                    credentials: BTreeMap::new(),
                    disabled: false,
                    notes: None,
                },
                WorkflowNode {
                    id: "n2".into(),
                    name: "End".into(),
                    node_type: "end".into(),
                    type_version: 1,
                    position: [600, 0],
                    parameters: serde_json::Value::Null,
                    credentials: BTreeMap::new(),
                    disabled: false,
                    notes: None,
                },
            ],
            connections,
        }
    }

    #[tokio::test]
    async fn test_compile_start_agent_end() {
        let registry = make_registry().await;
        let spec = make_workflow("agent-task", "researcher", "Summarize {{input}}");
        let def = spec.compile_to_pipeline(&registry).await.unwrap();

        assert_eq!(def.steps.len(), 1);
        assert_eq!(def.steps[0].id, "n1");
        assert_eq!(def.steps[0].depends_on, Vec::<String>::new());
        assert_eq!(def.output, Some("n1_output".into()));

        match &def.steps[0].action {
            StepAction::Agent { agent, task } => {
                assert_eq!(agent, "researcher");
                assert_eq!(task, "Summarize {{input}}");
            }
            _ => panic!("expected Agent action"),
        }
    }

    #[tokio::test]
    async fn test_unknown_node_type() {
        let registry = make_registry().await;
        let mut spec = make_workflow("agent-task", "r", "t");
        spec.nodes[1].node_type = "nonexistent.node".into();
        let result = spec.compile_to_pipeline(&registry).await;
        assert!(matches!(
            result,
            Err(WorkflowCompileError::UnknownNodeType(_))
        ));
    }

    #[tokio::test]
    async fn test_from_pipeline_definition_round_trip() {
        let def = PipelineDefinition {
            name: "my-pipeline".into(),
            version: "1.0.0".into(),
            description: Some("desc".into()),
            permissions: vec![],
            steps: vec![PipelineStep {
                id: "step1".into(),
                action: StepAction::Agent {
                    agent: "agent1".into(),
                    task: "do something".into(),
                },
                output_var: Some("step1_output".into()),
                depends_on: vec![],
                timeout_minutes: None,
                retry_on_failure: None,
                retry_backoff_ms: None,
                retry_max_delay_ms: None,
                on_failure: OnFailure::Fail,
                default_value: None,
            }],
            output: Some("step1_output".into()),
            max_cost_usd: None,
            max_wall_time_minutes: None,
        };

        let spec = WorkflowSpec::from_pipeline_definition(&def);
        assert_eq!(spec.name, "my-pipeline");
        // start + step + end = 3 nodes
        assert_eq!(spec.nodes.len(), 3);
        assert_eq!(spec.nodes[1].node_type, "agent-task");
    }
}
